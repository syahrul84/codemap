//! What a controller action returns, read statically on demand: API Resources
//! (`toArray` keys, `when*` / `merge*` marked conditional), Resource
//! collections, literal arrays and `response()->json([...])`. Anything built
//! from queries is reported by kind only; fields are never guessed.

use std::path::Path;

use pidx_core::symbols;
use pidx_php::Scope;
use pidx_php::ast::{find_class, named_children, text};
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
    /// `file:line` of the return that was read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub returned_at: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

struct Parsed {
    src: Vec<u8>,
    tree: tree_sitter::Tree,
    file: String,
}

fn parse_class(conn: &Connection, root: &Path, fqn: &str) -> Option<Parsed> {
    let sym = symbols::by_fqn(conn, fqn)
        .ok()?
        .into_iter()
        .find(|s| s.kind.is_class_like())?;
    let src = std::fs::read(root.join(&sym.file)).ok()?;
    let tree = pidx_php::parse::parse(&src)?;
    Some(Parsed {
        src,
        tree,
        file: sym.file,
    })
}

fn method<'t>(class: Node<'t>, src: &[u8], name: &str) -> Option<Node<'t>> {
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
        let s = expr_shape(conn, root, &p, &scope, body, *r, 0);
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
        pidx_php::ast::truncate(&pidx_php::ast::compact(text(rets[0], &p.src)), 80)
    ));
    shape
}

fn expr_shape(
    conn: &Connection,
    root: &Path,
    p: &Parsed,
    scope: &Scope,
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
            let (fields, complete) = array_fields(conn, root, scope, &p.src, e, 0);
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
                        let mut s = expr_shape(conn, root, p, scope, body, *a, depth + 1);
                        if s.kind == "array" {
                            s.kind = "json".into();
                        }
                        if s.kind == "unknown" {
                            s.kind = "json".into();
                            s.notes.push(format!(
                                "json payload `{}` is not a literal",
                                pidx_php::ast::truncate(
                                    &pidx_php::ast::compact(text(*a, &p.src)),
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
                return expr_shape(conn, root, p, scope, body, o, depth + 1);
            }
        }
        "variable_name" => {
            // `return $x;` → the last `$x = …` in the body.
            let var = text(e, &p.src).to_owned();
            if let Some(rhs) = last_assignment(body, &p.src, &var) {
                return expr_shape(conn, root, p, scope, body, rhs, depth + 1);
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
            pidx_php::ast::truncate(&pidx_php::ast::compact(text(r, &p.src)), 60)
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
                        pidx_php::ast::truncate(&pidx_php::ast::compact(vtext), 50)
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
