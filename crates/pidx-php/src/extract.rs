//! Walks a tree-sitter-php tree and produces [`NewSymbol`]s with resolved FQNs.

use pidx_core::symbols::{NewSymbol, Param, SymbolKind, SymbolMeta};
use serde::Serialize;
use tree_sitter::Node;

use crate::ast::{
    apply_use_declaration, children, compact, end_line, named_children, start_line, text, truncate,
};
use crate::doc::doc_before;
use crate::names::Scope;

const MAX_SIGNATURE_CHARS: usize = 240;
const MAX_VALUE_CHARS: usize = 80;
const MAX_SIGNATURE_VALUE_CHARS: usize = 60;

/// Expression kinds kept as literal property defaults / constant values.
const LITERAL_KINDS: &[&str] = &[
    "string",
    "encapsed_string",
    "integer",
    "float",
    "boolean",
    "null",
    "array_creation_expression",
    "unary_op_expression",
    "class_constant_access_expression",
    "heredoc",
    "nowdoc",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParseWarning {
    pub line: u32,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedFile {
    pub symbols: Vec<NewSymbol>,
    pub warnings: Vec<ParseWarning>,
    /// References to classes, methods, functions and routes (find usages).
    pub refs: Vec<crate::refs::Ref>,
}

/// Extracts every named declaration in `src`. Syntax errors produce one
/// warning; whatever tree-sitter recovered is still extracted.
pub fn extract(src: &[u8]) -> ParsedFile {
    let Some(tree) = crate::parse::parse(src) else {
        return ParsedFile {
            symbols: Vec::new(),
            warnings: vec![ParseWarning {
                line: 1,
                message: "PHP parser gave up on this file".into(),
            }],
            refs: Vec::new(),
        };
    };
    let root = tree.root_node();
    let mut x = Extractor {
        src,
        scope: Scope::default(),
        out: Vec::new(),
    };
    x.statements(root);
    let warnings = first_error(root)
        .map(|node| ParseWarning {
            line: start_line(node),
            message: if node.is_missing() {
                format!(
                    "could not parse this code (expected `{}`; or newer PHP syntax the parser does not support); symbols near it may be missing",
                    node.kind()
                )
            } else {
                "could not parse this code (syntax error, or newer PHP syntax the parser does not support); symbols near it may be missing".into()
            },
        })
        .into_iter()
        .collect();
    ParsedFile {
        symbols: x.out,
        warnings,
        refs: crate::refs::collect(root, src),
    }
}

fn first_error(node: Node<'_>) -> Option<Node<'_>> {
    if node.is_error() || node.is_missing() {
        return Some(node);
    }
    if !node.has_error() {
        return None;
    }
    children(node).into_iter().find_map(first_error)
}

struct Extractor<'s> {
    src: &'s [u8],
    scope: Scope,
    out: Vec<NewSymbol>,
}

/// Fields shared by every symbol; the rest come from `NewSymbol`'s defaults.
struct Base<'a> {
    kind: SymbolKind,
    name: &'a str,
    fqn: String,
    parent_fqn: Option<String>,
    node: Node<'a>,
}

impl<'s> Extractor<'s> {
    fn text(&self, node: Node<'_>) -> &'s str {
        text(node, self.src)
    }

    fn symbol(&self, base: Base<'_>, signature: String) -> NewSymbol {
        NewSymbol {
            kind: base.kind,
            name: base.name.to_owned(),
            fqn: base.fqn,
            parent_fqn: base.parent_fqn,
            signature: truncate(&signature, MAX_SIGNATURE_CHARS),
            visibility: None,
            is_static: false,
            is_abstract: false,
            is_final: false,
            start_line: start_line(base.node),
            end_line: end_line(base.node),
            doc_summary: doc_before(base.node, self.src),
            meta: SymbolMeta::default(),
        }
    }

    /// Statements at file level, including blocks whose declarations still
    /// define global names, e.g. `if (!function_exists('money')) { function money() {} }`.
    fn statements(&mut self, node: Node<'_>) {
        for child in named_children(node) {
            match child.kind() {
                "namespace_definition" => self.namespace(child),
                "namespace_use_declaration" => {
                    apply_use_declaration(child, self.src, &mut self.scope)
                }
                "class_declaration"
                | "interface_declaration"
                | "trait_declaration"
                | "enum_declaration" => self.class_like(child),
                "function_definition" => self.function(child),
                "const_declaration" => self.constants(child, None),
                "compound_statement" | "if_statement" | "else_clause" | "else_if_clause"
                | "colon_block" | "declare_statement" | "ERROR" => self.statements(child),
                _ => {}
            }
        }
    }

    fn namespace(&mut self, node: Node<'_>) {
        let name = node
            .child_by_field_name("name")
            .map(|n| self.text(n))
            .unwrap_or("");
        match node.child_by_field_name("body") {
            Some(body) => {
                let saved = self.scope.clone();
                self.scope.set_namespace(name);
                self.statements(body);
                self.scope = saved;
            }
            None => self.scope.set_namespace(name),
        }
    }

    /// Raw and resolved class names listed in an extends/implements/use clause.
    fn class_list(&self, node: Node<'_>) -> (Vec<&'s str>, Vec<String>) {
        let names: Vec<&str> = named_children(node)
            .into_iter()
            .filter(|n| matches!(n.kind(), "name" | "qualified_name" | "relative_name"))
            .map(|n| self.text(n))
            .collect();
        let fqns = names
            .iter()
            .filter_map(|n| self.scope.resolve_class(n))
            .collect();
        (names, fqns)
    }

    /// Resolved class names in a type (`?Order`, `Order|Collection`, ...).
    fn type_fqns(&self, node: Option<Node<'_>>) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack: Vec<Node<'_>> = node.into_iter().collect();
        while let Some(n) = stack.pop() {
            if n.kind() == "named_type" {
                for c in named_children(n) {
                    if let Some(fqn) = self.scope.resolve_class(self.text(c))
                        && !out.contains(&fqn)
                    {
                        out.push(fqn);
                    }
                }
            } else {
                stack.extend(named_children(n).into_iter().rev());
            }
        }
        out
    }

    /// Resolved names of `#[Attribute]`s on a declaration.
    fn attributes(&self, node: Node<'_>) -> Vec<String> {
        let Some(list) = node.child_by_field_name("attributes") else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut stack = vec![list];
        while let Some(n) = stack.pop() {
            if n.kind() == "attribute" {
                if let Some(name) = named_children(n)
                    .into_iter()
                    .find(|c| matches!(c.kind(), "name" | "qualified_name" | "relative_name"))
                {
                    out.extend(self.scope.resolve_class(self.text(name)));
                }
            } else {
                stack.extend(named_children(n).into_iter().rev());
            }
        }
        out
    }

    fn literal(&self, node: Option<Node<'_>>) -> Option<String> {
        let node = node?;
        LITERAL_KINDS
            .contains(&node.kind())
            .then(|| truncate(&compact(self.text(node)), MAX_VALUE_CHARS))
    }

    fn class_like(&mut self, node: Node<'_>) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = self.text(name_node);
        let fqn = self.scope.qualify(name);
        let (kind, keyword) = match node.kind() {
            "interface_declaration" => (SymbolKind::Interface, "interface"),
            "trait_declaration" => (SymbolKind::Trait, "trait"),
            "enum_declaration" => (SymbolKind::Enum, "enum"),
            _ => (SymbolKind::Class, "class"),
        };

        let mut modifiers = Vec::new();
        let mut meta = SymbolMeta::default();
        let (mut is_abstract, mut is_final) = (false, false);
        let mut extends_text = String::new();
        let mut implements_text = String::new();
        for child in children(node) {
            match child.kind() {
                "abstract_modifier" => {
                    is_abstract = true;
                    modifiers.push(self.text(child));
                }
                "final_modifier" => {
                    is_final = true;
                    modifiers.push(self.text(child));
                }
                "readonly_modifier" => {
                    meta.readonly = true;
                    modifiers.push(self.text(child));
                }
                "base_clause" => {
                    let (raw, fqns) = self.class_list(child);
                    extends_text = format!(" extends {}", raw.join(", "));
                    meta.extends = fqns;
                }
                "class_interface_clause" => {
                    let (raw, fqns) = self.class_list(child);
                    implements_text = format!(" implements {}", raw.join(", "));
                    meta.implements = fqns;
                }
                "primitive_type" => meta.backed_type = Some(self.text(child).to_owned()),
                _ => {}
            }
        }
        meta.attributes = self.attributes(node);

        let mut signature = modifiers.join(" ");
        if !signature.is_empty() {
            signature.push(' ');
        }
        signature.push_str(keyword);
        signature.push(' ');
        signature.push_str(name);
        if let Some(backed) = &meta.backed_type {
            signature.push_str(": ");
            signature.push_str(backed);
        }
        signature.push_str(&extends_text);
        signature.push_str(&implements_text);

        let saved = (self.scope.class.take(), self.scope.parent.take());
        self.scope.class = Some(fqn.clone());
        self.scope.parent = (kind == SymbolKind::Class)
            .then(|| meta.extends.first().cloned())
            .flatten();

        let body = node.child_by_field_name("body");
        let members = body.map(named_children).unwrap_or_default();
        for member in &members {
            if member.kind() == "use_declaration" {
                let (_, fqns) = self.class_list(*member);
                meta.traits.extend(fqns);
            }
        }

        let mut symbol = self.symbol(
            Base {
                kind,
                name,
                fqn: fqn.clone(),
                parent_fqn: None,
                node,
            },
            signature,
        );
        symbol.is_abstract = is_abstract;
        symbol.is_final = is_final;
        symbol.meta = meta;
        self.out.push(symbol);

        for member in members {
            match member.kind() {
                "method_declaration" => self.method(member, &fqn),
                "property_declaration" => self.properties(member, &fqn),
                "const_declaration" => self.constants(member, Some(&fqn)),
                "enum_case" => self.enum_case(member, &fqn),
                _ => {}
            }
        }

        (self.scope.class, self.scope.parent) = saved;
    }

    fn params<'t>(&self, node: Option<Node<'t>>) -> Vec<(Param, Node<'t>)> {
        let Some(node) = node else {
            return Vec::new();
        };
        named_children(node)
            .into_iter()
            .filter(|p| {
                matches!(
                    p.kind(),
                    "simple_parameter" | "variadic_parameter" | "property_promotion_parameter"
                )
            })
            .map(|p| {
                let raw_name = p
                    .child_by_field_name("name")
                    .map(|n| self.text(n))
                    .unwrap_or("");
                let ty = p.child_by_field_name("type");
                let param = Param {
                    name: raw_name.trim_start_matches('&').to_owned(),
                    r#type: ty.map(|t| compact(self.text(t))),
                    type_fqns: self.type_fqns(ty),
                    default: p
                        .child_by_field_name("default_value")
                        .map(|d| truncate(&compact(self.text(d)), MAX_VALUE_CHARS)),
                    variadic: p.kind() == "variadic_parameter",
                    by_ref: p.child_by_field_name("reference_modifier").is_some()
                        || raw_name.starts_with('&'),
                    promoted: p.kind() == "property_promotion_parameter",
                };
                (param, p)
            })
            .collect()
    }

    fn function(&mut self, node: Node<'_>) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = self.text(name_node);
        let mut symbol = self.callable(
            node,
            Base {
                kind: SymbolKind::Function,
                name,
                fqn: self.scope.qualify(name),
                parent_fqn: None,
                node,
            },
            "",
        );
        symbol.meta.attributes = self.attributes(node);
        self.out.push(symbol);
    }

    fn method(&mut self, node: Node<'_>, class_fqn: &str) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = self.text(name_node);
        let mut modifiers = Vec::new();
        let mut visibility = None;
        let (mut is_static, mut is_abstract, mut is_final) = (false, false, false);
        for child in children(node) {
            match child.kind() {
                "visibility_modifier" => visibility = Some(self.text(child).to_owned()),
                "static_modifier" => is_static = true,
                "abstract_modifier" => is_abstract = true,
                "final_modifier" => is_final = true,
                "readonly_modifier" | "var_modifier" => {}
                _ => continue,
            }
            modifiers.push(self.text(child));
        }
        let mut prefix = modifiers.join(" ");
        if !prefix.is_empty() {
            prefix.push(' ');
        }
        let mut symbol = self.callable(
            node,
            Base {
                kind: SymbolKind::Method,
                name,
                fqn: format!("{class_fqn}::{name}"),
                parent_fqn: Some(class_fqn.to_owned()),
                node,
            },
            &prefix,
        );
        symbol.visibility = Some(visibility.unwrap_or_else(|| "public".into()));
        symbol.is_static = is_static;
        symbol.is_abstract = is_abstract;
        symbol.is_final = is_final;
        symbol.meta.attributes = self.attributes(node);
        self.out.push(symbol);

        if name.eq_ignore_ascii_case("__construct") {
            self.promoted_properties(node, class_fqn);
        }
    }

    /// Shared by functions and methods: params, return type, signature.
    fn callable(&self, node: Node<'_>, base: Base<'_>, prefix: &str) -> NewSymbol {
        let params_node = node.child_by_field_name("parameters");
        let return_node = node.child_by_field_name("return_type");
        let params_text = params_node
            .map(|p| compact(self.text(p)))
            .unwrap_or_else(|| "()".into());
        let return_text = return_node.map(|r| compact(self.text(r)));
        let signature = format!(
            "{prefix}function {}{params_text}{}",
            base.name,
            return_text
                .as_deref()
                .map(|r| format!(": {r}"))
                .unwrap_or_default()
        );
        let mut symbol = self.symbol(base, signature);
        symbol.meta.params = self
            .params(params_node)
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        symbol.meta.return_type = return_text;
        symbol.meta.return_fqns = self.type_fqns(return_node);
        symbol
    }

    /// Constructor-promoted parameters are properties too.
    fn promoted_properties(&mut self, ctor: Node<'_>, class_fqn: &str) {
        let params = self.params(ctor.child_by_field_name("parameters"));
        for (param, node) in params.into_iter().filter(|(p, _)| p.promoted) {
            let name = param.name.trim_start_matches('$');
            let mut symbol = self.symbol(
                Base {
                    kind: SymbolKind::Property,
                    name,
                    fqn: format!("{class_fqn}::${name}"),
                    parent_fqn: Some(class_fqn.to_owned()),
                    node,
                },
                compact(self.text(node)),
            );
            symbol.doc_summary = None;
            symbol.visibility = node
                .child_by_field_name("visibility")
                .map(|v| self.text(v).to_owned());
            symbol.meta.readonly = node.child_by_field_name("readonly").is_some();
            symbol.meta.r#type = param.r#type.clone();
            symbol.meta.value = self.literal(node.child_by_field_name("default_value"));
            self.out.push(symbol);
        }
    }

    fn properties(&mut self, node: Node<'_>, class_fqn: &str) {
        let mut modifiers = Vec::new();
        let mut visibility = None;
        let (mut is_static, mut readonly, mut is_abstract, mut is_final) =
            (false, false, false, false);
        for child in children(node) {
            match child.kind() {
                "visibility_modifier" => visibility = Some(self.text(child).to_owned()),
                "var_modifier" => visibility = Some("public".to_owned()),
                "static_modifier" => is_static = true,
                "readonly_modifier" => readonly = true,
                "abstract_modifier" => is_abstract = true,
                "final_modifier" => is_final = true,
                _ => continue,
            }
            modifiers.push(self.text(child));
        }
        let ty = node
            .child_by_field_name("type")
            .map(|t| compact(self.text(t)));
        for element in named_children(node) {
            if element.kind() != "property_element" {
                continue;
            }
            let Some(var) = element.child_by_field_name("name") else {
                continue;
            };
            let name = self.text(var).trim_start_matches('$');
            let value = self.literal(element.child_by_field_name("default_value"));
            let mut parts: Vec<String> = modifiers.iter().map(|m| (*m).to_owned()).collect();
            parts.extend(ty.clone());
            parts.push(format!("${name}"));
            if let Some(v) = &value {
                parts.push(format!("= {}", truncate(v, MAX_SIGNATURE_VALUE_CHARS)));
            }
            let mut symbol = self.symbol(
                Base {
                    kind: SymbolKind::Property,
                    name,
                    fqn: format!("{class_fqn}::${name}"),
                    parent_fqn: Some(class_fqn.to_owned()),
                    node,
                },
                parts.join(" "),
            );
            symbol.visibility = Some(visibility.clone().unwrap_or_else(|| "public".into()));
            symbol.is_static = is_static;
            symbol.is_abstract = is_abstract;
            symbol.is_final = is_final;
            symbol.meta.readonly = readonly;
            symbol.meta.r#type = ty.clone();
            symbol.meta.value = value;
            self.out.push(symbol);
        }
    }

    /// Class constants (`class_fqn` set) or namespace-level `const X = ...;`.
    fn constants(&mut self, node: Node<'_>, class_fqn: Option<&str>) {
        let mut modifiers = Vec::new();
        let mut visibility = None;
        let mut is_final = false;
        for child in children(node) {
            match child.kind() {
                "visibility_modifier" => visibility = Some(self.text(child).to_owned()),
                "final_modifier" => is_final = true,
                _ => continue,
            }
            modifiers.push(self.text(child));
        }
        let ty = node
            .child_by_field_name("type")
            .map(|t| compact(self.text(t)));
        for element in named_children(node) {
            if element.kind() != "const_element" {
                continue;
            }
            let parts = named_children(element);
            let Some(name_node) = parts.iter().find(|n| n.kind() == "name") else {
                continue;
            };
            let name = self.text(*name_node);
            let value_node = parts.iter().find(|n| n.id() != name_node.id()).copied();
            let value = self.literal(value_node);
            let mut sig: Vec<String> = modifiers.iter().map(|m| (*m).to_owned()).collect();
            sig.push("const".into());
            sig.extend(ty.clone());
            sig.push(name.to_owned());
            if let Some(v) = value_node {
                sig.push(format!(
                    "= {}",
                    truncate(&compact(self.text(v)), MAX_SIGNATURE_VALUE_CHARS)
                ));
            }
            let (fqn, parent) = match class_fqn {
                Some(c) => (format!("{c}::{name}"), Some(c.to_owned())),
                None => (self.scope.qualify(name), None),
            };
            let mut symbol = self.symbol(
                Base {
                    kind: SymbolKind::Const,
                    name,
                    fqn,
                    parent_fqn: parent,
                    node,
                },
                sig.join(" "),
            );
            if class_fqn.is_some() {
                symbol.visibility = Some(visibility.clone().unwrap_or_else(|| "public".into()));
            }
            symbol.is_final = is_final;
            symbol.meta.r#type = ty.clone();
            symbol.meta.value = value;
            self.out.push(symbol);
        }
    }

    fn enum_case(&mut self, node: Node<'_>, enum_fqn: &str) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = self.text(name_node);
        let value_node = node.child_by_field_name("value");
        let signature = match value_node {
            Some(v) => format!("case {name} = {}", compact(self.text(v))),
            None => format!("case {name}"),
        };
        let mut symbol = self.symbol(
            Base {
                kind: SymbolKind::EnumCase,
                name,
                fqn: format!("{enum_fqn}::{name}"),
                parent_fqn: Some(enum_fqn.to_owned()),
                node,
            },
            signature,
        );
        symbol.meta.value = self.literal(value_node);
        self.out.push(symbol);
    }
}
