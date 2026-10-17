//! What a controller action returns, read statically on demand: API Resources
//! (`toArray` keys, `when*` / `merge*` marked conditional), Resource
//! collections, literal arrays and `response()->json([...])`. Anything built
//! from queries is reported by kind only; fields are never guessed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use codemap_core::symbols;
use codemap_php::Scope;
use codemap_php::ast::{find_class, named_children, text};
use rusqlite::Connection;
use serde::Serialize;
use tree_sitter::Node;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ShapeField {
    pub name: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub conditional: bool,
    /// Resource class of a nested value (`'items' => ItemResource::collection(...)`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    /// Keys one level down, when the value is a literal array or a Resource
    /// whose fields are all known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ResponseShape {
    /// `resource`, `collection`, `array`, `json`, `unknown`.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    /// Top-level fields of the payload (inside `data` for resources).
    pub fields: Vec<ShapeField>,
    /// Resources wrap their payload in `data` (collections add `links`/`meta`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub wrapped_in_data: bool,
    /// Every field is known (no `parent::toArray`, spreads or unknown returns).
    pub complete: bool,
    /// Keys added next to `data` with `->additional([...])` (e.g. `summary`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub additional: Vec<ShapeField>,
    /// `file:line` of the return that was read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub returned_at: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

pub(crate) struct Parsed {
    pub(crate) src: Vec<u8>,
    pub(crate) tree: tree_sitter::Tree,
    pub(crate) file: String,
}

/// Parsed controller / resource files, reused while unchanged: one controller
/// serves many routes, and a large frontend asks about hundreds of them.
type ParseCache = HashMap<PathBuf, (Option<SystemTime>, Arc<Parsed>)>;
static PARSED: Mutex<Option<ParseCache>> = Mutex::new(None);

pub(crate) fn parse_class(conn: &Connection, root: &Path, fqn: &str) -> Option<Arc<Parsed>> {
    let sym = symbols::by_fqn(conn, fqn)
        .ok()?
        .into_iter()
        .find(|s| s.kind.is_class_like())?;
    parse_file(root, sym.file)
}

/// The parsed file, from the cache while it is unchanged.
fn parse_file(root: &Path, file: String) -> Option<Arc<Parsed>> {
    let path = root.join(&file);
    let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    if let Ok(guard) = PARSED.lock()
        && let Some((t, p)) = guard.as_ref().and_then(|c| c.get(&path))
        && *t == mtime
        && mtime.is_some()
    {
        return Some(Arc::clone(p));
    }
    let src = std::fs::read(&path).ok()?;
    let tree = codemap_php::parse::parse(&src)?;
    let parsed = Arc::new(Parsed { src, tree, file });
    if let Ok(mut guard) = PARSED.lock() {
        let cache = guard.get_or_insert_with(HashMap::new);
        if cache.len() > 2000 {
            cache.clear();
        }
        cache.insert(path, (mtime, Arc::clone(&parsed)));
    }
    Some(parsed)
}

/// Parses the files of these classes on all cores, ahead of answers that
/// read them one by one (the controllers behind a backend's routes).
pub fn prefetch(conn: &Connection, root: &Path, classes: &[&str]) {
    use rayon::prelude::*;
    let mut files: Vec<String> = classes
        .iter()
        .filter_map(|fqn| {
            symbols::by_fqn(conn, fqn)
                .ok()?
                .into_iter()
                .find(|s| s.kind.is_class_like())
                .map(|s| s.file)
        })
        .collect();
    files.sort();
    files.dedup();
    files.truncate(1500);
    files.into_par_iter().for_each(|f| {
        parse_file(root, f);
    });
}

pub(crate) fn method<'t>(class: Node<'t>, src: &[u8], name: &str) -> Option<Node<'t>> {
    class
        .child_by_field_name("body")
        .map(named_children)
        .unwrap_or_default()
        .into_iter()
        .find(|m| {
            m.kind() == "method_declaration"
                && m.child_by_field_name("name").map(|n| text(n, src)) == Some(name)
        })
}

/// Return expressions of a body, not looking into closures.
fn returns<'t>(node: Node<'t>, out: &mut Vec<Node<'t>>) {
    for c in named_children(node) {
        match c.kind() {
            "anonymous_function" | "arrow_function" | "anonymous_function_creation_expression" => {}
            "return_statement" => {
                if let Some(e) = named_children(c).into_iter().next() {
                    out.push(e);
                }
            }
            _ => returns(c, out),
        }
    }
}

fn unwrap_parens(mut n: Node<'_>) -> Node<'_> {
    while n.kind() == "parenthesized_expression" {
        match named_children(n).into_iter().next() {
            Some(i) => n = i,
            None => break,
        }
    }
    n
}

fn args(n: Node<'_>) -> Vec<Node<'_>> {
    n.child_by_field_name("arguments")
        .map(named_children)
        .unwrap_or_default()
        .into_iter()
        .filter(|a| a.kind() == "argument")
        .filter_map(|a| named_children(a).into_iter().last())
        .collect()
}

fn is_resource(conn: &Connection, fqn: &str) -> bool {
    if fqn.ends_with("Resource") || fqn.ends_with("Collection") {
        return true;
    }
    symbols::by_fqn(conn, fqn)
        .ok()
        .and_then(|v| v.into_iter().next())
        .is_some_and(|s| s.meta.extends.iter().any(|e| e.contains("Resource")))
}

pub fn for_action(conn: &Connection, root: &Path, controller: &str, action: &str) -> ResponseShape {
    let mut shape = ResponseShape {
        kind: "unknown".into(),
        ..ResponseShape::default()
    };
    let Some(p) = parse_class(conn, root, controller) else {
        shape
            .notes
            .push(format!("{controller} not found in project code"));
        return shape;
    };
    let Some((class, scope)) = find_class(p.tree.root_node(), &p.src, controller) else {
        return shape;
    };
    let Some(m) = method(class, &p.src, action) else {
        shape.notes.push(format!(
            "{controller}::{action} not found (may be inherited)"
        ));
        return shape;
    };
    let Some(body) = m.child_by_field_name("body") else {
        return shape;
    };
    let mut rets = Vec::new();
    returns(body, &mut rets);
    if rets.is_empty() {
        shape
            .notes
            .push("the action has no return statement (e.g. noContent / void)".into());
        return shape;
    }
    // The first return that tells us something wins; others are noted.
    for r in &rets {
        let s = expr_shape(conn, root, &p, &scope, class, body, *r, 0);
        if s.kind != "unknown" {
            let mut s = s;
            if rets.len() > 1 {
                s.notes.push(format!(
                    "{} return statements; read the one at line {}",
                    rets.len(),
                    r.start_position().row + 1
                ));
                s.complete = false;
            }
            return s;
        }
    }
    shape.returned_at = Some(format!("{}:{}", p.file, rets[0].start_position().row + 1));
    shape.notes.push(format!(
        "returns `{}`, which is not a Resource or literal array",
        codemap_php::ast::truncate(&codemap_php::ast::compact(text(rets[0], &p.src)), 80)
    ));
    shape
}

#[allow(clippy::too_many_arguments)]
fn expr_shape(
    conn: &Connection,
    root: &Path,
    p: &Parsed,
    scope: &Scope,
    class: Node<'_>,
    body: Node<'_>,
    e: Node<'_>,
    depth: u8,
) -> ResponseShape {
    let e = unwrap_parens(e);
    let at = format!("{}:{}", p.file, e.start_position().row + 1);
    let mut shape = ResponseShape {
        kind: "unknown".into(),
        returned_at: Some(at),
        ..ResponseShape::default()
    };
    if depth > 4 {
        return shape;
    }
    match e.kind() {
        "object_creation_expression" => {
            let class = named_children(e)
                .into_iter()
                .find(|c| matches!(c.kind(), "name" | "qualified_name" | "relative_name"))
                .and_then(|n| scope.resolve_class(text(n, &p.src)));
            if let Some(class) = class.filter(|c| is_resource(conn, c)) {
                let mut s = resource_shape(conn, root, &class);
                s.kind = if class.ends_with("Collection") {
                    "collection".into()
                } else {
                    "resource".into()
                };
                s.returned_at = shape.returned_at;
                return s;
            }
        }
        "scoped_call_expression" => {
            let class = e
                .child_by_field_name("scope")
                .and_then(|n| scope.resolve_class(text(n, &p.src)));
            let name = e
                .child_by_field_name("name")
                .map(|n| text(n, &p.src))
                .unwrap_or("");
            if let Some(class) = class.filter(|c| is_resource(conn, c))
                && matches!(name, "collection" | "make")
            {
                let mut s = resource_shape(conn, root, &class);
                s.kind = if name == "collection" {
                    "collection".into()
                } else {
                    "resource".into()
                };
                s.returned_at = shape.returned_at;
                return s;
            }
        }
        "array_creation_expression" => {
            let (mut fields, complete) = array_fields(conn, root, scope, &p.src, e, 0);
            // `'data' => array_values($cards)`: keys of each item, when every
            // step that builds the list is readable.
            let items = Items {
                conn,
                root,
                src: &p.src,
                scope,
                class,
                budget: std::cell::Cell::new(2000),
            };
            for el in named_children(e) {
                let parts = named_children(el);
                let [key, value] = parts.as_slice() else {
                    continue;
                };
                let Some(name) = crate::literal::eval(*key, &p.src, scope)
                    .as_str()
                    .map(str::to_owned)
                else {
                    continue;
                };
                if let Some(f) = fields
                    .iter_mut()
                    .find(|f| f.name == name && f.children.is_none() && f.resource.is_none())
                    && let Some(keys) = items.list(body, *value, None, 0)
                {
                    f.children = Some(keys);
                }
            }
            shape.kind = "array".into();
            shape.fields = fields;
            shape.complete = complete;
            return shape;
        }
        "member_call_expression" => {
            // response()->json([...]) / response()->json(new XResource(...), 201)
            let name = e
                .child_by_field_name("name")
                .map(|n| text(n, &p.src))
                .unwrap_or("");
            let obj = e
                .child_by_field_name("object")
                .map(|n| text(n, &p.src))
                .unwrap_or("");
            if name == "json" && obj.replace(' ', "").starts_with("response(") {
                match args(e).first() {
                    Some(a) => {
                        let mut s = expr_shape(conn, root, p, scope, class, body, *a, depth + 1);
                        if s.kind == "array" {
                            s.kind = "json".into();
                        }
                        if s.kind == "unknown" {
                            s.kind = "json".into();
                            s.notes.push(format!(
                                "json payload `{}` is not a literal",
                                codemap_php::ast::truncate(
                                    &codemap_php::ast::compact(text(*a, &p.src)),
                                    60
                                )
                            ));
                        }
                        return s;
                    }
                    None => {
                        shape.kind = "json".into();
                        shape.complete = true;
                        return shape;
                    }
                }
            }
            // $resource->response()->setStatusCode(...), (new X)->additional([...])
            if let Some(o) = e.child_by_field_name("object")
                && matches!(
                    name,
                    "response" | "setStatusCode" | "additional" | "header" | "withHeaders"
                )
            {
                let mut s = expr_shape(conn, root, p, scope, class, body, o, depth + 1);
                if name == "additional"
                    && let Some(arr) = args(e).first().map(|a| unwrap_parens(*a))
                    && arr.kind() == "array_creation_expression"
                {
                    let (fields, complete) = array_fields(conn, root, scope, &p.src, arr, 0);
                    s.complete &= complete;
                    s.additional.extend(fields);
                }
                return s;
            }
        }
        "variable_name" => {
            // `return $x;` → the last `$x = …` in the body.
            let var = text(e, &p.src).to_owned();
            if let Some(rhs) = last_assignment(body, &p.src, &var) {
                return expr_shape(conn, root, p, scope, class, body, rhs, depth + 1);
            }
        }
        _ => {}
    }
    shape
}

fn last_assignment<'t>(node: Node<'t>, src: &[u8], var: &str) -> Option<Node<'t>> {
    let mut found = None;
    for c in named_children(node) {
        if matches!(c.kind(), "anonymous_function" | "arrow_function") {
            continue;
        }
        if c.kind() == "assignment_expression"
            && c.child_by_field_name("left").map(|l| text(l, src)) == Some(var)
        {
            found = c.child_by_field_name("right");
        }
        if let Some(inner) = last_assignment(c, src, var) {
            found = Some(inner);
        }
    }
    found
}

/// Keys of an array literal; `complete` is false when a key or spread is unknown.
fn array_fields(
    conn: &Connection,
    root: &Path,
    scope: &Scope,
    src: &[u8],
    arr: Node<'_>,
    depth: u8,
) -> (Vec<ShapeField>, bool) {
    let mut fields = Vec::new();
    let mut complete = true;
    for el in named_children(arr) {
        if el.kind() != "array_element_initializer" {
            continue;
        }
        let parts = named_children(el);
        match parts.as_slice() {
            [key, value] => {
                let k = crate::literal::eval(*key, src, scope);
                match k.as_str() {
                    Some(name) => {
                        let resource = nested_resource(conn, scope, src, *value);
                        let children =
                            children_of(conn, root, scope, src, *value, resource.as_deref(), depth);
                        fields.push(ShapeField {
                            name: name.to_owned(),
                            conditional: false,
                            resource,
                            children,
                        });
                    }
                    None => complete = false,
                }
            }
            _ => complete = false,
        }
    }
    (fields, complete)
}

/// Child keys of a literal array value or a fully known Resource.
fn children_of(
    conn: &Connection,
    root: &Path,
    scope: &Scope,
    src: &[u8],
    v: Node<'_>,
    resource: Option<&str>,
    depth: u8,
) -> Option<Vec<String>> {
    if depth > 2 {
        return None;
    }
    let v = unwrap_parens(v);
    if v.kind() == "array_creation_expression" {
        let (fields, complete) = array_fields(conn, root, scope, src, v, depth + 1);
        return complete.then(|| fields.into_iter().map(|f| f.name).collect());
    }
    let r = resource?;
    let s = resource_shape_depth(conn, root, r, depth + 1);
    s.complete
        .then(|| s.fields.into_iter().map(|f| f.name).collect())
}

fn nested_resource(conn: &Connection, scope: &Scope, src: &[u8], v: Node<'_>) -> Option<String> {
    let v = unwrap_parens(v);
    let class = match v.kind() {
        "object_creation_expression" => named_children(v)
            .into_iter()
            .find(|c| matches!(c.kind(), "name" | "qualified_name" | "relative_name"))
            .and_then(|n| scope.resolve_class(text(n, src))),
        "scoped_call_expression" => v
            .child_by_field_name("scope")
            .and_then(|n| scope.resolve_class(text(n, src))),
        _ => None,
    }?;
    is_resource(conn, &class).then_some(class)
}

/// Fields of `Resource::toArray()`.
pub fn resource_shape(conn: &Connection, root: &Path, class: &str) -> ResponseShape {
    resource_shape_depth(conn, root, class, 0)
}

fn resource_shape_depth(conn: &Connection, root: &Path, class: &str, depth: u8) -> ResponseShape {
    let mut s = ResponseShape {
        kind: "resource".into(),
        resource: Some(class.to_owned()),
        wrapped_in_data: true,
        ..ResponseShape::default()
    };
    let Some(p) = parse_class(conn, root, class) else {
        s.notes.push(format!("{class} is not in project code"));
        return s;
    };
    let Some((node, scope)) = find_class(p.tree.root_node(), &p.src, class) else {
        return s;
    };
    let Some(m) = method(node, &p.src, "toArray") else {
        s.notes.push(format!(
            "{class} has no toArray(): it returns the model's attributes"
        ));
        return s;
    };
    let Some(body) = m.child_by_field_name("body") else {
        return s;
    };
    let mut rets = Vec::new();
    returns(body, &mut rets);
    let Some(r) = rets.first().map(|r| unwrap_parens(*r)) else {
        return s;
    };
    if r.kind() != "array_creation_expression" {
        s.notes.push(format!(
            "toArray() returns `{}`",
            codemap_php::ast::truncate(&codemap_php::ast::compact(text(r, &p.src)), 60)
        ));
        return s;
    }
    s.complete = true;
    for el in named_children(r) {
        if el.kind() != "array_element_initializer" {
            continue;
        }
        let parts = named_children(el);
        match parts.as_slice() {
            [key, value] => {
                let k = crate::literal::eval(*key, &p.src, &scope);
                let Some(name) = k.as_str() else {
                    s.complete = false;
                    continue;
                };
                let vtext = text(*value, &p.src);
                let conditional = [
                    "$this->when",
                    "$this->whenLoaded",
                    "$this->whenNotNull",
                    "$this->whenCounted",
                    "$this->whenHas",
                    "$this->whenPivotLoaded",
                    "$this->whenAppended",
                ]
                .iter()
                .any(|w| vtext.contains(w));
                let resource = nested_resource(conn, &scope, &p.src, *value);
                let children = if depth < 2 {
                    children_of(
                        conn,
                        root,
                        &scope,
                        &p.src,
                        *value,
                        resource.as_deref(),
                        depth,
                    )
                } else {
                    None
                };
                s.fields.push(ShapeField {
                    name: name.to_owned(),
                    conditional,
                    resource,
                    children,
                });
            }
            [value] => {
                // `$this->merge([...])`, `$this->mergeWhen(cond, [...])`, `...parent::toArray()`
                let vtext = text(*value, &p.src);
                if vtext.contains("merge") {
                    if let Some(inner) = find_array(*value) {
                        let (fields, complete) =
                            array_fields(conn, root, &scope, &p.src, inner, depth + 1);
                        s.complete &= complete;
                        s.fields.extend(fields.into_iter().map(|mut f| {
                            f.conditional =
                                vtext.contains("mergeWhen") || vtext.contains("mergeUnless");
                            f
                        }));
                    } else {
                        s.complete = false;
                    }
                } else {
                    s.complete = false;
                    s.notes.push(format!(
                        "toArray() spreads `{}`",
                        codemap_php::ast::truncate(&codemap_php::ast::compact(vtext), 50)
                    ));
                }
            }
            _ => s.complete = false,
        }
    }
    s
}

fn find_array(n: Node<'_>) -> Option<Node<'_>> {
    if n.kind() == "array_creation_expression" {
        return Some(n);
    }
    named_children(n).into_iter().find_map(find_array)
}

/// Keys of the items of a list built in PHP, followed through local variables,
/// `$this->helper()` methods, `Cache::remember(…, fn () => …)`, collection /
/// array functions that keep items (`filter`, `values`, `array_slice`, …) and
/// item maps (`->map(fn ($m) => [...])`, `array_map(fn ($c) => $c + [...], …)`).
/// `None` whenever a step is not readable: keys are never guessed.
struct Items<'c, 't> {
    conn: &'c Connection,
    root: &'c Path,
    src: &'c [u8],
    scope: &'c Scope,
    class: Node<'t>,
    /// Steps left: a large method gives up rather than slow every answer.
    budget: std::cell::Cell<u32>,
}

/// Functions and collection methods whose result holds the same items.
const KEEPS_ITEMS: &[&str] = &[
    "array_values",
    "array_filter",
    "array_slice",
    "array_reverse",
    "array_unique",
    "collect",
    "values",
    "filter",
    "reject",
    "sortBy",
    "sortByDesc",
    "sort",
    "take",
    "skip",
    "slice",
    "unique",
    "uniqueStrict",
    "reverse",
    "all",
    "toArray",
    "where",
    "whereIn",
    "whereNotNull",
    "forPage",
    "shuffle",
];

fn union(a: Option<Vec<String>>, b: Option<Vec<String>>) -> Option<Vec<String>> {
    let (mut a, b) = (a?, b?);
    for k in b {
        if !a.contains(&k) {
            a.push(k);
        }
    }
    Some(a)
}

/// The closure's first parameter name and the expressions it returns.
fn closure_parts<'t>(n: Node<'t>, src: &[u8]) -> Option<(Option<String>, Vec<Node<'t>>)> {
    let n = unwrap_parens(n);
    if !matches!(
        n.kind(),
        "arrow_function" | "anonymous_function" | "anonymous_function_creation_expression"
    ) {
        return None;
    }
    let param = n.child_by_field_name("parameters").and_then(|ps| {
        named_children(ps).into_iter().find_map(|p| {
            p.child_by_field_name("name")
                .map(|v| text(v, src).to_owned())
        })
    });
    let body = n.child_by_field_name("body")?;
    let rets = if n.kind() == "arrow_function" {
        vec![body]
    } else {
        let mut v = Vec::new();
        returns(body, &mut v);
        v
    };
    Some((param, rets))
}

fn mentions(n: Node<'_>, src: &[u8], var: &str) -> bool {
    (n.kind() == "variable_name" && text(n, src) == var)
        || named_children(n).into_iter().any(|c| mentions(c, src, var))
}

/// Assignments to `var` in document order, not looking into closures.
fn assignments<'t>(node: Node<'t>, src: &[u8], var: &str, out: &mut Vec<Node<'t>>) {
    for c in named_children(node) {
        if matches!(
            c.kind(),
            "anonymous_function" | "arrow_function" | "anonymous_function_creation_expression"
        ) {
            continue;
        }
        if c.kind() == "assignment_expression"
            && c.child_by_field_name("left").map(|l| text(l, src)) == Some(var)
            && let Some(r) = c.child_by_field_name("right")
        {
            out.push(r);
        }
        assignments(c, src, var, out);
    }
}

impl<'c, 't> Items<'c, 't> {
    /// Item keys of the list `e` evaluates to. `this_var` is a variable being
    /// re-assigned from itself (`$x = array_slice($x, …)`) and its keys so far.
    fn list(
        &self,
        body: Node<'_>,
        e: Node<'_>,
        this_var: Option<(&str, &Option<Vec<String>>)>,
        depth: u8,
    ) -> Option<Vec<String>> {
        if depth > 24 || self.budget.get() == 0 {
            return None;
        }
        self.budget.set(self.budget.get() - 1);
        let e = unwrap_parens(e);
        let src = self.src;
        match e.kind() {
            "variable_name" => {
                let var = text(e, src);
                if let Some((v, keys)) = this_var
                    && v == var
                {
                    return (*keys).clone();
                }
                self.var(body, var, depth + 1)
            }
            "function_call_expression" => {
                let f = e
                    .child_by_field_name("function")
                    .map(|n| text(n, src).trim_start_matches('\\'))
                    .unwrap_or("");
                let a = args(e);
                match f {
                    "array_map" => {
                        let base = a
                            .get(1)
                            .and_then(|x| self.list(body, *x, this_var, depth + 1));
                        let (param, rets) = closure_parts(*a.first()?, src)?;
                        self.items_of(&rets, param.as_deref(), &base, depth)
                    }
                    _ if KEEPS_ITEMS.contains(&f) => {
                        self.list(body, *a.first()?, this_var, depth + 1)
                    }
                    _ => None,
                }
            }
            "member_call_expression" => {
                let name = e
                    .child_by_field_name("name")
                    .map(|n| text(n, src))
                    .unwrap_or("");
                let obj = e.child_by_field_name("object")?;
                if text(obj, src) == "$this" {
                    return self.method_list(name, depth + 1);
                }
                match name {
                    "map" | "transform" => {
                        let base = self.list(body, obj, this_var, depth + 1);
                        let (param, rets) = closure_parts(*args(e).first()?, src)?;
                        self.items_of(&rets, param.as_deref(), &base, depth)
                    }
                    _ if KEEPS_ITEMS.contains(&name) => self.list(body, obj, this_var, depth + 1),
                    _ => None,
                }
            }
            "scoped_call_expression" => {
                let name = e
                    .child_by_field_name("name")
                    .map(|n| text(n, src))
                    .unwrap_or("");
                let scope_text = e.child_by_field_name("scope").map(|n| text(n, src));
                if matches!(scope_text, Some("self" | "static")) {
                    return self.method_list(name, depth + 1);
                }
                // Cache::remember($key, $ttl, fn () => …) / rememberForever / flexible.
                if matches!(name, "remember" | "rememberForever" | "sear" | "flexible") {
                    let (_, rets) = closure_parts(*args(e).last()?, src)?;
                    let mut out: Option<Vec<String>> = None;
                    for r in rets {
                        let v = self.list(body, r, this_var, depth + 1);
                        out = Some(match out {
                            None => v?,
                            Some(prev) => union(Some(prev), v)?,
                        });
                    }
                    return out;
                }
                None
            }
            _ => None,
        }
    }

    /// Keys of `var`'s items after all its assignments in `body`.
    fn var(&self, body: Node<'_>, var: &str, depth: u8) -> Option<Vec<String>> {
        let mut rhs = Vec::new();
        assignments(body, self.src, var, &mut rhs);
        let mut state: Option<Option<Vec<String>>> = None;
        for r in rhs {
            let so_far = state.clone().flatten();
            let v = self.list(body, r, Some((var, &so_far)), depth + 1);
            state = Some(if mentions(r, self.src, var) {
                // A step on the same list: `$x = array_slice($x, …)`.
                v
            } else {
                // Another source (often another branch): items may be either.
                match state {
                    None => v,
                    Some(prev) => union(prev, v),
                }
            });
        }
        state.flatten()
    }

    /// Item keys of a list returned by `$this->name()` in the same class.
    fn method_list(&self, name: &str, depth: u8) -> Option<Vec<String>> {
        let m = method(self.class, self.src, name)?;
        let body = m.child_by_field_name("body")?;
        let mut rets = Vec::new();
        returns(body, &mut rets);
        let mut out: Option<Vec<String>> = None;
        for r in rets {
            let v = self.list(body, r, None, depth + 1);
            out = Some(match out {
                None => v?,
                Some(prev) => union(Some(prev), v)?,
            });
        }
        out
    }

    /// Keys of one item built by a closure returning `rets` from item `param`.
    fn items_of(
        &self,
        rets: &[Node<'_>],
        param: Option<&str>,
        base: &Option<Vec<String>>,
        depth: u8,
    ) -> Option<Vec<String>> {
        let mut out: Option<Vec<String>> = None;
        for r in rets {
            let v = self.item(*r, param, base, depth + 1);
            out = Some(match out {
                None => v?,
                Some(prev) => union(Some(prev), v)?,
            });
        }
        out
    }

    /// Keys of one item expression: a literal array, `$c + [...]`,
    /// `array_merge($c, [...])`, or a Resource.
    fn item(
        &self,
        e: Node<'_>,
        param: Option<&str>,
        base: &Option<Vec<String>>,
        depth: u8,
    ) -> Option<Vec<String>> {
        if depth > 24 {
            return None;
        }
        let e = unwrap_parens(e);
        let src = self.src;
        match e.kind() {
            "array_creation_expression" => {
                let (fields, complete) = array_fields(self.conn, self.root, self.scope, src, e, 0);
                complete.then(|| fields.into_iter().map(|f| f.name).collect())
            }
            "variable_name" if Some(text(e, src)) == param => base.clone(),
            "binary_expression"
                if e.child_by_field_name("operator").map(|o| text(o, src)) == Some("+") =>
            {
                let l = self.item(e.child_by_field_name("left")?, param, base, depth + 1);
                let r = self.item(e.child_by_field_name("right")?, param, base, depth + 1);
                union(l, r)
            }
            "function_call_expression"
                if e.child_by_field_name("function")
                    .map(|n| text(n, src).trim_start_matches('\\'))
                    == Some("array_merge") =>
            {
                let mut out = Some(Vec::new());
                for a in args(e) {
                    out = union(out, self.item(a, param, base, depth + 1));
                }
                out
            }
            _ => {
                let class = nested_resource(self.conn, self.scope, src, e)?;
                let s = resource_shape_depth(self.conn, self.root, &class, 1);
                s.complete
                    .then(|| s.fields.into_iter().map(|f| f.name).collect())
            }
        }
    }
}
