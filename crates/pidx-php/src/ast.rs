//! Small helpers over tree-sitter-php nodes, shared with framework adapters.

use tree_sitter::Node;

use crate::names::{Scope, UseKind};

pub fn text<'a>(node: Node<'_>, src: &'a [u8]) -> &'a str {
    node.utf8_text(src).unwrap_or("")
}

pub fn named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

pub fn children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

pub fn start_line(node: Node<'_>) -> u32 {
    node.start_position().row as u32 + 1
}

pub fn end_line(node: Node<'_>) -> u32 {
    node.end_position().row as u32 + 1
}

/// Applies a `use ...;` statement (`namespace_use_declaration`) to `scope`,
/// including aliases, grouped imports and `use function` / `use const`.
pub fn apply_use_declaration(node: Node<'_>, src: &[u8], scope: &mut Scope) {
    let decl_kind = use_kind(node.child_by_field_name("type"));
    let mut prefix: Option<&str> = None;
    for child in named_children(node) {
        match child.kind() {
            "namespace_name" => prefix = Some(text(child, src).trim_start_matches('\\')),
            "namespace_use_clause" => use_clause(child, src, scope, decl_kind, None),
            "namespace_use_group" => {
                for clause in named_children(child) {
                    if clause.kind() == "namespace_use_clause" {
                        use_clause(clause, src, scope, decl_kind, prefix);
                    }
                }
            }
            _ => {}
        }
    }
}

fn use_clause(
    clause: Node<'_>,
    src: &[u8],
    scope: &mut Scope,
    decl_kind: Option<UseKind>,
    prefix: Option<&str>,
) {
    let kind = use_kind(clause.child_by_field_name("type"))
        .or(decl_kind)
        .unwrap_or(UseKind::Class);
    let alias = clause.child_by_field_name("alias");
    let Some(name) = named_children(clause).into_iter().find(|n| {
        matches!(n.kind(), "name" | "qualified_name") && Some(n.id()) != alias.map(|a| a.id())
    }) else {
        return;
    };
    let name = text(name, src).trim_start_matches('\\');
    let full = match prefix {
        Some(p) => format!("{p}\\{name}"),
        None => name.to_owned(),
    };
    scope.add_use(kind, &full, alias.map(|a| text(a, src)));
}

fn use_kind(node: Option<Node<'_>>) -> Option<UseKind> {
    match node?.kind() {
        "function" => Some(UseKind::Function),
        "const" => Some(UseKind::Const),
        _ => None,
    }
}

/// Collapses whitespace runs and tidies bracket spacing left by line breaks.
pub fn compact(s: &str) -> String {
    let joined = s.split_whitespace().collect::<Vec<_>>().join(" ");
    joined
        .replace("( ", "(")
        .replace(" )", ")")
        .replace("[ ", "[")
        .replace(" ]", "]")
        .replace(",)", ")")
        .replace(",]", "]")
}

/// At most `max` characters, ending in `…` when cut.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The class-like declaration named `fqn` in a parsed file, with the names in
/// scope at that point (namespace + imports).
pub fn find_class<'t>(root: Node<'t>, src: &[u8], fqn: &str) -> Option<(Node<'t>, Scope)> {
    let mut scope = Scope::default();
    find_class_in(root, src, fqn, &mut scope)
}

fn find_class_in<'t>(
    node: Node<'t>,
    src: &[u8],
    fqn: &str,
    scope: &mut Scope,
) -> Option<(Node<'t>, Scope)> {
    for child in named_children(node) {
        match child.kind() {
            "namespace_definition" => {
                let name = child
                    .child_by_field_name("name")
                    .map(|n| text(n, src))
                    .unwrap_or("");
                match child.child_by_field_name("body") {
                    Some(body) => {
                        let mut inner = scope.clone();
                        inner.set_namespace(name);
                        if let Some(found) = find_class_in(body, src, fqn, &mut inner) {
                            return Some(found);
                        }
                    }
                    None => scope.set_namespace(name),
                }
            }
            "namespace_use_declaration" => apply_use_declaration(child, src, scope),
            "class_declaration"
            | "interface_declaration"
            | "trait_declaration"
            | "enum_declaration" => {
                let name = child
                    .child_by_field_name("name")
                    .map(|n| text(n, src))
                    .unwrap_or("");
                if scope.qualify(name).eq_ignore_ascii_case(fqn) {
                    let mut class_scope = scope.clone();
                    class_scope.class = Some(fqn.to_owned());
                    return Some((child, class_scope));
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_tidies_multiline_params() {
        assert_eq!(
            compact("(\n    int $a,\n    array $b = [\n 'x',\n ],\n)"),
            "(int $a, array $b = ['x'])"
        );
    }

    #[test]
    fn truncate_counts_chars() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
    }
}
