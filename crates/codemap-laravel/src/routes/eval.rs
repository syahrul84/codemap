//! Static evaluation of route registration code. A small interpreter over the
//! tree-sitter AST: it follows `Route::` chains, groups, includes and resource
//! registrations, and records anything it cannot evaluate as `unresolved`
//! instead of guessing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use codemap_php::Scope;
use codemap_php::ast::{apply_use_declaration, named_children, start_line, text};
use serde::Serialize;
use tree_sitter::Node;

use super::resource::{self, Names, ResourceKind, ResourceSpec};
use super::{ANY, ActionKind, GET, RouteDef, join_uri};
use crate::literal::{unquote_double, unquote_single};

const MAX_INCLUDE_DEPTH: usize = 16;
pub const ROUTE_FACADE: &str = "Illuminate\\Support\\Facades\\Route";
const VIEW_CONTROLLER: &str = "Illuminate\\Routing\\ViewController";
const REDIRECT_CONTROLLER: &str = "Illuminate\\Routing\\RedirectController";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Warning {
    pub file: Option<String>,
    pub line: Option<u32>,
    pub message: String,
}

/// Attributes inherited from enclosing groups and registrar calls.
#[derive(Debug, Clone, Default)]
pub struct Group {
    pub prefix: String,
    pub as_: String,
    pub middleware: Vec<String>,
    pub without_middleware: Vec<String>,
    pub namespace: Option<String>,
    pub controller: Option<String>,
    pub domain: Option<String>,
    pub wheres: BTreeMap<String, String>,
    pub defaults: BTreeMap<String, String>,
    pub conditional: bool,
    pub in_loop: bool,
    pub unresolved: Option<String>,
}

impl Group {
    pub fn with_middleware(middleware: &str) -> Self {
        Self {
            middleware: vec![middleware.to_owned()],
            ..Self::default()
        }
    }
}

/// A statically known value (or why it is not known).
#[derive(Debug, Clone)]
enum Val<'t> {
    Str(String),
    Int(i64),
    Bool(bool),
    Null,
    Class(String),
    Arr(Vec<(Option<String>, Val<'t>)>),
    Closure(Node<'t>),
    Dynamic(String),
}

impl Val<'_> {
    fn as_str(&self) -> Option<&str> {
        match self {
            Val::Str(s) | Val::Class(s) => Some(s),
            _ => None,
        }
    }

    /// A string or a list of strings.
    fn strings(&self) -> Option<Vec<String>> {
        match self {
            Val::Str(s) => Some(vec![s.clone()]),
            Val::Arr(items) => items
                .iter()
                .map(|(_, v)| v.as_str().map(str::to_owned))
                .collect(),
            _ => None,
        }
    }

    fn describe(&self) -> String {
        match self {
            Val::Dynamic(why) => why.clone(),
            Val::Closure(_) => "closure".into(),
            Val::Null => "null".into(),
            _ => "unexpected value".into(),
        }
    }
}

struct FileCtx<'s> {
    rel: String,
    dir: PathBuf,
    src: &'s [u8],
    scope: Scope,
    /// Literal `$namespace` of the RouteServiceProvider being evaluated.
    this_namespace: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Root {
    Route,
    This,
}

struct Call<'t> {
    name: String,
    args: Vec<(Option<String>, Node<'t>)>,
    node: Node<'t>,
}

impl<'t> Call<'t> {
    /// Named argument `name`, else the `idx`-th positional argument.
    fn arg(&self, idx: usize, name: &str) -> Option<Node<'t>> {
        self.args
            .iter()
            .find(|(n, _)| n.as_deref() == Some(name))
            .or_else(|| self.args.iter().filter(|(n, _)| n.is_none()).nth(idx))
            .map(|(_, v)| *v)
    }
}

struct ActionInfo {
    kind: ActionKind,
    controller: Option<String>,
    method: Option<String>,
    raw: Option<String>,
    reason: Option<String>,
    name: Option<String>,
    middleware: Vec<String>,
}

impl ActionInfo {
    fn unresolved(raw: Option<String>, reason: String) -> Self {
        Self {
            kind: ActionKind::Unresolved,
            controller: None,
            method: None,
            raw,
            reason: Some(reason),
            name: None,
            middleware: Vec::new(),
        }
    }

    fn controller(class: String, method: String) -> Self {
        Self {
            kind: ActionKind::Controller,
            raw: Some(format!("{class}@{method}")),
            controller: Some(class),
            method: Some(method),
            reason: None,
            name: None,
            middleware: Vec::new(),
        }
    }
}

const VERBS: &[&str] = &[
    "get",
    "post",
    "put",
    "patch",
    "delete",
    "options",
    "any",
    "match",
    "view",
    "redirect",
    "permanentRedirect",
    "fallback",
];
const RESOURCES: &[&str] = &[
    "resource",
    "apiResource",
    "singleton",
    "apiSingleton",
    "resources",
    "apiResources",
];

fn is_registration(name: &str) -> bool {
    name == "group" || VERBS.contains(&name) || RESOURCES.contains(&name)
}

pub struct Evaluator<'r> {
    root: &'r Path,
    pub routes: Vec<RouteDef>,
    pub warnings: Vec<Warning>,
    /// Every file read while evaluating (relative paths).
    pub files: BTreeSet<String>,
    stack: Vec<PathBuf>,
}

impl<'r> Evaluator<'r> {
    pub fn new(root: &'r Path) -> Self {
        Self {
            root,
            routes: Vec::new(),
            warnings: Vec::new(),
            files: BTreeSet::new(),
            stack: Vec::new(),
        }
    }

    fn warn(&mut self, file: Option<&str>, line: Option<u32>, message: impl Into<String>) {
        self.warnings.push(Warning {
            file: file.map(str::to_owned),
            line,
            message: message.into(),
        });
    }

    fn rel(&self, abs: &Path) -> Option<String> {
        let rel = abs.strip_prefix(self.root).ok()?;
        Some(
            rel.components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/"),
        )
    }

    fn read(&mut self, rel: &str) -> Option<Vec<u8>> {
        let src = std::fs::read(self.root.join(rel)).ok()?;
        self.files.insert(rel.to_owned());
        Some(src)
    }

    // ------------------------------------------------------------ entry points

    /// Evaluates a route file with the given group attributes.
    pub fn route_file(&mut self, rel: &str, group: &Group) {
        let abs = self.root.join(rel);
        self.include(abs, group, None);
    }

    /// Reads `withRouting(...)` in `bootstrap/app.php`. Returns false when the
    /// call is not found.
    pub fn bootstrap(&mut self, rel: &str) -> bool {
        let Some(src) = self.read(rel) else {
            return false;
        };
        let Some(tree) = codemap_php::parse::parse(&src) else {
            return false;
        };
        let mut f = self.file_ctx(rel, &src);
        let root_node = tree.root_node();
        self.collect_uses(root_node, &mut f);
        let Some(call_node) = find_call(root_node, "withRouting", f.src) else {
            return false;
        };
        let Some(call) = self.call(call_node, &f) else {
            return false;
        };
        let line = start_line(call_node);
        if let Some(using) = call.arg(0, "using")
            && !matches!(self.eval(using, &f), Val::Null)
        {
            self.warn(
                Some(rel),
                Some(line),
                "withRouting(using: ...) registers routes with custom code; they are not indexed",
            );
            return true;
        }
        let api_prefix = call
            .arg(7, "apiPrefix")
            .map(|n| self.eval(n, &f))
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "api".into());

        // Laravel's order: api, health, web, then.
        if let Some(api) = call.arg(2, "api") {
            let group = Group {
                prefix: api_prefix,
                middleware: vec!["api".into()],
                ..Group::default()
            };
            self.route_files_from(api, &group, &f, line);
        }
        if let Some(health) = call.arg(6, "health") {
            match self.eval(health, &f) {
                Val::Str(uri) => self.routes.push(RouteDef {
                    methods: strings(GET),
                    uri: join_uri("", &uri),
                    action_kind: ActionKind::Health,
                    action_raw: Some("Closure".into()),
                    file: Some(rel.to_owned()),
                    line: Some(line),
                    ..RouteDef::default()
                }),
                Val::Null => {}
                other => self.warn(
                    Some(rel),
                    Some(line),
                    format!("health route URI is not a literal ({})", other.describe()),
                ),
            }
        }
        if let Some(web) = call.arg(1, "web") {
            self.route_files_from(web, &Group::with_middleware("web"), &f, line);
        }
        if call.arg(5, "pages").is_some() {
            self.warn(
                Some(rel),
                Some(line),
                "Folio pages (withRouting pages:) are not indexed",
            );
        }
        if let Some(then) = call.arg(8, "then") {
            self.group_body(then, &Group::default(), &mut f, line);
        }
        true
    }

    /// Walks every method body of a RouteServiceProvider subclass.
    pub fn provider(&mut self, rel: &str, class_fqn: &str) {
        let Some(src) = self.read(rel) else {
            self.warn(Some(rel), None, format!("cannot read {class_fqn}"));
            return;
        };
        let Some(tree) = codemap_php::parse::parse(&src) else {
            return;
        };
        let mut f = self.file_ctx(rel, &src);
        self.provider_statements(tree.root_node(), class_fqn, &mut f);
    }

    fn provider_statements(&mut self, node: Node<'_>, class_fqn: &str, f: &mut FileCtx<'_>) {
        for child in named_children(node) {
            match child.kind() {
                "namespace_definition" => {
                    let name = child
                        .child_by_field_name("name")
                        .map(|n| text(n, f.src))
                        .unwrap_or("");
                    f.scope.set_namespace(name);
                    if let Some(body) = child.child_by_field_name("body") {
                        self.provider_statements(body, class_fqn, f);
                    }
                }
                "namespace_use_declaration" => apply_use_declaration(child, f.src, &mut f.scope),
                "class_declaration" => {
                    let name = child
                        .child_by_field_name("name")
                        .map(|n| text(n, f.src))
                        .unwrap_or("");
                    if !f.scope.qualify(name).eq_ignore_ascii_case(class_fqn) {
                        continue;
                    }
                    let Some(body) = child.child_by_field_name("body") else {
                        continue;
                    };
                    f.scope.class = Some(class_fqn.to_owned());
                    f.this_namespace = namespace_property(body, f.src);
                    for member in named_children(body) {
                        if member.kind() == "method_declaration"
                            && let Some(method_body) = member.child_by_field_name("body")
                        {
                            self.statements(method_body, &Group::default(), f);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // ------------------------------------------------------------ files

    fn file_ctx<'s>(&self, rel: &str, src: &'s [u8]) -> FileCtx<'s> {
        let dir = self
            .root
            .join(rel)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.root.to_path_buf());
        FileCtx {
            rel: rel.to_owned(),
            dir,
            src,
            scope: Scope::default(),
            this_namespace: None,
        }
    }

    fn collect_uses(&self, node: Node<'_>, f: &mut FileCtx<'_>) {
        for child in named_children(node) {
            match child.kind() {
                "namespace_use_declaration" => apply_use_declaration(child, f.src, &mut f.scope),
                "namespace_definition" => {
                    let name = child
                        .child_by_field_name("name")
                        .map(|n| text(n, f.src))
                        .unwrap_or("");
                    f.scope.set_namespace(name);
                }
                _ => {}
            }
        }
    }

    fn include(&mut self, abs: PathBuf, group: &Group, from: Option<(&str, u32)>) {
        let abs = normalize(&abs);
        let Some(rel) = self.rel(&abs) else {
            self.warn(
                from.map(|f| f.0),
                from.map(|f| f.1),
                format!("route file {} is outside the project", abs.display()),
            );
            return;
        };
        if self.stack.contains(&abs) || self.stack.len() >= MAX_INCLUDE_DEPTH {
            self.warn(
                from.map(|f| f.0),
                from.map(|f| f.1),
                format!("{rel} is included recursively; skipped"),
            );
            return;
        }
        let Some(src) = self.read(&rel) else {
            self.warn(
                from.map(|f| f.0),
                from.map(|f| f.1),
                format!("route file {rel} not found"),
            );
            return;
        };
        let Some(tree) = codemap_php::parse::parse(&src) else {
            return;
        };
        if tree.root_node().has_error() {
            self.warn(
                Some(&rel),
                None,
                "route file has syntax errors; routes may be missing",
            );
        }
        let mut f = self.file_ctx(&rel, &src);
        self.stack.push(abs);
        self.statements(tree.root_node(), group, &mut f);
        self.stack.pop();
    }

    /// A string path or a list of paths (withRouting `web:` / `api:`).
    fn route_files_from(&mut self, node: Node<'_>, group: &Group, f: &FileCtx<'_>, line: u32) {
        let val = self.eval(node, f);
        match val.strings() {
            Some(paths) => {
                for p in paths {
                    let abs = self.path(&p, f);
                    self.include(abs, group, Some((&f.rel, line)));
                }
            }
            None if matches!(val, Val::Null) => {}
            None => self.warn(
                Some(&f.rel),
                Some(line),
                format!("route file path is not a literal ({})", val.describe()),
            ),
        }
    }

    fn path(&self, p: &str, f: &FileCtx<'_>) -> PathBuf {
        let path = Path::new(p);
        if path.is_absolute() {
            path.to_path_buf()
        } else if p.starts_with("./") || p.starts_with("../") {
            f.dir.join(path)
        } else {
            self.root.join(path)
        }
    }

    // ------------------------------------------------------------ statements

    fn statements(&mut self, node: Node<'_>, g: &Group, f: &mut FileCtx<'_>) {
        for child in named_children(node) {
            self.statement(child, g, f);
        }
    }

    fn statement(&mut self, n: Node<'_>, g: &Group, f: &mut FileCtx<'_>) {
        match n.kind() {
            "namespace_use_declaration" => apply_use_declaration(n, f.src, &mut f.scope),
            "namespace_definition" => {
                let name = n
                    .child_by_field_name("name")
                    .map(|c| text(c, f.src))
                    .unwrap_or("");
                f.scope.set_namespace(name);
                if let Some(body) = n.child_by_field_name("body") {
                    self.statements(body, g, f);
                }
            }
            "expression_statement" | "return_statement" => {
                if let Some(e) = named_children(n).into_iter().next() {
                    self.expression(e, g, f);
                }
            }
            "compound_statement" | "colon_block" => self.statements(n, g, f),
            "if_statement" | "else_clause" | "else_if_clause" => {
                let inner = Group {
                    conditional: true,
                    ..g.clone()
                };
                for c in named_children(n) {
                    if c.kind() != "parenthesized_expression" {
                        self.statement(c, &inner, f);
                    }
                }
            }
            "foreach_statement" | "for_statement" | "while_statement" | "do_statement" => {
                let inner = Group {
                    in_loop: true,
                    ..g.clone()
                };
                if let Some(body) = n.child_by_field_name("body") {
                    self.statement(body, &inner, f);
                }
            }
            _ => {}
        }
    }

    fn expression(&mut self, e: Node<'_>, g: &Group, f: &mut FileCtx<'_>) {
        match e.kind() {
            "include_expression"
            | "include_once_expression"
            | "require_expression"
            | "require_once_expression" => {
                let line = start_line(e);
                let Some(arg) = named_children(e).into_iter().next() else {
                    return;
                };
                match self.eval(arg, f) {
                    Val::Str(p) => {
                        let abs = self.path(&p, f);
                        self.include(abs, g, Some((&f.rel.clone(), line)));
                    }
                    other => self.warn(
                        Some(&f.rel.clone()),
                        Some(line),
                        format!("include path is not a literal ({})", other.describe()),
                    ),
                }
            }
            "member_call_expression"
            | "scoped_call_expression"
            | "nullsafe_member_call_expression" => self.chain(e, g, f),
            "assignment_expression" => {
                if let Some(r) = e.child_by_field_name("right") {
                    self.expression(r, g, f);
                }
            }
            "parenthesized_expression" => {
                if let Some(inner) = named_children(e).into_iter().next() {
                    self.expression(inner, g, f);
                }
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------ chains

    fn call<'t>(&self, n: Node<'t>, f: &FileCtx<'_>) -> Option<Call<'t>> {
        let name = text(n.child_by_field_name("name")?, f.src).to_owned();
        let args = n
            .child_by_field_name("arguments")
            .map(|a| {
                named_children(a)
                    .into_iter()
                    .filter(|x| x.kind() == "argument")
                    .filter_map(|arg| {
                        let name_node = arg.child_by_field_name("name");
                        let value = named_children(arg)
                            .into_iter()
                            .find(|c| Some(c.id()) != name_node.map(|nn| nn.id()))?;
                        Some((name_node.map(|nn| text(nn, f.src).to_owned()), value))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Call {
            name,
            args,
            node: n,
        })
    }

    fn flatten<'t>(&self, n: Node<'t>, f: &FileCtx<'_>) -> Option<(Root, Vec<Call<'t>>)> {
        match n.kind() {
            "member_call_expression" | "nullsafe_member_call_expression" => {
                let obj = n.child_by_field_name("object")?;
                let call = self.call(n, f)?;
                let (root, mut calls) = if obj.kind() == "variable_name" {
                    match text(obj, f.src) {
                        "$this" => (Root::This, Vec::new()),
                        "$router" | "$route" => (Root::Route, Vec::new()),
                        _ => return None,
                    }
                } else {
                    self.flatten(obj, f)?
                };
                calls.push(call);
                Some((root, calls))
            }
            "scoped_call_expression" => {
                let scope = text(n.child_by_field_name("scope")?, f.src);
                let is_route = scope.trim_start_matches('\\') == "Route"
                    || f.scope.resolve_class(scope).as_deref() == Some(ROUTE_FACADE);
                is_route.then(|| self.call(n, f).map(|c| (Root::Route, vec![c])))?
            }
            _ => None,
        }
    }

    fn chain(&mut self, n: Node<'_>, g: &Group, f: &mut FileCtx<'_>) {
        let Some((root, calls)) = self.flatten(n, f) else {
            return;
        };
        let line = start_line(calls[0].node);
        if root == Root::This {
            // RouteServiceProvider: `$this->routes(fn)`, `$this->loadRoutesFrom(path)`.
            let call = &calls[0];
            match call.name.as_str() {
                "routes" => {
                    if let Some(cb) = call.arg(0, "routesCallback") {
                        self.group_body(cb, g, f, line);
                    }
                }
                "loadRoutesFrom" => {
                    if let Some(p) = call.arg(0, "path") {
                        self.route_files_from(p, g, f, line);
                    }
                }
                _ => {}
            }
            return;
        }

        let Some(i) = calls.iter().position(|c| is_registration(&c.name)) else {
            return; // Route::pattern(), Route::bind(), Route::macro(), ...
        };
        let mut group = g.clone();
        let mut unknown = Vec::new();
        for c in &calls[..i] {
            self.apply_attr(&mut group, c, f, &mut unknown);
        }
        let reg = &calls[i];
        let post = &calls[i + 1..];
        match reg.name.as_str() {
            "group" => {
                if !unknown.is_empty() {
                    group.unresolved = Some(unknown_reason(&unknown));
                }
                self.group_call(reg, group, f, line);
            }
            name if RESOURCES.contains(&name) => {
                self.resource_call(reg, post, &group, f, line, unknown);
            }
            _ => self.verb_call(reg, post, group, f, line, unknown),
        }
    }

    /// Applies a registrar attribute (`prefix`, `name`, `middleware`, ...).
    fn apply_attr(
        &mut self,
        g: &mut Group,
        c: &Call<'_>,
        f: &FileCtx<'_>,
        unknown: &mut Vec<String>,
    ) {
        let args: Vec<Val<'_>> = c.args.iter().map(|(_, v)| self.eval(*v, f)).collect();
        let first = args.first();
        match c.name.as_str() {
            "prefix" => match first {
                Some(Val::Str(p)) => g.prefix = join_prefix(&g.prefix, p),
                _ => g.unresolved = Some("group prefix is not a literal".into()),
            },
            "name" | "as" => match first {
                Some(Val::Str(n)) => g.as_.push_str(n),
                _ => g.unresolved = Some("route name is not a literal".into()),
            },
            "middleware" => {
                for a in &args {
                    match a.strings() {
                        Some(m) => g.middleware.extend(m),
                        None => self.warn(
                            Some(&f.rel),
                            Some(start_line(c.node)),
                            format!("middleware is not a literal ({})", a.describe()),
                        ),
                    }
                }
            }
            "withoutMiddleware" => {
                for a in &args {
                    g.without_middleware.extend(a.strings().unwrap_or_default());
                }
            }
            "namespace" => {
                if let Some(Val::Str(ns)) = first {
                    g.namespace = Some(merge_namespace(g.namespace.as_deref(), ns));
                }
            }
            "controller" => match first {
                Some(Val::Class(c)) => g.controller = Some(c.clone()),
                Some(Val::Str(c)) => g.controller = Some(resolve_string_class(c, g)),
                _ => g.unresolved = Some("group controller is not a literal".into()),
            },
            "domain" => match first {
                Some(Val::Str(d)) => g.domain = Some(d.clone()),
                _ => g.unresolved = Some("domain is not a literal".into()),
            },
            "where" => match (args.first(), args.get(1)) {
                (Some(Val::Str(k)), Some(Val::Str(v))) => {
                    g.wheres.insert(k.clone(), v.clone());
                }
                (Some(Val::Arr(items)), _) => {
                    for (k, v) in items {
                        if let (Some(k), Some(v)) = (k, v.as_str()) {
                            g.wheres.insert(k.clone(), v.to_owned());
                        }
                    }
                }
                _ => {}
            },
            "whereNumber" | "whereAlpha" | "whereAlphaNumeric" | "whereUuid" | "whereUlid" => {
                let pattern = match c.name.as_str() {
                    "whereNumber" => "[0-9]+",
                    "whereAlpha" => "[a-zA-Z]+",
                    "whereAlphaNumeric" => "[a-zA-Z0-9]+",
                    "whereUuid" => "uuid",
                    _ => "ulid",
                };
                for p in first.and_then(Val::strings).unwrap_or_default() {
                    g.wheres.insert(p, pattern.into());
                }
            }
            "whereIn" => {
                if let (Some(Val::Str(k)), Some(values)) = (args.first(), args.get(1)) {
                    let values = values.strings().unwrap_or_default().join(",");
                    g.wheres.insert(k.clone(), format!("in:{values}"));
                }
            }
            "can" => {
                if let Some(Val::Str(ability)) = first {
                    let mut m = format!("can:{ability}");
                    if let Some(model) = args.get(1).and_then(Val::as_str) {
                        m.push(',');
                        m.push_str(model);
                    }
                    g.middleware.push(m);
                }
            }
            "defaults" => {
                if let (Some(Val::Str(k)), Some(v)) = (args.first(), args.get(1)) {
                    let v = match v {
                        Val::Str(s) => s.clone(),
                        Val::Int(i) => i.to_string(),
                        Val::Bool(b) => b.to_string(),
                        other => other.describe(),
                    };
                    g.defaults.insert(k.clone(), v);
                }
            }
            "scopeBindings"
            | "withoutScopedBindings"
            | "missing"
            | "withTrashed"
            | "fallback"
            | "block"
            | "withoutBlocking"
            | "lock"
            | "setBindingFields" => {}
            other => unknown.push(other.to_owned()),
        }
    }

    fn apply_array_attrs(&mut self, g: &mut Group, items: &[(Option<String>, Val<'_>)]) {
        for (key, v) in items {
            let Some(key) = key else { continue };
            match (key.as_str(), v) {
                ("prefix", Val::Str(p)) => g.prefix = join_prefix(&g.prefix, p),
                ("as", Val::Str(n)) => g.as_.push_str(n),
                ("middleware", v) => g.middleware.extend(v.strings().unwrap_or_default()),
                ("excluded_middleware" | "withoutMiddleware", v) => {
                    g.without_middleware.extend(v.strings().unwrap_or_default());
                }
                ("namespace", Val::Str(ns)) => {
                    g.namespace = Some(merge_namespace(g.namespace.as_deref(), ns));
                }
                ("domain", Val::Str(d)) => g.domain = Some(d.clone()),
                ("controller", Val::Class(c) | Val::Str(c)) => g.controller = Some(c.clone()),
                ("where", Val::Arr(ws)) => {
                    for (k, v) in ws {
                        if let (Some(k), Some(v)) = (k, v.as_str()) {
                            g.wheres.insert(k.clone(), v.to_owned());
                        }
                    }
                }
                ("prefix" | "as" | "domain", _) => {
                    g.unresolved = Some(format!("group `{key}` is not a literal"));
                }
                _ => {}
            }
        }
    }

    fn group_call(&mut self, reg: &Call<'_>, mut group: Group, f: &mut FileCtx<'_>, line: u32) {
        let (attrs, cb) = match reg.args.len() {
            0 => return,
            1 => (None, reg.args[0].1),
            _ => (Some(reg.args[0].1), reg.args[1].1),
        };
        if let Some(attrs) = attrs {
            match self.eval(attrs, f) {
                Val::Arr(items) => self.apply_array_attrs(&mut group, &items),
                other => self.warn(
                    Some(&f.rel),
                    Some(line),
                    format!(
                        "group attributes are not a literal array ({})",
                        other.describe()
                    ),
                ),
            }
        }
        self.group_body(cb, &group, f, line);
    }

    fn group_body(&mut self, cb: Node<'_>, g: &Group, f: &mut FileCtx<'_>, line: u32) {
        match self.eval(cb, f) {
            Val::Closure(n) if n.kind() == "anonymous_function" => {
                if let Some(body) = n.child_by_field_name("body") {
                    self.statements(body, g, f);
                }
            }
            Val::Closure(n) => {
                if let Some(body) = n.child_by_field_name("body") {
                    self.expression(body, g, f);
                }
            }
            Val::Str(p) => {
                let abs = self.path(&p, f);
                let rel = f.rel.clone();
                self.include(abs, g, Some((&rel, line)));
            }
            other => {
                let rel = f.rel.clone();
                self.warn(
                    Some(&rel),
                    Some(line),
                    format!(
                        "group callback is not a closure or a file path ({})",
                        other.describe()
                    ),
                );
            }
        }
    }

    // ------------------------------------------------------------ routes

    #[allow(clippy::too_many_arguments)]
    fn finish(
        &mut self,
        mut r: RouteDef,
        g: &Group,
        f: &FileCtx<'_>,
        line: u32,
        unknown: &[String],
        reason: Option<String>,
    ) {
        r.middleware = g.middleware.iter().cloned().chain(r.middleware).collect();
        r.prefix = (!g.prefix.is_empty()).then(|| g.prefix.clone());
        r.domain = g.domain.clone();
        r.file = Some(f.rel.clone());
        r.line = Some(line);
        r.extra.wheres.extend(g.wheres.clone());
        r.extra.defaults.extend(g.defaults.clone());
        r.extra
            .without_middleware
            .extend(g.without_middleware.clone());
        r.extra.conditional = g.conditional;
        r.unresolved_reason = g
            .unresolved
            .clone()
            .or_else(|| g.in_loop.then(|| "defined inside a loop".to_owned()))
            .or_else(|| (!unknown.is_empty()).then(|| unknown_reason(unknown)))
            .or(reason)
            .or(r.unresolved_reason);
        if g.conditional {
            self.warn(
                Some(&f.rel),
                Some(line),
                format!(
                    "{} {} is registered inside an `if`; it may not exist at runtime",
                    r.methods_str(),
                    r.uri
                ),
            );
        }
        self.routes.push(r);
    }

    fn verb_call(
        &mut self,
        reg: &Call<'_>,
        post: &[Call<'_>],
        mut g: Group,
        f: &mut FileCtx<'_>,
        line: u32,
        mut unknown: Vec<String>,
    ) {
        for c in post {
            self.apply_attr(&mut g, c, f, &mut unknown);
        }
        let eval_arg = |s: &Self, idx: usize, name: &str| reg.arg(idx, name).map(|n| s.eval(n, f));
        let mut r = RouteDef::default();
        let (methods, uri, action): (Vec<String>, Option<Val<'_>>, Option<Val<'_>>) =
            match reg.name.as_str() {
                "get" => (
                    strings(GET),
                    eval_arg(self, 0, "uri"),
                    eval_arg(self, 1, "action"),
                ),
                "any" => (
                    strings(ANY),
                    eval_arg(self, 0, "uri"),
                    eval_arg(self, 1, "action"),
                ),
                "match" => {
                    let mut m: Vec<String> = eval_arg(self, 0, "methods")
                        .and_then(|v| v.strings())
                        .unwrap_or_default()
                        .into_iter()
                        .map(|s| s.to_uppercase())
                        .collect();
                    if m.contains(&"GET".to_owned()) && !m.contains(&"HEAD".to_owned()) {
                        m.push("HEAD".into());
                    }
                    if m.is_empty() {
                        r.unresolved_reason = Some("Route::match methods are not a literal".into());
                    }
                    (m, eval_arg(self, 1, "uri"), eval_arg(self, 2, "action"))
                }
                "view" => {
                    r.action_kind = ActionKind::View;
                    r.action_raw = Some(VIEW_CONTROLLER.into());
                    r.extra.view =
                        eval_arg(self, 1, "view").and_then(|v| v.as_str().map(str::to_owned));
                    (strings(GET), eval_arg(self, 0, "uri"), None)
                }
                "redirect" | "permanentRedirect" => {
                    r.action_kind = ActionKind::Redirect;
                    r.action_raw = Some(REDIRECT_CONTROLLER.into());
                    r.extra.redirect_to = eval_arg(self, 1, "destination")
                        .and_then(|v| v.as_str().map(str::to_owned));
                    r.extra.status = Some(if reg.name == "permanentRedirect" {
                        301
                    } else {
                        match eval_arg(self, 2, "status") {
                            Some(Val::Int(s)) => u16::try_from(s).unwrap_or(302),
                            _ => 302,
                        }
                    });
                    (strings(ANY), eval_arg(self, 0, "uri"), None)
                }
                "fallback" => {
                    r.extra
                        .wheres
                        .insert("fallbackPlaceholder".into(), ".*".into());
                    (
                        strings(GET),
                        Some(Val::Str("{fallbackPlaceholder}".into())),
                        eval_arg(self, 0, "action"),
                    )
                }
                verb => (
                    vec![verb.to_uppercase()],
                    eval_arg(self, 0, "uri"),
                    eval_arg(self, 1, "action"),
                ),
            };
        r.methods = methods;

        let mut reason = None;
        r.uri = match &uri {
            Some(Val::Str(u)) => join_uri(&g.prefix, u),
            other => {
                reason = Some(format!(
                    "URI is not a literal ({})",
                    other.as_ref().map_or("missing".into(), Val::describe)
                ));
                let raw = reg
                    .arg(usize::from(reg.name == "match"), "uri")
                    .map(|n| text(n, f.src).to_owned())
                    .unwrap_or_default();
                join_uri(&g.prefix, &raw)
            }
        };

        if !matches!(r.action_kind, ActionKind::View | ActionKind::Redirect) {
            let info = self.resolve_action(action, &g);
            r.action_kind = if reg.name == "fallback" && info.kind == ActionKind::Closure {
                ActionKind::Fallback
            } else {
                info.kind
            };
            r.controller_fqn = info.controller;
            r.controller_method = info.method;
            r.action_raw = info.raw;
            r.middleware = info.middleware;
            if let Some(n) = info.name {
                g.as_.push_str(&n);
            }
            reason = reason.or(info.reason);
        }
        r.name = (!g.as_.is_empty() && reg.name != "fallback").then(|| g.as_.clone());
        self.finish(r, &g, f, line, &unknown, reason);
    }

    fn resolve_action(&self, action: Option<Val<'_>>, g: &Group) -> ActionInfo {
        match action {
            None => ActionInfo::unresolved(None, "route has no action".into()),
            Some(Val::Closure(_)) => ActionInfo {
                kind: ActionKind::Closure,
                raw: Some("Closure".into()),
                ..ActionInfo::unresolved(None, String::new())
            }
            .with_reason(None),
            Some(Val::Class(c)) => ActionInfo::controller(c, "__invoke".into()),
            Some(Val::Str(s)) => string_action(&s, g),
            Some(Val::Arr(items)) => {
                let positional: Vec<&Val<'_>> = items
                    .iter()
                    .filter(|(k, _)| k.is_none())
                    .map(|(_, v)| v)
                    .collect();
                if let [class, Val::Str(method)] = positional.as_slice()
                    && items.len() == 2
                {
                    return match class {
                        Val::Class(c) => ActionInfo::controller(c.clone(), method.clone()),
                        Val::Str(c) => {
                            ActionInfo::controller(resolve_string_class(c, g), method.clone())
                        }
                        other => ActionInfo::unresolved(
                            None,
                            format!("controller is not a literal ({})", other.describe()),
                        ),
                    };
                }
                let get = |key: &str| items.iter().find(|(k, _)| k.as_deref() == Some(key));
                let mut info = match get("uses") {
                    Some((_, uses)) => self.resolve_action(Some(uses.clone()), g),
                    None => match positional.iter().find(|v| matches!(v, Val::Closure(_))) {
                        Some(c) => self.resolve_action(Some((*c).clone()), g),
                        None => ActionInfo::unresolved(None, "action array has no `uses`".into()),
                    },
                };
                if let Some((_, Val::Str(n))) = get("as") {
                    info.name = Some(n.clone());
                }
                if let Some((_, m)) = get("middleware") {
                    info.middleware = m.strings().unwrap_or_default();
                }
                info
            }
            Some(other) => ActionInfo::unresolved(
                None,
                format!("action is not a literal ({})", other.describe()),
            ),
        }
    }

    fn resource_call(
        &mut self,
        reg: &Call<'_>,
        post: &[Call<'_>],
        g: &Group,
        f: &mut FileCtx<'_>,
        line: u32,
        mut unknown: Vec<String>,
    ) {
        let kind = match reg.name.as_str() {
            "apiResource" | "apiResources" => ResourceKind::Api,
            "singleton" => ResourceKind::Singleton,
            "apiSingleton" => ResourceKind::ApiSingleton,
            _ => ResourceKind::Resource,
        };
        let mut specs: Vec<(ResourceSpec, Option<String>)> = Vec::new();
        let controller_of = |v: &Val<'_>| match v {
            Val::Class(c) => Some(c.clone()),
            Val::Str(c) => Some(resolve_string_class(c, g)),
            _ => None,
        };
        if reg.name == "resources" || reg.name == "apiResources" {
            match reg.arg(0, "resources").map(|n| self.eval(n, f)) {
                Some(Val::Arr(items)) => {
                    for (name, controller) in &items {
                        match name {
                            Some(name) => {
                                specs.push((
                                    ResourceSpec::new(kind, name, controller_of(controller)),
                                    None,
                                ));
                            }
                            None => self.warn(
                                Some(&f.rel),
                                Some(line),
                                "resource name is not a literal",
                            ),
                        }
                    }
                }
                other => self.warn(
                    Some(&f.rel),
                    Some(line),
                    format!(
                        "resources() argument is not a literal array ({})",
                        other.map_or("missing".into(), |v| v.describe())
                    ),
                ),
            }
        } else {
            let name = reg.arg(0, "name").map(|n| self.eval(n, f));
            let controller = reg
                .arg(1, "controller")
                .map(|n| self.eval(n, f))
                .and_then(|v| controller_of(&v));
            match name {
                Some(Val::Str(name)) => {
                    specs.push((ResourceSpec::new(kind, &name, controller), None))
                }
                other => {
                    let raw = reg
                        .arg(0, "name")
                        .map(|n| text(n, f.src).to_owned())
                        .unwrap_or_default();
                    specs.push((
                        ResourceSpec::new(kind, raw.trim_matches(['\'', '"']), controller),
                        Some(format!(
                            "resource name is not a literal ({})",
                            other.map_or("missing".into(), |v| v.describe())
                        )),
                    ));
                }
            }
        }
        let options = reg.arg(2, "options").map(|n| self.eval(n, f));
        let mut g = g.clone();
        for (spec, _) in &mut specs {
            if let Some(Val::Arr(opts)) = &options {
                apply_resource_options(spec, opts);
            }
            for c in post {
                let args: Vec<Val<'_>> = c.args.iter().map(|(_, v)| self.eval(*v, f)).collect();
                let all_strings = || -> Vec<String> {
                    args.iter()
                        .flat_map(|a| a.strings().unwrap_or_default())
                        .collect()
                };
                match c.name.as_str() {
                    "only" => spec.only = Some(all_strings()),
                    "except" => spec.except = all_strings(),
                    "names" => match args.first() {
                        Some(Val::Str(base)) => spec.names = Names::Base(base.clone()),
                        Some(Val::Arr(map)) => spec.names = Names::Map(string_map(map)),
                        _ => {}
                    },
                    "name" => {
                        if let (Some(Val::Str(m)), Some(Val::Str(n))) = (args.first(), args.get(1))
                        {
                            let mut map = match std::mem::take(&mut spec.names) {
                                Names::Map(m) => m,
                                _ => BTreeMap::new(),
                            };
                            map.insert(m.clone(), n.clone());
                            spec.names = Names::Map(map);
                        }
                    }
                    "parameters" => {
                        if let Some(Val::Arr(map)) = args.first() {
                            spec.parameters.extend(string_map(map));
                        }
                    }
                    "parameter" => {
                        if let (Some(Val::Str(k)), Some(Val::Str(v))) = (args.first(), args.get(1))
                        {
                            spec.parameters.insert(k.clone(), v.clone());
                        }
                    }
                    "shallow" => spec.shallow = !matches!(args.first(), Some(Val::Bool(false))),
                    "creatable" => spec.creatable = true,
                    "destroyable" => spec.destroyable = true,
                    "middleware" => spec.middleware.extend(all_strings()),
                    "withoutMiddleware" => g.without_middleware.extend(all_strings()),
                    "middlewareFor" | "withoutMiddlewareFor" => self.warn(
                        Some(&f.rel),
                        Some(line),
                        format!("->{}() on resources is not applied per method yet", c.name),
                    ),
                    "scoped" | "missing" | "withTrashed" => {}
                    // PendingResourceRegistration::where*() applies to every route.
                    "where" | "whereNumber" | "whereAlpha" | "whereAlphaNumeric" | "whereUuid"
                    | "whereUlid" | "whereIn" => {
                        let mut ignored = Vec::new();
                        self.apply_attr(&mut g, c, f, &mut ignored);
                    }
                    other => unknown.push(other.to_owned()),
                }
            }
        }
        for (spec, reason) in specs {
            let (prefix, expanded) = resource::expand(&spec);
            let prefix = join_prefix(&g.prefix, &prefix);
            for mut r in resource::to_routes(&spec, expanded) {
                r.uri = join_uri(&prefix, &r.uri);
                r.name = r.name.map(|n| format!("{}{n}", g.as_));
                self.finish(r, &g, f, line, &unknown, reason.clone());
            }
        }
    }

    // ------------------------------------------------------------ values

    fn eval<'t>(&self, n: Node<'t>, f: &FileCtx<'_>) -> Val<'t> {
        let raw = text(n, f.src);
        match n.kind() {
            "string" => Val::Str(unquote_single(raw)),
            "encapsed_string" => {
                let interpolated = named_children(n)
                    .iter()
                    .any(|c| !matches!(c.kind(), "string_content" | "escape_sequence"));
                if interpolated {
                    Val::Dynamic("interpolated string".into())
                } else {
                    Val::Str(unquote_double(raw))
                }
            }
            "integer" => raw
                .replace('_', "")
                .parse()
                .map_or_else(|_| Val::Dynamic("integer".into()), Val::Int),
            "boolean" => Val::Bool(raw.eq_ignore_ascii_case("true")),
            "null" => Val::Null,
            "name" => match raw {
                "__DIR__" => Val::Str(f.dir.to_string_lossy().into_owned()),
                "__FILE__" => Val::Str(self.root.join(&f.rel).to_string_lossy().into_owned()),
                other => Val::Dynamic(format!("constant {other}")),
            },
            "parenthesized_expression" => named_children(n)
                .into_iter()
                .next()
                .map_or(Val::Null, |c| self.eval(c, f)),
            "binary_expression" => {
                let op = n.child_by_field_name("operator").map(|o| text(o, f.src));
                let (Some(l), Some(r)) = (
                    n.child_by_field_name("left"),
                    n.child_by_field_name("right"),
                ) else {
                    return Val::Dynamic("expression".into());
                };
                match (op, self.eval(l, f), self.eval(r, f)) {
                    (Some("."), Val::Str(a), Val::Str(b)) => Val::Str(a + &b),
                    (Some("."), a, b) => Val::Dynamic(match (a, b) {
                        (Val::Dynamic(why), _) | (_, Val::Dynamic(why)) => why,
                        _ => "concatenation".into(),
                    }),
                    _ => Val::Dynamic("expression".into()),
                }
            }
            "class_constant_access_expression" => {
                let parts = named_children(n);
                match (parts.first(), parts.last()) {
                    (Some(scope), Some(name)) if text(*name, f.src) == "class" => f
                        .scope
                        .resolve_class(text(*scope, f.src))
                        .map_or_else(|| Val::Dynamic("class name".into()), Val::Class),
                    _ => Val::Dynamic(format!("constant {raw}")),
                }
            }
            "array_creation_expression" => {
                let mut items = Vec::new();
                for el in named_children(n) {
                    if el.kind() != "array_element_initializer" {
                        continue;
                    }
                    let parts = named_children(el);
                    match parts.as_slice() {
                        [value] if value.kind() != "variadic_unpacking" => {
                            items.push((None, self.eval(*value, f)));
                        }
                        [key, value] => {
                            let key = match self.eval(*key, f) {
                                Val::Str(k) => Some(k),
                                Val::Int(i) => Some(i.to_string()),
                                Val::Class(c) => Some(c),
                                _ => return Val::Dynamic("array key".into()),
                            };
                            items.push((key, self.eval(*value, f)));
                        }
                        _ => return Val::Dynamic("array spread".into()),
                    }
                }
                Val::Arr(items)
            }
            "anonymous_function" | "arrow_function" => Val::Closure(n),
            "function_call_expression" => {
                let fname = n
                    .child_by_field_name("function")
                    .map(|c| text(c, f.src).trim_start_matches('\\'))
                    .unwrap_or("");
                let base = match fname {
                    "base_path" => "",
                    "app_path" => "app",
                    "database_path" => "database",
                    "config_path" => "config",
                    "resource_path" => "resources",
                    other => return Val::Dynamic(format!("{other}()")),
                };
                let arg = n
                    .child_by_field_name("arguments")
                    .and_then(|a| named_children(a).into_iter().next())
                    .and_then(|a| named_children(a).into_iter().next())
                    .map(|a| self.eval(a, f));
                let sub = match arg {
                    None => String::new(),
                    Some(Val::Str(s)) => s,
                    Some(other) => return Val::Dynamic(other.describe()),
                };
                let mut p = self.root.join(base);
                if !sub.is_empty() {
                    p = p.join(sub.trim_start_matches('/'));
                }
                Val::Str(p.to_string_lossy().into_owned())
            }
            "member_access_expression" => {
                let obj = n.child_by_field_name("object").map(|o| text(o, f.src));
                let name = n.child_by_field_name("name").map(|o| text(o, f.src));
                if obj == Some("$this") && name == Some("namespace") {
                    f.this_namespace.clone().map_or(Val::Null, Val::Str)
                } else {
                    Val::Dynamic(format!("property {raw}"))
                }
            }
            "variable_name" => Val::Dynamic(format!("variable {raw}")),
            other => Val::Dynamic(other.replace('_', " ")),
        }
    }
}

impl ActionInfo {
    fn with_reason(mut self, reason: Option<String>) -> Self {
        self.reason = reason;
        self
    }
}

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

fn unknown_reason(unknown: &[String]) -> String {
    let calls: Vec<String> = unknown.iter().map(|u| format!("->{u}()")).collect();
    format!("unknown route method {} (macro?)", calls.join(", "))
}

pub fn join_prefix(a: &str, b: &str) -> String {
    let joined = format!("{}/{}", a.trim_matches('/'), b.trim_matches('/'));
    joined.trim_matches('/').to_owned()
}

/// Laravel's `RouteGroup::formatNamespace`.
fn merge_namespace(old: Option<&str>, new: &str) -> String {
    if let Some(abs) = new.strip_prefix('\\') {
        return abs.to_owned();
    }
    match old {
        Some(old) if !old.is_empty() => format!("{}\\{}", old.trim_end_matches('\\'), new),
        _ => new.to_owned(),
    }
}

/// A controller class given as a string (`'OrderController'`, `'\App\X'`).
fn resolve_string_class(class: &str, g: &Group) -> String {
    if let Some(abs) = class.strip_prefix('\\') {
        return abs.to_owned();
    }
    match &g.namespace {
        Some(ns) => format!("{ns}\\{class}"),
        None => class.to_owned(),
    }
}

/// `'Controller@method'`, `'InvokableController'`, or `'method'` inside `Route::controller()`.
fn string_action(s: &str, g: &Group) -> ActionInfo {
    let (class, method) = match s.split_once('@') {
        Some((c, m)) => (c, m),
        None => match &g.controller {
            Some(ctrl) if !s.contains('\\') => {
                return ActionInfo::controller(ctrl.clone(), s.to_owned());
            }
            _ => (s, "__invoke"),
        },
    };
    if class.starts_with('\\') || g.namespace.is_some() || class.contains('\\') {
        let fqn = if g.namespace.is_some() || class.starts_with('\\') {
            resolve_string_class(class, g)
        } else {
            class.to_owned()
        };
        return ActionInfo::controller(fqn, method.to_owned());
    }
    let mut info = ActionInfo::unresolved(
        Some(s.to_owned()),
        format!("string action '{s}' needs a controller namespace (none is set for this group)"),
    );
    info.method = Some(method.to_owned());
    info
}

fn string_map(items: &[(Option<String>, Val<'_>)]) -> BTreeMap<String, String> {
    items
        .iter()
        .filter_map(|(k, v)| Some((k.clone()?, v.as_str()?.to_owned())))
        .collect()
}

fn apply_resource_options(spec: &mut ResourceSpec, opts: &[(Option<String>, Val<'_>)]) {
    for (key, v) in opts {
        match (key.as_deref(), v) {
            (Some("only"), v) => spec.only = v.strings(),
            (Some("except"), v) => spec.except = v.strings().unwrap_or_default(),
            (Some("names"), Val::Str(base)) => spec.names = Names::Base(base.clone()),
            (Some("names"), Val::Arr(map)) => spec.names = Names::Map(string_map(map)),
            (Some("parameters"), Val::Arr(map)) => spec.parameters.extend(string_map(map)),
            (Some("as"), Val::Str(a)) => spec.as_prefix = Some(a.clone()),
            (Some("middleware"), v) => spec.middleware.extend(v.strings().unwrap_or_default()),
            (Some("shallow"), Val::Bool(b)) => spec.shallow = *b,
            (Some("creatable"), Val::Bool(b)) => spec.creatable = *b,
            (Some("destroyable"), Val::Bool(b)) => spec.destroyable = *b,
            _ => {}
        }
    }
}

/// The literal `$namespace` property of a class body, if any.
fn namespace_property(body: Node<'_>, src: &[u8]) -> Option<String> {
    for member in named_children(body) {
        if member.kind() != "property_declaration" {
            continue;
        }
        for el in named_children(member) {
            if el.kind() != "property_element" {
                continue;
            }
            let name = el.child_by_field_name("name").map(|n| text(n, src));
            if name != Some("$namespace") {
                continue;
            }
            let value = el.child_by_field_name("default_value")?;
            return match value.kind() {
                "string" => Some(unquote_single(text(value, src))),
                "encapsed_string" => Some(unquote_double(text(value, src))),
                _ => None,
            };
        }
    }
    None
}

fn find_call<'t>(node: Node<'t>, name: &str, src: &[u8]) -> Option<Node<'t>> {
    if node.kind() == "member_call_expression"
        && node.child_by_field_name("name").map(|n| text(n, src)) == Some(name)
    {
        return Some(node);
    }
    named_children(node)
        .into_iter()
        .find_map(|c| find_call(c, name, src))
}

/// Resolves `.` and `..` without touching the filesystem.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_and_namespaces() {
        assert_eq!(join_prefix("", "api"), "api");
        assert_eq!(join_prefix("api/", "/v1"), "api/v1");
        assert_eq!(join_prefix("api", ""), "api");
        assert_eq!(
            merge_namespace(None, "App\\Http\\Controllers"),
            "App\\Http\\Controllers"
        );
        assert_eq!(
            merge_namespace(Some("App\\Http\\Controllers"), "Admin"),
            "App\\Http\\Controllers\\Admin"
        );
        assert_eq!(merge_namespace(Some("App"), "\\Other"), "Other");
    }

    #[test]
    fn string_quoting() {
        assert_eq!(
            unquote_single(r"'Admin\ReportController'"),
            r"Admin\ReportController"
        );
        assert_eq!(unquote_single(r"'it\'s'"), "it's");
        assert_eq!(unquote_double("\"a\\tb\""), "a\tb");
        assert_eq!(
            normalize(Path::new("/w/routes/../routes/./api.php")),
            PathBuf::from("/w/routes/api.php")
        );
    }
}
