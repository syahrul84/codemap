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
    wrappers: HashMap<String, (usize, Option<usize>)>,
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
                        && !matches!(
                            method,
                            "put" | "set" | "forget" | "update" | "store" | "save" | "delete"
                        )
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
                    // The default may be a parameter used as the fallback.
                    let default = pos(1).or_else(|| {
                        let f = fallback_of(c, self.src)?;
                        params
                            .iter()
                            .position(|p| p == text(strip_casts(f), self.src))
                    });
                    self.wrappers.insert(name.clone(), (k, default));
                }
            }
        }
    }

    fn setting_at(&mut self, n: Node<'_>, key_idx: usize, default_idx: Option<usize>, write: bool) {
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
        let default = default_idx
            .and_then(|i| args.get(i))
            .and_then(|a| value(*a))
            .or_else(|| {
                if write {
                    None
                } else {
                    fallback_of(n, self.src).and_then(value)
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

    fn class_name(&self, raw: &str) -> Option<String> {
        match raw.to_ascii_lowercase().as_str() {
            "self" | "static" => self.class.clone(),
            "parent" => self.parent.clone(),
            _ => self.scope.resolve_class(raw),
        }
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
                        self.push("call", format!("{fqn}::{method}"), n);
                        let short = fqn.rsplit('\\').next().unwrap_or(&fqn);
                        if short.contains("Setting") {
                            self.setting(n, &method);
                        } else if self.class.as_deref() == Some(fqn.as_str())
                            && let Some((k, d)) = self.wrappers.get(&method).copied()
                        {
                            self.setting_at(n, k, d, false);
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
                // Tests: `$this->getJson('/api/orders')`.
                if obj_text == "$this"
                    && let Some((_, verb)) = HTTP_TEST_METHODS.iter().find(|(m, _)| *m == name)
                    && let Some(url) = first_arg(n).and_then(|a| url_lit(a, self.src))
                    && (url.starts_with('/') || url.contains('/'))
                {
                    self.push("http", format!("{verb} {url}"), n);
                }
                if name == "route"
                    && let Some(r) = first_arg(n).and_then(|a| string_lit(a, self.src))
                {
                    self.push("route", r, n);
                }
                if obj_text == "$this"
                    && let Some((k, d)) = self.wrappers.get(&name).copied()
                {
                    self.setting_at(n, k, d, false);
                }
                let receiver = match obj_text.as_str() {
                    "$this" => self.class.clone(),
                    v => self.vars.get(v).cloned(),
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
