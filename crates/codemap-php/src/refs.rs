//! References from PHP code, for "find usages": classes (new, static calls,
//! `X::class`, type hints, instanceof, catch, extends/implements), methods
//! (`X::m()`, `$this->m()`, `$typed->m()`), functions, route names
//! (`route('x')`) and HTTP calls in tests (`$this->getJson('/api/x')`).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tree_sitter::Node;

use crate::Scope;
use crate::ast::{apply_use_declaration, named_children, start_line, text};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ref {
    /// `class`, `call`, `call_unknown` (receiver type unknown), `function`, `route`, `http`.
    pub kind: &'static str,
    /// `App\Models\Order`, `App\Models\Order::save`, `save`, `orders.show`, `GET /api/orders`.
    pub target: String,
    pub line: u32,
}

const HTTP_TEST_METHODS: &[(&str, &str)] = &[
    ("get", "GET"),
    ("getJson", "GET"),
    ("post", "POST"),
    ("postJson", "POST"),
    ("put", "PUT"),
    ("putJson", "PUT"),
    ("patch", "PATCH"),
    ("patchJson", "PATCH"),
    ("delete", "DELETE"),
    ("deleteJson", "DELETE"),
    ("options", "OPTIONS"),
    ("optionsJson", "OPTIONS"),
];

struct Collector<'s> {
    src: &'s [u8],
    scope: Scope,
    class: Option<String>,
    parent: Option<String>,
    /// Variable → class, in the current function.
    vars: HashMap<String, String>,
    /// Methods of the current class that pass their parameters straight to a
    /// settings read: name → (key parameter, default parameter).
    /// Same-class methods wrapping a setting read: key parameter, default
    /// parameter, and how the default is built from it (`$this->configValue({})`).
    wrappers: HashMap<String, (usize, Option<usize>, Option<String>)>,
    out: Vec<Ref>,
}

pub fn collect(root: Node<'_>, src: &[u8]) -> Vec<Ref> {
    let mut c = Collector {
        src,
        scope: Scope::default(),
        class: None,
        parent: None,
        vars: HashMap::new(),
        wrappers: HashMap::new(),
        out: Vec::new(),
    };
    c.walk(root);
    c.out
}

fn string_lit(n: Node<'_>, src: &[u8]) -> Option<String> {
    let n = if n.kind() == "argument" {
        named_children(n).into_iter().last()?
    } else {
        n
    };
    match n.kind() {
        "string" | "encapsed_string" => {
            let t = text(n, src);
            let inner = t.get(1..t.len().saturating_sub(1))?;
            (!inner.contains('$')).then(|| inner.to_owned())
        }
        _ => None,
    }
}

/// A URL string; interpolations (`{$mall->slug}`, `$id`) become `{}`.
fn url_lit(n: Node<'_>, src: &[u8]) -> Option<String> {
    let n = if n.kind() == "argument" {
        named_children(n).into_iter().last()?
    } else {
        n
    };
    // `'/api/v1/brands?sort=popular' . $near`, `'/api/v1/brands/' . $brand->id`
    if n.kind() == "binary_expression"
        && n.child_by_field_name("operator").map(|o| text(o, src)) == Some(".")
    {
        let left = n.child_by_field_name("left")?;
        let right = n.child_by_field_name("right")?;
        let l = url_lit(left, src)?;
        let r = url_lit(right, src).unwrap_or_else(|| "{}".to_owned());
        return Some(format!("{l}{r}"));
    }
    if n.kind() == "parenthesized_expression" {
        return url_lit(named_children(n).into_iter().next()?, src);
    }
    if !matches!(n.kind(), "string" | "encapsed_string") {
        return None;
    }
    let t = text(n, src);
    let inner = t.get(1..t.len().saturating_sub(1))?;
    let mut out = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'$') => {
                for x in chars.by_ref() {
                    if x == '}' {
                        break;
                    }
                }
                out.push_str("{}");
            }
            '$' => {
                while chars
                    .peek()
                    .is_some_and(|x| x.is_alphanumeric() || *x == '_')
                {
                    chars.next();
                }
                out.push_str("{}");
            }
            c => out.push(c),
        }
    }
    Some(out)
}

/// The `i`-th argument node of a call.
fn nth_arg(n: Node<'_>, i: usize) -> Option<Node<'_>> {
    n.child_by_field_name("arguments")
        .map(named_children)
        .unwrap_or_default()
        .into_iter()
        .filter(|a| a.kind() == "argument")
        .nth(i)
}

/// Test helpers that set up a request and return the test case, so the
/// request after them still comes from the test: `$this->actingAs($u)->postJson(…)`.
const TEST_REQUEST_SETUP: &[&str] = &[
    "actingAs",
    "be",
    "withHeaders",
    "withHeader",
    "withToken",
    "withBasicAuth",
    "withoutToken",
    "withoutMiddleware",
    "withMiddleware",
    "withSession",
    "withCookie",
    "withCookies",
    "withUnencryptedCookie",
    "withUnencryptedCookies",
    "withServerVariables",
    "withoutExceptionHandling",
    "withExceptionHandling",
    "followingRedirects",
    "from",
    "fromRoute",
    "withCredentials",
    "withoutCredentials",
    "flushHeaders",
    "travel",
];

/// `$this` or `$this->actingAs($u)->withHeaders([...])`.
fn is_test_case(obj_text: &str) -> bool {
    let t = obj_text.trim();
    if t == "$this" {
        return true;
    }
    let Some(rest) = t.strip_prefix("$this->") else {
        return false;
    };
    let first = rest.split(['(', '-']).next().unwrap_or("");
    TEST_REQUEST_SETUP.contains(&first)
}

fn first_arg(n: Node<'_>) -> Option<Node<'_>> {
    n.child_by_field_name("arguments")
        .map(named_children)
        .unwrap_or_default()
        .into_iter()
        .find(|a| a.kind() == "argument")
}

impl Collector<'_> {
    fn push(&mut self, kind: &'static str, target: String, node: Node<'_>) {
        self.out.push(Ref {
            kind,
            target,
            line: start_line(node),
        });
    }

    /// `Setting::get(self::KEY, self::DEFAULT)` / `Setting::put('key', $v)` /
    /// `setting('key', 5)`: key and default as literals or `const:Class::NAME`.
    fn setting(&mut self, n: Node<'_>, method: &str) {
        const WRITE: &[&str] = &[
            "put", "set", "forget", "update", "store", "save", "remember", "delete",
        ];
        let write = WRITE.contains(&method);
        if !write && !SETTING_READS.contains(&method) {
            return; // `ListSettings::route('/')`, `Setting::query()`: not a setting
        }
        self.setting_at(n, 0, (!write).then_some(1), write);
    }

    /// Methods whose body reads a setting with their own parameters:
    /// `function days($key, $default) { return Setting::get($key, $default); }`.
    fn find_setting_wrappers(&mut self, class: Node<'_>) {
        let Some(body) = named_children(class)
            .into_iter()
            .find(|c| c.kind() == "declaration_list")
        else {
            return;
        };
        for m in named_children(body)
            .into_iter()
            .filter(|m| m.kind() == "method_declaration")
        {
            let Some(name) = m
                .child_by_field_name("name")
                .map(|x| text(x, self.src).to_owned())
            else {
                continue;
            };
            let params: Vec<String> = m
                .child_by_field_name("parameters")
                .map(named_children)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|p| {
                    p.child_by_field_name("name")
                        .map(|x| text(x, self.src).to_owned())
                })
                .collect();
            let mut calls = Vec::new();
            fn walk_calls<'t>(n: Node<'t>, out: &mut Vec<Node<'t>>) {
                for c in named_children(n) {
                    if c.kind() == "scoped_call_expression"
                        || c.kind() == "function_call_expression"
                    {
                        out.push(c);
                    }
                    walk_calls(c, out);
                }
            }
            if let Some(b) = m.child_by_field_name("body") {
                walk_calls(b, &mut calls);
            }
            for c in calls {
                let is_read = if c.kind() == "scoped_call_expression" {
                    let cls = c
                        .child_by_field_name("scope")
                        .and_then(|s| self.class_name(text(s, self.src)));
                    let method = c
                        .child_by_field_name("name")
                        .map(|x| text(x, self.src))
                        .unwrap_or("");
                    cls.is_some_and(|f| f.rsplit('\\').next().unwrap_or("").contains("Setting"))
                        && SETTING_READS.contains(&method)
                } else {
                    matches!(
                        c.child_by_field_name("function").map(|f| text(f, self.src)),
                        Some("setting" | "settings")
                    )
                };
                if !is_read {
                    continue;
                }
                let args: Vec<String> = c
                    .child_by_field_name("arguments")
                    .map(named_children)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|a| a.kind() == "argument")
                    .filter_map(|a| named_children(a).into_iter().last())
                    .map(|a| text(a, self.src).to_owned())
                    .collect();
                let pos = |i: usize| args.get(i).and_then(|a| params.iter().position(|p| p == a));
                if let Some(k) = pos(0) {
                    // The default may be a parameter used as the fallback, or an
                    // expression built from one (`?? $this->configValue($configKey)`).
                    let mut tpl = None;
                    let default = pos(1).or_else(|| {
                        let f = fallback_of(c, self.src)?;
                        let ft = text(strip_casts(f), self.src);
                        if let Some(i) = params.iter().position(|p| p == ft) {
                            return Some(i);
                        }
                        let i = params.iter().position(|p| contains_var(ft, p))?;
                        tpl = Some(crate::ast::compact(ft).replace(params[i].as_str(), "{}"));
                        Some(i)
                    });
                    // The wrapper's own read has a fixed default (`Setting::get($key, '{}')`,
                    // `return $stored !== null && …`): every caller gets it.
                    if default.is_none() && tpl.is_none() {
                        let fixed = args
                            .get(1)
                            .filter(|a| !params.iter().any(|p| p == *a))
                            .map(|a| crate::ast::compact(a))
                            .or_else(|| {
                                fallback_of(c, self.src)
                                    .map(|f| crate::ast::compact(text(f, self.src)))
                            })
                            .or_else(|| unset_means_false(c, self.src).then(|| "false".to_owned()));
                        if let Some(f) = fixed {
                            tpl = Some(format!("={f}"));
                        }
                    }
                    self.wrappers.insert(name.clone(), (k, default, tpl));
                }
            }
        }
    }

    fn setting_at(&mut self, n: Node<'_>, key_idx: usize, default_idx: Option<usize>, write: bool) {
        self.setting_at_with(n, key_idx, default_idx, write, None);
    }

    /// `tpl`: how a wrapper builds the default from the argument (`{}` = it).
    fn setting_at_with(
        &mut self,
        n: Node<'_>,
        key_idx: usize,
        default_idx: Option<usize>,
        write: bool,
        tpl: Option<&str>,
    ) {
        let args: Vec<Node<'_>> = n
            .child_by_field_name("arguments")
            .map(named_children)
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a.kind() == "argument")
            .filter_map(|a| named_children(a).into_iter().last())
            .collect();
        let value = |x: Node<'_>| -> Option<String> {
            match x.kind() {
                "string" | "encapsed_string" => string_lit(x, self.src),
                "class_constant_access_expression" => {
                    let parts = named_children(x);
                    let cls = parts
                        .first()
                        .and_then(|c| self.class_name(text(*c, self.src)))?;
                    let name = parts.get(1).map(|c| text(*c, self.src))?;
                    Some(format!("const:{cls}::{name}"))
                }
                "integer" | "float" | "boolean" | "null" => Some(text(x, self.src).to_owned()),
                _ => Some(format!(
                    "expr:{}",
                    crate::ast::truncate(&crate::ast::compact(text(x, self.src)), 60)
                )),
            }
        };
        let Some(key) = args.get(key_idx).and_then(|a| value(*a)) else {
            return;
        };
        if key.starts_with("expr:") {
            return;
        }
        // A wrapper with a fixed default (`=…`): the same for every caller.
        let fixed = tpl.and_then(|t| t.strip_prefix('=')).map(|f| match f {
            "false" | "true" | "null" => f.to_owned(),
            _ if f.parse::<f64>().is_ok() => f.to_owned(),
            _ => format!("expr:{f}"),
        });
        let tpl = tpl.filter(|t| !t.starts_with('='));
        let default = fixed
            .or_else(|| {
                default_idx
                    .and_then(|i| args.get(i))
                    .and_then(|a| match tpl {
                        Some(t) => Some(format!(
                            "expr:{}",
                            t.replace("{}", &crate::ast::compact(text(*a, self.src)))
                        )),
                        None => value(*a),
                    })
            })
            .or_else(|| {
                if write {
                    None
                } else {
                    fallback_of(n, self.src)
                        .and_then(value)
                        .or_else(|| unset_means_false(n, self.src).then(|| "false".to_owned()))
                }
            });
        let target = match default {
            Some(d) => format!("{key}\t{d}"),
            None => key,
        };
        self.push(
            if write {
                "setting_write"
            } else {
                "setting_read"
            },
            target,
            n,
        );
    }

    /// A cache key: a literal, `const:Class::NAME`, or a pattern with `{}`
    /// for variable parts (`"brands:{$id}"`, `'menu.' . $outlet->id`).
    fn cache_key(&self, a: Node<'_>) -> Option<String> {
        let a = if a.kind() == "argument" {
            named_children(a).into_iter().last()?
        } else {
            a
        };
        let key = self.cache_key_parts(a, 0)?;
        (!key.is_empty() && key != "{}" && !key.chars().all(|c| c == '{' || c == '}'))
            .then_some(key)
    }

    /// Key text with constants as `«const:Class::NAME»` (resolved when shown)
    /// and variable parts as `{}`.
    fn cache_key_parts(&self, a: Node<'_>, depth: u8) -> Option<String> {
        if depth > 8 {
            return None;
        }
        match a.kind() {
            "class_constant_access_expression" => {
                let parts = named_children(a);
                let cls = parts
                    .first()
                    .and_then(|c| self.class_name(text(*c, self.src)))?;
                let name = parts.get(1).map(|c| text(*c, self.src))?;
                Some(format!("«const:{cls}::{name}»"))
            }
            "binary_expression"
                if a.child_by_field_name("operator").map(|o| text(o, self.src)) == Some(".") =>
            {
                let l = a
                    .child_by_field_name("left")
                    .and_then(|x| self.cache_key_parts(x, depth + 1))
                    .unwrap_or_else(|| "{}".into());
                let r = a
                    .child_by_field_name("right")
                    .and_then(|x| self.cache_key_parts(x, depth + 1))
                    .unwrap_or_else(|| "{}".into());
                Some(format!("{l}{r}"))
            }
            "parenthesized_expression" => {
                self.cache_key_parts(named_children(a).into_iter().next()?, depth + 1)
            }
            "string" | "encapsed_string" => url_lit(a, self.src),
            _ => Some("{}".into()),
        }
    }

    /// `remember`, `put`, `get`, `forget`, `flush`… on the cache.
    fn cache_op(&mut self, n: Node<'_>, method: &str, tags: Option<String>) {
        let prefix = tags.map(|t| format!("tags:{t}|")).unwrap_or_default();
        let key = || first_arg(n).and_then(|a| self.cache_key(a));
        let (kind, ttl_arg) = match method {
            "remember" | "flexible" | "add" | "increment" | "decrement" => ("cache_write", Some(1)),
            "put" | "set" => ("cache_write", Some(2)),
            "rememberForever" | "sear" | "forever" => ("cache_write", None),
            "get" | "has" | "missing" | "many" => ("cache_read", None),
            "pull" | "forget" | "delete" => ("cache_forget", None),
            "flush" | "clear" => {
                let target = if prefix.is_empty() {
                    "*".to_owned()
                } else {
                    prefix.trim_end_matches('|').to_owned()
                };
                self.push("cache_flush", target, n);
                return;
            }
            _ => return,
        };
        let Some(k) = key() else {
            return;
        };
        let key = format!("{prefix}{k}");
        if kind == "cache_write" {
            let ttl = match ttl_arg {
                Some(i) => arg_text(n, i, self.src).unwrap_or_else(|| "?".into()),
                None => "forever".to_owned(),
            };
            self.push(kind, format!("{key}\t{ttl}"), n);
        } else {
            if method == "pull" {
                self.push("cache_read", key.clone(), n);
            }
            self.push(kind, key, n);
        }
    }

    fn class_name(&self, raw: &str) -> Option<String> {
        match raw.to_ascii_lowercase().as_str() {
            "self" | "static" => self.class.clone(),
            "parent" => self.parent.clone(),
            _ => self.scope.resolve_class(raw),
        }
    }

    /// The class a service-container lookup returns: `app(X::class)`,
    /// `resolve(X::class)`, `App::make(X::class)`, `app()->make(X::class)`,
    /// `$this->app->make(X::class)` (and `makeWith` / `get`).
    fn container_class(&self, n: Node<'_>) -> Option<String> {
        let t = crate::ast::compact(text(n, self.src));
        let t = t.trim_start_matches('(').trim_end_matches(')');
        let t = t.trim_start_matches('\\');
        let rest = [
            "app(",
            "resolve(",
            "App::make(",
            "App::makeWith(",
            "app()->make(",
            "app()->makeWith(",
            "app()->get(",
            "$this->app->make(",
            "$this->app->makeWith(",
            "$this->app->get(",
            "Container::getInstance()->make(",
        ]
        .iter()
        .find_map(|p| t.strip_prefix(p))?;
        let arg = rest.split([',', ')']).next()?;
        let class = arg.strip_suffix("::class")?;
        self.class_name(class)
    }

    fn type_refs(&mut self, n: Node<'_>) {
        if matches!(n.kind(), "named_type" | "name" | "qualified_name") {
            let raw = text(n, self.src).to_owned();
            if let Some(fqn) = self.scope.resolve_class(&raw) {
                self.push("class", fqn, n);
            }
            return;
        }
        for c in named_children(n) {
            self.type_refs(c);
        }
    }

    fn function_like(&mut self, n: Node<'_>) {
        let saved = std::mem::take(&mut self.vars);
        if let Some(params) = n.child_by_field_name("parameters") {
            for p in named_children(params) {
                let ty = p.child_by_field_name("type");
                let name = p
                    .child_by_field_name("name")
                    .map(|x| text(x, self.src).to_owned());
                if let Some(t) = ty {
                    self.type_refs(t);
                    if let (Some(name), Some(fqn)) = (
                        name,
                        self.scope
                            .resolve_class(text(t, self.src).trim_start_matches('?')),
                    ) {
                        self.vars.insert(name, fqn);
                    }
                }
            }
        }
        if let Some(r) = n.child_by_field_name("return_type") {
            self.type_refs(r);
        }
        if let Some(body) = n.child_by_field_name("body") {
            self.walk(body);
        }
        self.vars = saved;
    }

    fn walk(&mut self, n: Node<'_>) {
        for c in named_children(n) {
            self.node(c);
        }
    }

    fn node(&mut self, n: Node<'_>) {
        match n.kind() {
            "namespace_definition" => {
                let name = n
                    .child_by_field_name("name")
                    .map(|x| text(x, self.src).to_owned())
                    .unwrap_or_default();
                match n.child_by_field_name("body") {
                    Some(body) => {
                        let saved = self.scope.clone();
                        self.scope.set_namespace(&name);
                        self.walk(body);
                        self.scope = saved;
                    }
                    None => self.scope.set_namespace(&name),
                }
            }
            "namespace_use_declaration" => apply_use_declaration(n, self.src, &mut self.scope),
            "class_declaration"
            | "interface_declaration"
            | "trait_declaration"
            | "enum_declaration" => {
                let name = n
                    .child_by_field_name("name")
                    .map(|x| text(x, self.src).to_owned())
                    .unwrap_or_default();
                let saved = (self.class.clone(), self.parent.clone());
                let saved_wrappers = std::mem::take(&mut self.wrappers);
                self.class = Some(self.scope.qualify(&name));
                self.parent = None;
                self.find_setting_wrappers(n);
                for c in named_children(n) {
                    match c.kind() {
                        "base_clause" | "class_interface_clause" => {
                            for t in named_children(c) {
                                if let Some(fqn) = self.scope.resolve_class(text(t, self.src)) {
                                    if c.kind() == "base_clause" {
                                        self.parent = Some(fqn.clone());
                                    }
                                    self.push("class", fqn, t);
                                }
                            }
                        }
                        "declaration_list" | "enum_declaration_list" => self.walk(c),
                        _ => {}
                    }
                }
                (self.class, self.parent) = saved;
                self.wrappers = saved_wrappers;
            }
            "use_declaration" => {
                for t in named_children(n) {
                    if matches!(t.kind(), "name" | "qualified_name")
                        && let Some(fqn) = self.scope.resolve_class(text(t, self.src))
                    {
                        self.push("class", fqn, t);
                    }
                }
            }
            "method_declaration"
            | "function_definition"
            | "anonymous_function"
            | "arrow_function" => {
                if n.kind() == "anonymous_function" || n.kind() == "arrow_function" {
                    // Closures see the enclosing variables, plus their typed parameters.
                    let saved = self.vars.clone();
                    if let Some(params) = n.child_by_field_name("parameters") {
                        for p in named_children(params) {
                            if let Some(t) = p.child_by_field_name("type") {
                                self.type_refs(t);
                                if let (Some(name), Some(fqn)) = (
                                    p.child_by_field_name("name")
                                        .map(|x| text(x, self.src).to_owned()),
                                    self.scope
                                        .resolve_class(text(t, self.src).trim_start_matches('?')),
                                ) {
                                    self.vars.insert(name, fqn);
                                }
                            }
                        }
                    }
                    if let Some(body) = n.child_by_field_name("body") {
                        self.node(body);
                    }
                    self.vars = saved;
                } else {
                    self.function_like(n);
                }
            }
            "property_declaration" => {
                if let Some(t) = n.child_by_field_name("type") {
                    self.type_refs(t);
                }
                self.walk(n);
            }
            "object_creation_expression" => {
                if let Some(cls) = named_children(n)
                    .into_iter()
                    .find(|c| matches!(c.kind(), "name" | "qualified_name" | "relative_name"))
                    && let Some(fqn) = self.class_name(text(cls, self.src))
                {
                    self.push("class", fqn, n);
                }
                self.walk(n);
            }
            "assignment_expression" => {
                // `$order = new Order(...)` types `$order` for the rest of the function.
                if let (Some(left), Some(right)) = (
                    n.child_by_field_name("left"),
                    n.child_by_field_name("right"),
                ) && left.kind() == "variable_name"
                    && right.kind() == "object_creation_expression"
                    && let Some(cls) = named_children(right)
                        .into_iter()
                        .find(|c| matches!(c.kind(), "name" | "qualified_name" | "relative_name"))
                    && let Some(fqn) = self.class_name(text(cls, self.src))
                {
                    self.vars.insert(text(left, self.src).to_owned(), fqn);
                } else if let (Some(left), Some(right)) = (
                    n.child_by_field_name("left"),
                    n.child_by_field_name("right"),
                ) && left.kind() == "variable_name"
                    && let Some(fqn) = self.container_class(right)
                {
                    // `$resolver = app(CoordinateResolver::class)` types it too.
                    self.vars.insert(text(left, self.src).to_owned(), fqn);
                }
                self.walk(n);
            }
            "scoped_call_expression"
            | "class_constant_access_expression"
            | "scoped_property_access_expression" => {
                let scope_node = n
                    .child_by_field_name("scope")
                    .or_else(|| named_children(n).into_iter().next());
                if let Some(s) = scope_node
                    && matches!(
                        s.kind(),
                        "name" | "qualified_name" | "relative_name" | "relative_scope"
                    )
                    && let Some(fqn) = self.class_name(text(s, self.src))
                {
                    if n.kind() == "scoped_call_expression"
                        && let Some(m) = n.child_by_field_name("name")
                    {
                        let method = text(m, self.src).to_owned();
                        // `Artisan::call('orders:expire')`, `Schedule::command('x')`.
                        let short_scope = fqn.rsplit('\\').next().unwrap_or(&fqn);
                        if ((short_scope == "Artisan"
                            && matches!(method.as_str(), "call" | "queue"))
                            || (short_scope == "Schedule" && method == "command"))
                            && let Some(cmd) = first_arg(n).and_then(|a| string_lit(a, self.src))
                            && let Some(cmd) = cmd.split_whitespace().next()
                        {
                            self.push("command", cmd.to_owned(), n);
                        }
                        self.push("call", format!("{fqn}::{method}"), n);
                        let short = fqn.rsplit('\\').next().unwrap_or(&fqn);
                        if short == "Cache" {
                            self.cache_op(n, &method, None);
                        }
                        if short.contains("Setting") {
                            self.setting(n, &method);
                        } else if self.class.as_deref() == Some(fqn.as_str())
                            && let Some((k, d, tpl)) = self.wrappers.get(&method).cloned()
                        {
                            self.setting_at_with(n, k, d, false, tpl.as_deref());
                        }
                    }
                    self.push("class", fqn, n);
                }
                self.walk(n);
            }
            "member_call_expression" | "nullsafe_member_call_expression" => {
                let name = n
                    .child_by_field_name("name")
                    .map(|x| text(x, self.src).to_owned())
                    .unwrap_or_default();
                let obj = n.child_by_field_name("object");
                let obj_text = obj
                    .map(|o| text(o, self.src).to_owned())
                    .unwrap_or_default();
                // Tests: `$this->getJson('/api/orders')`, also after set-up
                // calls (`$this->actingAs($u)->postJson(…)`).
                let test_case = is_test_case(&obj_text);
                if test_case
                    && let Some((_, verb)) = HTTP_TEST_METHODS.iter().find(|(m, _)| *m == name)
                    && let Some(url) = first_arg(n).and_then(|a| url_lit(a, self.src))
                    && (url.starts_with('/') || url.contains('/'))
                {
                    self.push("http", format!("{verb} {url}"), n);
                }
                // `$this->call('POST', '/api/webhooks', …)` / `$this->json('PATCH', '/x')`.
                if test_case
                    && matches!(name.as_str(), "call" | "json")
                    && let Some(verb) = first_arg(n).and_then(|a| string_lit(a, self.src))
                    && let Some(url) = nth_arg(n, 1).and_then(|a| url_lit(a, self.src))
                    && (url.starts_with('/') || url.contains('/'))
                {
                    self.push("http", format!("{} {url}", verb.to_uppercase()), n);
                }
                // `$this->artisan('orders:expire', …)`, `$schedule->command('x')`.
                if (name == "artisan" && test_case
                    || name == "command" && obj_text.contains("schedule"))
                    && let Some(cmd) = first_arg(n).and_then(|a| string_lit(a, self.src))
                    && let Some(cmd) = cmd.split_whitespace().next()
                {
                    self.push("command", cmd.to_owned(), n);
                }
                if name == "route"
                    && let Some(r) = first_arg(n).and_then(|a| string_lit(a, self.src))
                {
                    self.push("route", r, n);
                }
                // `Cache::tags(['brands'])->remember(…)`, `Cache::store('redis')->get(…)`, `cache()->forget(…)`.
                if let Some(o) = obj {
                    let ot = crate::ast::compact(&obj_text);
                    let tags = (ot.starts_with("Cache::tags(") || ot.starts_with("\\Cache::tags("))
                        .then(|| cache_tags(o, self.src));
                    if tags.is_some()
                        || ot.starts_with("Cache::store(")
                        || ot.starts_with("\\Cache::store(")
                        || ot == "cache()"
                    {
                        self.cache_op(n, &name, tags);
                    }
                }
                if obj_text == "$this"
                    && let Some((k, d, tpl)) = self.wrappers.get(&name).cloned()
                {
                    self.setting_at_with(n, k, d, false, tpl.as_deref());
                }
                let receiver = match obj_text.as_str() {
                    "$this" => self.class.clone(),
                    v => self
                        .vars
                        .get(v)
                        .cloned()
                        // `app(X::class)->m()`, `resolve(X::class)->m()`, …
                        .or_else(|| obj.and_then(|o| self.container_class(o))),
                };
                match receiver {
                    Some(cls) if !name.is_empty() => self.push("call", format!("{cls}::{name}"), n),
                    _ if !name.is_empty() => self.push("call_unknown", name, n),
                    _ => {}
                }
                self.walk(n);
            }
            "function_call_expression" => {
                let f = n
                    .child_by_field_name("function")
                    .map(|x| text(x, self.src).to_owned())
                    .unwrap_or_default();
                let f = f.trim_start_matches('\\');
                if matches!(f, "setting" | "settings") {
                    self.setting(n, "get");
                }
                // `cache('key')` reads; `cache(['key' => $v], $ttl)` writes.
                if f == "cache"
                    && let Some(a) = first_arg(n)
                {
                    let a = named_children(a).into_iter().last().unwrap_or(a);
                    if a.kind() == "array_creation_expression" {
                        if let Some(el) = named_children(a).into_iter().next()
                            && let Some(k) = named_children(el).into_iter().next()
                            && let Some(key) = self.cache_key(k)
                        {
                            let ttl = arg_text(n, 1, self.src).unwrap_or_else(|| "?".into());
                            self.push("cache_write", format!("{key}\t{ttl}"), n);
                        }
                    } else if let Some(key) = self.cache_key(a) {
                        self.push("cache_read", key, n);
                    }
                }
                // `config('shop.radius_km')` / `env('SHOP_RADIUS')`: which keys code reads.
                if matches!(f, "config" | "env")
                    && let Some(k) = first_arg(n).and_then(|a| string_lit(a, self.src))
                {
                    self.push(if f == "config" { "config" } else { "env" }, k, n);
                }
                if matches!(f, "route" | "to_route" | "url_for")
                    && let Some(r) = first_arg(n).and_then(|a| string_lit(a, self.src))
                {
                    self.push("route", r, n);
                } else if !f.is_empty() && !f.starts_with('$') && !f.contains(['(', '-', ':']) {
                    // Unqualified calls fall back to the global function (helpers).
                    let resolved = if f.contains('\\') {
                        self.scope.resolve_function(f)
                    } else {
                        f.to_owned()
                    };
                    self.push("function", resolved, n);
                }
                self.walk(n);
            }
            "binary_expression" => {
                // `$x instanceof Order`
                if let Some(op) = n.child_by_field_name("operator")
                    && text(op, self.src) == "instanceof"
                    && let Some(r) = n.child_by_field_name("right")
                    && matches!(r.kind(), "name" | "qualified_name")
                    && let Some(fqn) = self.class_name(text(r, self.src))
                {
                    self.push("class", fqn, n);
                }
                self.walk(n);
            }
            "catch_clause" => {
                if let Some(t) = n.child_by_field_name("type") {
                    self.type_refs(t);
                }
                self.walk(n);
            }
            _ => self.walk(n),
        }
    }
}

fn strip_casts(n: Node<'_>) -> Node<'_> {
    let mut n = n;
    while matches!(n.kind(), "parenthesized_expression" | "cast_expression") {
        match named_children(n).into_iter().last() {
            Some(c) => n = c,
            None => break,
        }
    }
    n
}

/// The fallback a settings read gets when nothing is stored:
/// `Setting::get(K) ?? X`, or `$v = Setting::get(K); … cond($v) ? $v : X`.
fn fallback_of<'t>(read: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    // Walk up through casts / parentheses.
    let mut cur = read;
    let mut parent = cur.parent()?;
    while matches!(
        parent.kind(),
        "parenthesized_expression" | "cast_expression"
    ) {
        cur = parent;
        parent = parent.parent()?;
    }
    // `Setting::get(K) === self::MODE_X ? self::MODE_X : self::MODE_Y`: when
    // unset the comparison fails, so the `else` branch is the default.
    if parent.kind() == "binary_expression"
        && matches!(
            parent.child_by_field_name("operator").map(|o| text(o, src)),
            Some("===" | "==")
        )
        && let Some(cond) = parent.parent()
        && cond.kind() == "conditional_expression"
        && cond.child_by_field_name("condition").map(|c| c.id()) == Some(parent.id())
        && let Some(alt) = cond.child_by_field_name("alternative")
    {
        return Some(alt);
    }
    let mut coalesce = None;
    if parent.kind() == "binary_expression"
        && parent.child_by_field_name("operator").map(|o| text(o, src)) == Some("??")
        && parent.child_by_field_name("left").map(|l| l.id()) == Some(cur.id())
    {
        let right = parent.child_by_field_name("right")?;
        // `?? ''` / `?? null` only marks "missing"; the real default may follow.
        if !matches!(text(right, src).trim(), "''" | "\"\"" | "null" | "[]") {
            return Some(right);
        }
        coalesce = Some(right);
        cur = parent;
        parent = parent.parent()?;
        while matches!(
            parent.kind(),
            "parenthesized_expression" | "cast_expression"
        ) {
            cur = parent;
            parent = parent.parent()?;
        }
    }
    let _ = cur;
    if parent.kind() != "assignment_expression" {
        return coalesce;
    }
    let var = parent
        .child_by_field_name("left")
        .filter(|l| l.kind() == "variable_name")
        .map(|l| text(l, src))?;
    // The enclosing function body.
    let mut body = parent;
    while !matches!(body.kind(), "compound_statement")
        || !body
            .parent()
            .is_some_and(|p| matches!(p.kind(), "method_declaration" | "function_definition"))
    {
        match body.parent() {
            Some(p) => body = p,
            None => return coalesce,
        }
    }
    let mut stack = vec![body];
    while let Some(n) = stack.pop() {
        // `return $stored ?? X;` later in the body.
        if n.kind() == "binary_expression"
            && n.start_byte() > parent.start_byte()
            && n.child_by_field_name("operator").map(|o| text(o, src)) == Some("??")
            && n.child_by_field_name("left")
                .is_some_and(|l| text(strip_casts(l), src) == var)
            && let Some(right) = n.child_by_field_name("right")
        {
            return Some(right);
        }
        // `if ($stored !== null) { return $stored; }` then the fallback: the
        // next statement's value (`$value = config(…)` / `return …`).
        if n.kind() == "if_statement"
            && n.start_byte() > parent.start_byte()
            && let Some(cond) = n.child_by_field_name("condition")
            && is_set_test(text(cond, src), var)
            && n.child_by_field_name("alternative").is_none()
        {
            let mut next = n.next_named_sibling();
            while let Some(st) = next {
                if st.kind() == "comment" {
                    next = st.next_named_sibling();
                    continue;
                }
                let value = match st.kind() {
                    "expression_statement" => named_children(st).into_iter().next().map(|e| {
                        if e.kind() == "assignment_expression" {
                            e.child_by_field_name("right").unwrap_or(e)
                        } else {
                            e
                        }
                    }),
                    "return_statement" => named_children(st).into_iter().next(),
                    _ => None,
                };
                if let Some(v) = value {
                    return Some(v);
                }
                break;
            }
        }
        // `if ($stored === null) { return X; }`: X is the default.
        if n.kind() == "if_statement"
            && n.start_byte() > parent.start_byte()
            && let Some(cond) = n.child_by_field_name("condition")
            && is_null_test(text(cond, src), var)
            && let Some(ret) = n.child_by_field_name("body").and_then(|b| first_return(b))
        {
            return Some(ret);
        }
        if n.kind() == "conditional_expression" && n.start_byte() > parent.start_byte() {
            let cond = n
                .child_by_field_name("condition")
                .map(|c| text(c, src))
                .unwrap_or("");
            let body_ = n.child_by_field_name("body");
            let alt = n.child_by_field_name("alternative");
            if cond.contains(var) {
                // The branch that does not use the stored value is the default.
                let uses = |x: Option<Node<'t>>| x.is_some_and(|b| text(b, src).contains(var));
                match (uses(body_), uses(alt)) {
                    (true, false) => return alt,
                    (false, true) => return body_,
                    _ => {}
                }
            }
        }
        stack.extend(named_children(n).into_iter().rev());
    }
    coalesce
}

/// The `i`th argument as written (compacted, at most 40 characters).
fn arg_text(n: Node<'_>, i: usize, src: &[u8]) -> Option<String> {
    let a = n
        .child_by_field_name("arguments")
        .map(named_children)
        .unwrap_or_default()
        .into_iter()
        .filter(|a| a.kind() == "argument")
        .nth(i)?;
    let a = named_children(a).into_iter().last().unwrap_or(a);
    Some(crate::ast::truncate(&crate::ast::compact(text(a, src)), 40))
}

/// `Cache::tags(['brands', 'menu'])` / `Cache::tags('brands')` → `brands,menu`.
fn cache_tags(member_call_object: Node<'_>, src: &[u8]) -> String {
    let mut out: Vec<String> = Vec::new();
    if let Some(a) = first_arg(member_call_object) {
        let a = named_children(a).into_iter().last().unwrap_or(a);
        if a.kind() == "array_creation_expression" {
            for el in named_children(a) {
                if let Some(v) = named_children(el).into_iter().last()
                    && let Some(t) = string_lit(v, src)
                {
                    out.push(t);
                }
            }
        } else if let Some(t) = string_lit(a, src) {
            out.push(t);
        }
    }
    if out.is_empty() {
        "{}".to_owned()
    } else {
        out.join(",")
    }
}

/// Methods of a settings class that read a value.
const SETTING_READS: &[&str] = &[
    "get", "value", "read", "first", "string", "int", "integer", "bool", "boolean", "float",
    "array", "json", "getValue", "fetch", "find", "lookup",
];

/// `$x === null`, `null === $x`, `is_null($x)`, `!$x`, `empty($x)`, `! isset($x)`.
fn is_null_test(cond: &str, var: &str) -> bool {
    let c: String = cond.chars().filter(|c| !c.is_whitespace()).collect();
    let c = c.trim_start_matches('(').trim_end_matches(')');
    // `$x === null || trim($x) === ''`: any alternative that tests for null.
    if c.contains("||") {
        return c.split("||").any(|part| is_null_test(part, var));
    }
    [
        format!("{var}===null"),
        format!("{var}==null"),
        format!("null==={var}"),
        format!("null=={var}"),
        format!("is_null({var})"),
        format!("!{var}"),
        format!("empty({var})"),
        format!("!isset({var})"),
    ]
    .iter()
    .any(|p| c == p.as_str())
}

/// `$x !== null`, `null !== $x`, `!is_null($x)`, `isset($x)`, `$x`.
fn is_set_test(cond: &str, var: &str) -> bool {
    let c: String = cond.chars().filter(|c| !c.is_whitespace()).collect();
    let c = c.trim_start_matches('(').trim_end_matches(')');
    [
        format!("{var}!==null"),
        format!("{var}!=null"),
        format!("null!=={var}"),
        format!("!is_null({var})"),
        format!("isset({var})"),
        var.to_owned(),
    ]
    .iter()
    .any(|p| c == p.as_str())
}

/// The expression of the first `return` in a block.
fn first_return(n: Node<'_>) -> Option<Node<'_>> {
    if n.kind() == "return_statement" {
        return named_children(n).into_iter().next();
    }
    named_children(n).into_iter().find_map(first_return)
}

/// `$stored = Setting::get(K); return $stored !== null && …;`: unset reads as false.
fn unset_means_false(read: Node<'_>, src: &[u8]) -> bool {
    let Some(assign) = read
        .parent()
        .filter(|p| p.kind() == "assignment_expression")
    else {
        return false;
    };
    let Some(var) = assign
        .child_by_field_name("left")
        .filter(|l| l.kind() == "variable_name")
        .map(|l| text(l, src).to_owned())
    else {
        return false;
    };
    let mut body = assign;
    while body.kind() != "compound_statement" {
        match body.parent() {
            Some(p) => body = p,
            None => return false,
        }
    }
    let after = &src[assign.end_byte()..body.end_byte().min(src.len())];
    let after: String = String::from_utf8_lossy(after)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    after.contains(&format!("return{var}!==null&&"))
        || after.contains(&format!("return{var}!=null&&"))
        || after.contains(&format!("return!is_null({var})&&"))
}

/// `$configKey` appears in an expression as a variable (not `$configKeys`).
fn contains_var(expr: &str, var: &str) -> bool {
    expr.match_indices(var).any(|(i, _)| {
        !expr[i + var.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

/// `route('x')` in Blade templates (not parsed as PHP).
pub fn blade_refs(src: &str) -> Vec<Ref> {
    let mut out = Vec::new();
    let b = src.as_bytes();
    let mut line = 1u32;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\n' {
            line += 1;
        } else if b[i] == b'r'
            && src[i..].starts_with("route(")
            && (i == 0
                || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_' || b[i - 1] == b'>'))
        {
            let arg = src[i + 6..].trim_start();
            if let Some(q) = arg.chars().next().filter(|c| matches!(c, '\'' | '"'))
                && let Some(end) = arg[1..].find(q)
            {
                out.push(Ref {
                    kind: "route",
                    target: arg[1..1 + end].to_owned(),
                    line,
                });
            }
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tests_reaching_routes_and_commands() {
        let src = br#"<?php
namespace Tests\Feature;

use Illuminate\Support\Facades\Artisan;

class WebhookTest extends TestCase
{
    private function send(string $raw)
    {
        return $this->call('POST', '/api/v1/webhooks/curlec', [], [], [], ['CONTENT_TYPE' => 'application/json'], $raw);
    }

    public function test_it(): void
    {
        $this->actingAs($this->user)->postJson('/api/v1/orders', []);
        $this->withHeaders(['X-A' => '1'])->get('/api/v1/malls');
        $this->json('PATCH', '/api/v1/carts/1');
        $this->artisan('credit:fold-card-refund', ['reference' => 'x'])->assertSuccessful();
        Artisan::call('orders:expire --dry');
    }
}
"#;
        let tree = crate::parse::parse(src).unwrap();
        let refs: Vec<String> = collect(tree.root_node(), src)
            .into_iter()
            .filter(|r| matches!(r.kind, "http" | "command"))
            .map(|r| format!("{} {}", r.kind, r.target))
            .collect();
        assert_eq!(
            refs,
            [
                "http POST /api/v1/webhooks/curlec",
                "http POST /api/v1/orders",
                "http GET /api/v1/malls",
                "http PATCH /api/v1/carts/1",
                "command credit:fold-card-refund",
                "command orders:expire",
            ]
        );
    }

    #[test]
    fn container_lookups_are_typed() {
        let src = br#"<?php
namespace App\Http\Controllers;

use App\Support\CoordinateResolver;
use App\Credit\CreditRefundToCard;

class GeoController
{
    public function show($query)
    {
        $resolver = app(CoordinateResolver::class);
        $found = $resolver->lookup($query);
        app(CreditRefundToCard::class)->move(1);
        resolve(CoordinateResolver::class)->near(2);
        $other = \App::make(CoordinateResolver::class);
        $other->far(3);
        $this->app->make(CreditRefundToCard::class)->undo();
    }
}
"#;
        let tree = crate::parse::parse(src).unwrap();
        let calls: Vec<String> = collect(tree.root_node(), src)
            .into_iter()
            .filter(|r| r.kind == "call")
            .map(|r| r.target)
            .collect();
        for want in [
            "App\\Support\\CoordinateResolver::lookup",
            "App\\Credit\\CreditRefundToCard::move",
            "App\\Support\\CoordinateResolver::near",
            "App\\Support\\CoordinateResolver::far",
            "App\\Credit\\CreditRefundToCard::undo",
        ] {
            assert!(calls.iter().any(|c| c == want), "{want} not in {calls:?}");
        }
    }

    #[test]
    fn php_references() {
        let src = br#"<?php
namespace App\Http\Controllers;

use App\Models\Order;
use App\Services\Billing as Bill;
use Illuminate\Http\Request;

class OrderController extends Controller implements HasMiddleware
{
    use Concerns\Audits;

    public function store(Request $request, Bill $billing): Order
    {
        $order = new Order($request->all());
        $order->save();
        $billing->charge($order);
        Order::query()->where('x', 1);
        $this->notify($order);
        $x->unknown();
        if ($order instanceof Order) { return redirect()->route('orders.show', $order); }
        money(5);
        $f = fn (Order $o) => $o->total();
        return Order::class;
    }
}
"#;
        let tree = crate::parse::parse(src).unwrap();
        let refs: Vec<String> = collect(tree.root_node(), src)
            .into_iter()
            .map(|r| format!("{} {} :{}", r.kind, r.target, r.line))
            .collect();
        for expected in [
            "class App\\Http\\Controllers\\Controller :8",
            "class App\\Http\\Controllers\\HasMiddleware :8",
            "class App\\Http\\Controllers\\Concerns\\Audits :10",
            "class Illuminate\\Http\\Request :12",
            "class App\\Services\\Billing :12",
            "class App\\Models\\Order :12",
            "class App\\Models\\Order :14",
            "call App\\Models\\Order::save :15",
            "call App\\Services\\Billing::charge :16",
            "call App\\Models\\Order::query :17",
            "call_unknown where :17",
            "call App\\Http\\Controllers\\OrderController::notify :18",
            "call_unknown unknown :19",
            "route orders.show :20",
            "function money :21",
            "call App\\Models\\Order::total :22",
        ] {
            assert!(
                refs.contains(&expected.to_owned()),
                "missing {expected} in {refs:#?}"
            );
        }
    }

    #[test]
    fn settings_reads_and_writes() {
        let src = br#"<?php
namespace App\Brands;
use App\Models\Setting;
class Popularity {
    public const KEY = 'popularity.window_days';
    public static function days() { return Setting::get(self::KEY, self::DEFAULT_DAYS); }
    public static function put($d) { Setting::put(self::KEY, (string) $d); }
    public static function x() { return setting('site.name', 'Shop') . Setting::get($dynamic); }
    public static function windowDays() { return self::days(self::KEY, 14); }
    public function type() { $stored = (string) (Setting::get('commission.type') ?? ''); return in_array($stored, ['a'], true) ? $stored : self::TYPE_FLAT; }
    public function rate() { $stored = Setting::get('commission.rate'); $r = $stored !== null ? (float) $stored : (float) config('x.rate'); return $r; }
    private static function days(string $key, int $default) { return (int) Setting::get($key, $default); }
}
"#;
        let tree = crate::parse::parse(src).unwrap();
        let refs: Vec<String> = collect(tree.root_node(), src)
            .into_iter()
            .filter(|r| r.kind.starts_with("setting"))
            .map(|r| format!("{} {} :{}", r.kind, r.target.replace('\t', " | "), r.line))
            .collect();
        assert_eq!(
            refs,
            [
                "setting_read const:App\\Brands\\Popularity::KEY | const:App\\Brands\\Popularity::DEFAULT_DAYS :6",
                "setting_write const:App\\Brands\\Popularity::KEY :7",
                "setting_read site.name | Shop :8",
                "setting_read const:App\\Brands\\Popularity::KEY | 14 :9",
                "setting_read commission.type | const:App\\Brands\\Popularity::TYPE_FLAT :10",
                "setting_read commission.rate | expr:(float) config('x.rate') :11",
            ]
        );
    }

    #[test]
    fn setting_defaults_in_later_statements() {
        let src = br#"<?php
namespace App\Config;
use App\Models\Setting;
class Shop {
    public function enabled(): bool { $stored = Setting::get('delivery.enabled'); return $stored !== null && filter_var($stored, FILTER_VALIDATE_BOOLEAN); }
    public function radius() { $stored = Setting::get('discovery.radius_km'); if ($stored === null) { return (float) config('shop.radius'); } return (float) $stored; }
    public function points() { return $this->read('membership.points', 'points_per_myr'); }
    private function read(string $settingKey, string $configKey): ?string { $stored = Setting::get($settingKey); if ($stored !== null) { return $stored; } $value = config('shop.'.$configKey); return $value; }
    public function title() { $s = Setting::get('welcome.title'); return $s ?? 'Hello'; }
    public static function pages() { return [ListSettings::route('/')]; }
    public function ladder() { $stored = Setting::get('delivery.ladder'); if ($stored === null || trim($stored) === '') { return self::DEFAULT_LADDER; } return $stored; }
    public function mode() { return Setting::get('delivery.mode') === self::MODE_IGNORE ? self::MODE_IGNORE : self::MODE_ASSUME; }
    public function geocoding() { return $this->enabled('maps.geocoding'); }
    public function title2() { return $this->messageMap('welcome.title2'); }
    private function enabled(string $key): bool { $stored = Setting::get($key); return $stored !== null && filter_var($stored, FILTER_VALIDATE_BOOLEAN); }
    private function messageMap(string $key): array { return json_decode((string) Setting::get($key, '{}'), true); }
}
"#;
        let tree = crate::parse::parse(src).unwrap();
        let refs: Vec<String> = collect(tree.root_node(), src)
            .into_iter()
            .filter(|r| r.kind == "setting_read")
            .map(|r| r.target.replace('\t', " | "))
            .collect();
        assert_eq!(
            refs,
            [
                "delivery.enabled | false",
                "discovery.radius_km | expr:(float) config('shop.radius')",
                "membership.points | expr:config('shop.'.'points_per_myr')",
                "welcome.title | Hello",
                "delivery.ladder | const:App\\Config\\Shop::DEFAULT_LADDER",
                "delivery.mode | const:App\\Config\\Shop::MODE_ASSUME",
                "maps.geocoding | false",
                "welcome.title2 | expr:'{}'",
            ]
        );
    }

    #[test]
    fn cache_operations() {
        let src = br#"<?php
namespace App\Models;
use Illuminate\Support\Facades\Cache;
class Popup {
    public const CACHE_KEY = 'consumer:popups:v1';
    public static function live($locale) { return Cache::remember(self::CACHE_KEY.':'.$locale, 120, fn () => []); }
    public static function clear($locale) { Cache::forget(self::CACHE_KEY.':'.$locale); }
    public function menu($id) { return Cache::tags(['menu'])->rememberForever("menu:{$id}", fn () => 1); }
    public function flushMenu() { Cache::tags('menu')->flush(); cache()->forget('legacy'); return cache('settings.map'); }
    public function put() { Cache::put('fcm.token', 'x', now()->addMinutes(55)); Cache::query(); }
}
"#;
        let tree = crate::parse::parse(src).unwrap();
        let refs: Vec<String> = collect(tree.root_node(), src)
            .into_iter()
            .filter(|r| r.kind.starts_with("cache"))
            .map(|r| format!("{} {}", r.kind, r.target.replace('\t', " | ")))
            .collect();
        assert_eq!(
            refs,
            [
                "cache_write «const:App\\Models\\Popup::CACHE_KEY»:{} | 120",
                "cache_forget «const:App\\Models\\Popup::CACHE_KEY»:{}",
                "cache_write tags:menu|menu:{} | forever",
                "cache_flush tags:menu",
                "cache_forget legacy",
                "cache_read settings.map",
                "cache_write fcm.token | now()->addMinutes(55)",
            ]
        );
    }

    #[test]
    fn test_http_calls_and_blade() {
        let src = b"<?php\nclass OrderTest extends TestCase {\n  public function test_it() {\n    $this->getJson('/api/v1/orders')->assertOk();\n    $this->postJson(route('orders.store'), []);\n    $this->getJson('/api/v1/brands?sort=popular'.$near);\n    $this->getJson('/api/v1/brands/' . $brand->id . '/menu');\n  }\n}\n";
        let tree = crate::parse::parse(src).unwrap();
        let refs: Vec<String> = collect(tree.root_node(), src)
            .into_iter()
            .map(|r| format!("{} {}", r.kind, r.target))
            .collect();
        assert!(
            refs.contains(&"http GET /api/v1/orders".to_owned()),
            "{refs:?}"
        );
        assert!(refs.contains(&"route orders.store".to_owned()), "{refs:?}");
        assert!(
            refs.contains(&"http GET /api/v1/brands?sort=popular{}".to_owned()),
            "{refs:?}"
        );
        assert!(
            refs.contains(&"http GET /api/v1/brands/{}/menu".to_owned()),
            "{refs:?}"
        );
        let b: Vec<String> =
            blade_refs("<a href=\"{{ route('orders.index') }}\">\n{{ route(\"home\") }}")
                .into_iter()
                .map(|r| format!("{}:{}", r.target, r.line))
                .collect();
        assert_eq!(b, ["orders.index:1", "home:2"]);
    }
}
