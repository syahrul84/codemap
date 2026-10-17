//! What a controller action accepts from the request: the rules of its Form
//! Request or of an inline `validate([...])`, and the fields it reads
//! (`$request->input('x')`, `$request->x`, `request('x')`, `->only([...])`).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use codemap_core::symbols::{self, MemberLookup};
use codemap_php::ast::{find_class, named_children, text};
use rusqlite::Connection;
use serde::Serialize;
use tree_sitter::Node;

use codemap_php::Scope;

use crate::literal::{self, Lit};
use crate::requests;
use crate::responses::{method, parse_class};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Accepted {
    /// `App\Http\Requests\StoreOrder rules()`, `validate()` or `request reads`.
    pub source: String,
    /// Where the rules (or the action) are.
    pub at: Option<String>,
    /// Top-level field → required.
    pub fields: BTreeMap<String, bool>,
    /// Every field the action takes is listed: no `$request->all()`, no
    /// request handed to other code, rules fully literal.
    pub complete: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// Request methods that read one field by name.
const READS: &[&str] = &[
    "input", "get", "post", "query", "boolean", "string", "str", "integer", "float", "date",
    "enum", "file", "has", "filled", "missing", "exists", "hasFile", "array", "collect",
];
/// Request methods that take everything (or everything but some).
const OPEN: &[&str] = &[
    "all", "except", "json", "keys", "merge", "replace", "toArray",
];
/// `$request->x` properties that are not fields.
const NOT_FIELDS: &[&str] = &[
    "query",
    "request",
    "headers",
    "files",
    "cookies",
    "server",
    "attributes",
    "json",
];

fn top(field: &str) -> &str {
    field.split('.').next().unwrap_or(field)
}

/// Rules → fields (top level; a field is required when its own rules say
/// `required` without `sometimes`).
fn add_rules(items: &[(Option<String>, Lit)], out: &mut Accepted) {
    for (key, value) in items {
        let Some(field) = key else {
            out.complete = false;
            out.notes.push("a rule has a non-literal key".into());
            continue;
        };
        let rules: Vec<String> = match value {
            Lit::Str(s) => s.split('|').map(|r| r.trim().to_owned()).collect(),
            Lit::Arr(a) => a
                .iter()
                .filter_map(|(_, r)| r.as_str().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        };
        let required = !field.contains('.')
            && rules.iter().any(|r| r == "required")
            && !rules.iter().any(|r| r == "sometimes");
        let e = out.fields.entry(top(field).to_owned()).or_insert(false);
        *e |= required;
    }
}

fn args<'t>(call: Node<'t>) -> Vec<Node<'t>> {
    call.child_by_field_name("arguments")
        .map(named_children)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|a| {
            if a.kind() == "argument" {
                named_children(a).into_iter().last()
            } else {
                Some(a)
            }
        })
        .collect()
}

/// `field => rules` pairs, as `literal::eval` returns them.
type Rules = Vec<(Option<String>, Lit)>;

struct Walk<'s> {
    src: &'s [u8],
    scope: &'s Scope,
    vars: Vec<String>,
    reads: Vec<String>,
    /// The first `validate()` line and every rule found.
    rules: Option<(u32, Rules)>,
    open: Vec<String>,
    /// `$this->helper(…)` calls: their reads count too.
    /// With the line of the call.
    helpers: Vec<(String, u32)>,
    /// Calls `paginate()` / `simplePaginate()` / `cursorPaginate()`, which
    /// read `page` / `cursor` themselves.
    paginates: bool,
}

impl Walk<'_> {
    fn is_request(&self, n: Node<'_>) -> bool {
        match n.kind() {
            "variable_name" => {
                let t = text(n, self.src).trim_start_matches('$');
                self.vars.iter().any(|v| v == t)
            }
            // `request()`.
            "function_call_expression" => {
                n.child_by_field_name("function")
                    .is_some_and(|f| text(f, self.src) == "request")
                    && args(n).is_empty()
            }
            _ => false,
        }
    }

    fn str_arg(&self, n: Option<Node<'_>>) -> Option<String> {
        match literal::eval(n?, self.src, self.scope) {
            Lit::Str(s) => Some(s),
            _ => None,
        }
    }

    fn visit(&mut self, n: Node<'_>) {
        match n.kind() {
            // Closures are other code; arrow functions see the request too.
            "anonymous_function" => {}
            "member_call_expression" | "nullsafe_member_call_expression" => {
                let obj = n.child_by_field_name("object");
                let name = n
                    .child_by_field_name("name")
                    .map(|x| text(x, self.src))
                    .unwrap_or("");
                let a = args(n);
                let line = n.start_position().row as u32 + 1;
                if name.ends_with("aginate") {
                    self.paginates = true;
                }
                if obj.is_some_and(|o| self.is_request(o)) {
                    if READS.contains(&name) {
                        match self.str_arg(a.first().copied()) {
                            Some(f) => self.reads.push(top(&f).to_owned()),
                            None if a.is_empty()
                                && matches!(name, "input" | "collect" | "query" | "post") =>
                            {
                                self.open.push(format!("$request->{name}() at line {line}"))
                            }
                            None => self.open.push(format!(
                                "$request->{name}(…) with a computed name at line {line}"
                            )),
                        }
                    } else if name == "only" {
                        for x in &a {
                            match literal::eval(*x, self.src, self.scope) {
                                Lit::Str(s) => self.reads.push(top(&s).to_owned()),
                                Lit::Arr(items) => self.reads.extend(
                                    items
                                        .iter()
                                        .filter_map(|(_, v)| v.as_str())
                                        .map(|s| top(s).to_owned()),
                                ),
                                _ => self.open.push(format!(
                                    "$request->only(…) with computed names at line {line}"
                                )),
                            }
                        }
                    } else if name == "validate" || name == "validateWithBag" {
                        let rules = if name == "validate" {
                            a.first()
                        } else {
                            a.get(1)
                        };
                        self.take_rules(rules.copied(), line);
                    } else if OPEN.contains(&name) {
                        self.open.push(format!("$request->{name}() at line {line}"));
                    }
                } else if name == "validate"
                    && obj.is_some_and(|o| text(o, self.src) == "$this")
                    && a.first().is_some_and(|x| self.is_request(*x))
                {
                    self.take_rules(a.get(1).copied(), line);
                } else if obj.is_some_and(|o| text(o, self.src) == "$this") {
                    // A helper of the controller: followed, not a hand-off.
                    if !self.helpers.iter().any(|(h, _)| h == name) {
                        self.helpers.push((name.to_owned(), line));
                    }
                    for c in named_children(n) {
                        self.visit(c);
                    }
                    return;
                } else if obj.is_some_and(|o| {
                    o.kind() == "member_access_expression"
                        && o.child_by_field_name("object")
                            .is_some_and(|x| self.is_request(x))
                }) && name == "get"
                {
                    // `$request->query->get('x')`.
                    if let Some(f) = self.str_arg(a.first().copied()) {
                        self.reads.push(top(&f).to_owned());
                    }
                }
                self.hand_off(&a, line);
            }
            "scoped_call_expression" => {
                let name = n.child_by_field_name("name").map(|x| text(x, self.src));
                let line = n.start_position().row as u32 + 1;
                let a = args(n);
                let scope = n
                    .child_by_field_name("scope")
                    .map(|x| text(x, self.src))
                    .unwrap_or("");
                if name == Some("make") && scope.ends_with("Validator") {
                    self.take_rules(a.get(1).copied(), line);
                } else {
                    self.hand_off(&a, line);
                }
            }
            "function_call_expression" => {
                let f = n.child_by_field_name("function").map(|x| text(x, self.src));
                let a = args(n);
                let line = n.start_position().row as u32 + 1;
                if f == Some("request") && !a.is_empty() {
                    match self.str_arg(a.first().copied()) {
                        Some(x) => self.reads.push(top(&x).to_owned()),
                        None => self
                            .open
                            .push(format!("request(…) with a computed name at line {line}")),
                    }
                } else {
                    self.hand_off(&a, line);
                }
            }
            "object_creation_expression" => {
                let a: Vec<Node<'_>> = named_children(n)
                    .into_iter()
                    .filter(|c| c.kind() == "arguments")
                    .flat_map(named_children)
                    .filter_map(|x| named_children(x).into_iter().last())
                    .collect();
                self.hand_off(&a, n.start_position().row as u32 + 1);
            }
            // `$request['x']`.
            "subscript_expression" => {
                let kids = named_children(n);
                if kids.first().is_some_and(|o| self.is_request(*o)) {
                    match self.str_arg(kids.get(1).copied()) {
                        Some(f) => self.reads.push(top(&f).to_owned()),
                        None => self.open.push(format!(
                            "$request[…] with a computed name at line {}",
                            n.start_position().row + 1
                        )),
                    }
                }
            }
            "member_access_expression" | "nullsafe_member_access_expression" => {
                if n.child_by_field_name("object")
                    .is_some_and(|o| self.is_request(o))
                    && let Some(name) = n.child_by_field_name("name").map(|x| text(x, self.src))
                    && !NOT_FIELDS.contains(&name)
                {
                    self.reads.push(name.to_owned());
                }
            }
            _ => {}
        }
        for c in named_children(n) {
            self.visit(c);
        }
    }

    /// The request handed to other code (a service, a job, `fill($request)`):
    /// it may read any field.
    fn hand_off(&mut self, a: &[Node<'_>], line: u32) {
        if a.iter().any(|x| self.is_request(*x)) {
            self.open.push(format!(
                "the request is passed to other code at line {line}"
            ));
        }
    }

    fn take_rules(&mut self, n: Option<Node<'_>>, line: u32) {
        match n.map(|n| literal::eval(n, self.src, self.scope)) {
            Some(Lit::Arr(items)) => {
                let mut all = self.rules.take().map(|(_, r)| r).unwrap_or_default();
                all.extend(items);
                self.rules = Some((line, all));
            }
            _ => self.open.push(format!(
                "validation rules at line {line} are not a literal array"
            )),
        }
    }
}

/// What `controller::action` accepts; `None` when the action is not in project code.
/// Helper methods already walked while answering one question:
/// `(controller, helper, request variables)` → what the helper reads.
#[derive(Default)]
pub struct AcceptsCache(
    HashMap<(String, String, String), Option<HelperWalk>>,
    /// Classes parsed for this answer (a controller serves many actions).
    HashMap<String, Option<std::sync::Arc<crate::responses::Parsed>>>,
);

impl AcceptsCache {
    fn class(
        &mut self,
        conn: &Connection,
        root: &Path,
        fqn: &str,
    ) -> Option<std::sync::Arc<crate::responses::Parsed>> {
        self.1
            .entry(fqn.to_owned())
            .or_insert_with(|| parse_class(conn, root, fqn))
            .clone()
    }
}

/// What one controller helper reads from the request.
pub struct HelperWalk {
    file: String,
    reads: Vec<String>,
    open: Vec<String>,
    helpers: Vec<(String, u32)>,
    paginates: bool,
    rules: Option<(u32, Rules)>,
}

fn walk_helper(
    conn: &Connection,
    root: &Path,
    controller: &str,
    h: &str,
    vars: &[String],
    classes: &mut AcceptsCache,
) -> Option<HelperWalk> {
    let Ok(MemberLookup::Found(hm)) = symbols::find_member(conn, controller, h) else {
        return None;
    };
    let owner = hm
        .parent_fqn
        .clone()
        .unwrap_or_else(|| controller.to_owned());
    let hp = classes.class(conn, root, &owner)?;
    let (hc, hscope) = find_class(hp.tree.root_node(), &hp.src, &owner)?;
    let hbody = method(hc, &hp.src, h).and_then(|m| m.child_by_field_name("body"))?;
    let mut hv: Vec<String> = hm
        .meta
        .params
        .iter()
        .filter(|p| {
            p.type_fqns.iter().any(|t| t.ends_with("Request"))
                || p.name.trim_start_matches('$') == "request"
        })
        .map(|p| p.name.trim_start_matches('$').to_owned())
        .collect();
    hv.extend(vars.iter().cloned());
    let mut hw = Walk {
        src: &hp.src,
        scope: &hscope,
        vars: hv,
        reads: Vec::new(),
        rules: None,
        open: Vec::new(),
        helpers: Vec::new(),
        paginates: false,
    };
    hw.visit(hbody);
    Some(HelperWalk {
        file: hp.file.clone(),
        reads: hw.reads,
        open: hw.open,
        helpers: hw.helpers,
        paginates: hw.paginates,
        rules: hw.rules,
    })
}

/// [`for_action_with`] without a shared cache.
pub fn for_action(
    conn: &Connection,
    root: &Path,
    controller: &str,
    action: &str,
) -> Option<Accepted> {
    for_action_with(conn, root, controller, action, &mut AcceptsCache::default())
}

/// What `controller::action` accepts; helpers already walked for other
/// actions come from `cache`.
pub fn for_action_with(
    conn: &Connection,
    root: &Path,
    controller: &str,
    action: &str,
    cache: &mut AcceptsCache,
) -> Option<Accepted> {
    let MemberLookup::Found(m) = symbols::find_member(conn, controller, action).ok()? else {
        return None;
    };
    let mut out = Accepted {
        complete: true,
        ..Accepted::default()
    };
    let mut vars = Vec::new();
    let mut form = None;
    for p in &m.meta.params {
        let name = p.name.trim_start_matches('$').to_owned();
        for t in &p.type_fqns {
            if t == "Illuminate\\Http\\Request" || t.ends_with("\\Request") {
                vars.push(name.clone());
            }
            if let Some(r) = requests::load(conn, Some(t))
                .ok()
                .and_then(|r| r.into_iter().next())
            {
                vars.push(name.clone());
                form = Some(r);
            }
        }
    }
    if let Some(r) = &form {
        out.source = format!("{} rules()", r.fqn);
        out.at = Some(format!("{}:{}", r.file, r.line));
        if let Some(why) = &r.unresolved_reason {
            out.complete = false;
            out.notes.push(why.clone());
        }
        let items: Rules = r
            .rules
            .iter()
            .map(|f| {
                (
                    Some(f.field.clone()),
                    Lit::Arr(
                        f.rules
                            .iter()
                            .map(|x| (None, Lit::Str(x.clone())))
                            .collect(),
                    ),
                )
            })
            .collect();
        add_rules(&items, &mut out);
    }
    let parsed = cache.class(
        conn,
        root,
        &m.parent_fqn
            .clone()
            .unwrap_or_else(|| controller.to_owned()),
    )?;
    let (class, scope) = find_class(
        parsed.tree.root_node(),
        &parsed.src,
        m.parent_fqn.as_deref().unwrap_or(controller),
    )?;
    let body = method(class, &parsed.src, action)?.child_by_field_name("body")?;
    let mut w = Walk {
        src: &parsed.src,
        scope: &scope,
        vars: vars.clone(),
        reads: Vec::new(),
        rules: None,
        open: Vec::new(),
        helpers: Vec::new(),
        paginates: false,
    };
    w.visit(body);
    let mut rules_at = w
        .rules
        .as_ref()
        .map(|(l, _)| format!("{}:{l}", parsed.file));
    // The controller's own helpers (here, in a parent or a trait), two levels.
    let mut done: Vec<String> = vec![action.to_owned()];
    for _ in 0..2 {
        let todo: Vec<(String, u32)> = std::mem::take(&mut w.helpers)
            .into_iter()
            .filter(|(h, _)| !done.contains(h))
            .collect();
        for (h, called_at) in todo {
            done.push(h.clone());
            let key = (controller.to_owned(), h.clone(), vars.join(","));
            if !cache.0.contains_key(&key) {
                let walked = walk_helper(conn, root, controller, &h, &vars, cache);
                cache.0.insert(key.clone(), walked);
            }
            let Some(hw) = cache.0.get(&key).and_then(Option::as_ref) else {
                continue;
            };
            w.reads.extend(hw.reads.iter().cloned());
            w.open.extend(hw.open.iter().cloned());
            w.helpers.extend(hw.helpers.iter().cloned());
            w.paginates |= hw.paginates;
            if let Some((hline, r)) = &hw.rules {
                if rules_at.is_none() {
                    rules_at = Some(format!(
                        "{}:{hline} (in {h}(), called at line {called_at})",
                        hw.file
                    ));
                }
                let line = w.rules.as_ref().map_or(*hline, |(l, _)| *l);
                let mut all = w.rules.take().map(|(_, x)| x).unwrap_or_default();
                all.extend(r.iter().cloned());
                w.rules = Some((line, all));
            }
        }
    }
    if let Some((_, rules)) = &w.rules {
        if form.is_none() {
            out.source = "validate()".into();
            out.at = rules_at.clone().or_else(|| Some(parsed.file.clone()));
        }
        add_rules(rules, &mut out);
    }
    if form.is_none() && w.rules.is_none() {
        if w.reads.is_empty() {
            // Nothing visible: the action takes no input, or takes it elsewhere.
            return None;
        }
        out.source = "request reads".into();
        out.at = Some(format!("{}:{}", parsed.file, body.start_position().row + 1));
    }
    for r in w.reads {
        out.fields.entry(r).or_insert(false);
    }
    if !w.open.is_empty() {
        out.complete = false;
        out.notes.extend(w.open);
    }
    // Read by Laravel's paginator itself.
    if w.paginates {
        for f in ["page", "cursor"] {
            out.fields.entry(f.to_owned()).or_insert(false);
        }
    }
    Some(out)
}
