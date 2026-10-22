//! Per-file extraction for `.rs` files: items become symbols named by their
//! Rust path (`my_crate::store::Store::open`), names used become references,
//! and trait impls, `fn main`, command-line subcommands and test code are kept
//! as per-file facts.

use std::collections::HashMap;

use codemap_core::symbols::{NewSymbol, Param, SymbolKind, SymbolMeta};
use serde::{Deserialize, Serialize};
use tree_sitter::Node;

/// `impl Trait for Type`, or `impl Type`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Impl {
    /// Resolved path of the type.
    pub r#type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#trait: Option<String>,
    pub line: u32,
}

/// A variant of an enum deriving clap's `Subcommand`: one CLI command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Command {
    /// As typed: `scan`, `report-wrong`.
    pub name: String,
    /// `Command::Scan`.
    pub variant: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    pub line: u32,
}

/// A reference to a name: kind is `rust_type`, `rust_call` or `rust_member`,
/// or `rust_path`: the path a name was written with, through the file's `use`
/// lines (`views::replace` → `my_crate::views::replace`), on the same line.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Ref {
    pub kind: &'static str,
    pub target: String,
    pub line: u32,
}

/// Everything known about one file, stored as `rust_files.facts_json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RustFacts {
    /// `my_crate::store`.
    pub module: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub impls: Vec<Impl>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<Command>,
    /// Line of `fn main`, when the file has one at the top level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main: Option<u32>,
    /// Line ranges of `#[cfg(test)]` modules and `#[test]` functions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_ranges: Vec<(u32, u32)>,
    #[serde(default)]
    pub errors: u32,
    pub lines: u32,
}

#[derive(Debug, Default)]
pub struct Extracted {
    pub facts: RustFacts,
    pub symbols: Vec<NewSymbol>,
    pub refs: Vec<Ref>,
}

/// Extracts one file. `module` is its Rust path: `my_crate` for `src/lib.rs`,
/// `my_crate::store` for `src/store.rs` or `src/store/mod.rs`.
pub fn extract_file(module: &str, src: &str) -> Extracted {
    let mut out = Extracted::default();
    out.facts.lines = src.lines().count() as u32;
    out.facts.module = module.to_owned();
    let Some(tree) = crate::parse::parse(src.as_bytes()) else {
        return out;
    };
    let root = tree.root_node();
    let mut cx = Cx {
        src: src.as_bytes(),
        out: &mut out,
        crate_name: module.split("::").next().unwrap_or(module).to_owned(),
        types: HashMap::new(),
        uses: HashMap::new(),
        lets: Default::default(),
    };
    cx.out.facts.errors = count_errors(root);
    cx.collect_types(root, module);
    cx.collect_uses(root, module);
    cx.collect_lets(root);
    cx.items(root, module, false);
    cx.refs(root);
    let mut seen = std::collections::HashSet::new();
    out.refs
        .retain(|r| seen.insert((r.kind, r.target.clone(), r.line)));
    out
}

fn count_errors(n: Node) -> u32 {
    if !n.has_error() {
        return 0;
    }
    if n.is_error() || n.is_missing() {
        return 1;
    }
    let mut c = n.walk();
    n.children(&mut c).map(count_errors).sum()
}

struct Cx<'a> {
    src: &'a [u8],
    out: &'a mut Extracted,
    crate_name: String,
    /// Types declared in this file: short name → path.
    types: HashMap<String, String>,
    /// `use` imports: name → path.
    uses: HashMap<String, String>,
    /// Names bound by `let` somewhere in the file (closures, mostly).
    lets: std::collections::HashSet<String>,
}

fn line(n: Node) -> u32 {
    n.start_position().row as u32 + 1
}

fn end_line(n: Node) -> u32 {
    n.end_position().row as u32 + 1
}

fn named_children(n: Node) -> Vec<Node> {
    let mut c = n.walk();
    n.named_children(&mut c).collect()
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_capitalised(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

/// `MAX_LEN`: a constant or static.
fn is_constant(s: &str) -> bool {
    s.len() > 1
        && s.chars().any(|c| c.is_ascii_uppercase())
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// The reference kind of a bare name: types are capitalised, constants
/// SCREAMING_CASE, functions and macros lowercase.
fn name_kind(s: &str) -> &'static str {
    if is_constant(s) {
        "rust_call"
    } else if is_capitalised(s) {
        "rust_type"
    } else {
        "rust_call"
    }
}

/// `ReportWrong` → `report-wrong` (clap's default command name).
pub fn kebab(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Attributes and doc comment written above an item.
#[derive(Default)]
struct Above {
    attributes: Vec<String>,
    doc: Option<String>,
}

impl Above {
    fn has(&self, prefix: &str) -> bool {
        self.attributes.iter().any(|a| a.starts_with(prefix))
    }
    fn derives(&self, name: &str) -> bool {
        self.attributes.iter().any(|a| {
            a.starts_with("derive(")
                && a.trim_start_matches("derive(")
                    .trim_end_matches(')')
                    .split(',')
                    .any(|d| d.trim().rsplit("::").next() == Some(name))
        })
    }
}

impl Cx<'_> {
    fn text(&self, n: Node) -> &str {
        n.utf8_text(self.src).unwrap_or("")
    }

    fn above(&self, n: Node) -> Above {
        let mut a = Above::default();
        let mut docs = Vec::new();
        let mut prev = n.prev_sibling();
        while let Some(p) = prev {
            match p.kind() {
                "attribute_item" => {
                    let t = self.text(p);
                    let inner = t.trim_start_matches("#[").trim_end_matches(']');
                    a.attributes.push(squash(inner));
                }
                "line_comment" => {
                    let t = self.text(p);
                    match t.strip_prefix("///") {
                        Some(d) => docs.push(d.trim().to_owned()),
                        None => break,
                    }
                }
                "block_comment" if self.text(p).starts_with("/**") => {
                    let t = self.text(p);
                    docs.push(squash(
                        &t.trim_start_matches("/**")
                            .trim_end_matches("*/")
                            .replace('*', " "),
                    ));
                }
                _ => break,
            }
            prev = p.prev_sibling();
        }
        a.attributes.reverse();
        docs.reverse();
        let doc = squash(&docs.join(" "));
        if !doc.is_empty() {
            let first = doc
                .split(". ")
                .next()
                .unwrap_or(&doc)
                .trim_end_matches('.')
                .to_owned();
            a.doc = Some(first.chars().take(200).collect());
        }
        a
    }

    fn visibility(&self, n: Node) -> String {
        named_children(n)
            .into_iter()
            .find(|c| c.kind() == "visibility_modifier")
            .map(|v| squash(self.text(v)))
            .unwrap_or_else(|| "private".into())
    }

    /// The item's text up to its body, squashed.
    fn header(&self, n: Node) -> String {
        let body = n.child_by_field_name("body");
        let end = body.map(|b| b.start_byte()).unwrap_or(n.end_byte());
        let s = std::str::from_utf8(&self.src[n.start_byte()..end]).unwrap_or("");
        let s = squash(s.trim_end_matches(';'));
        s.chars().take(240).collect()
    }

    // ------------------------------------------------------------ names

    fn collect_types(&mut self, n: Node, module: &str) {
        for c in named_children(n) {
            match c.kind() {
                "struct_item" | "enum_item" | "union_item" | "trait_item" | "type_item" => {
                    if let Some(name) = c.child_by_field_name("name") {
                        let name = self.text(name).to_owned();
                        self.types
                            .entry(name.clone())
                            .or_insert(format!("{module}::{name}"));
                    }
                }
                "mod_item" => {
                    if let (Some(name), Some(body)) =
                        (c.child_by_field_name("name"), c.child_by_field_name("body"))
                    {
                        let inner = format!("{module}::{}", self.text(name));
                        self.collect_types(body, &inner);
                    }
                }
                _ => {}
            }
        }
    }

    /// `crate::a::B` → `my_crate::a::B`; `super::x` → the parent module.
    fn absolute(&self, path: &str, module: &str) -> String {
        let mut parts: Vec<&str> = module.split("::").collect();
        let mut segs = path.split("::").peekable();
        match segs.peek().copied() {
            Some("crate") => {
                segs.next();
                parts = vec![self.crate_name.as_str()];
            }
            Some("self") => {
                segs.next();
            }
            Some("super") => {
                while segs.peek() == Some(&"super") {
                    segs.next();
                    if parts.len() > 1 {
                        parts.pop();
                    }
                }
            }
            _ => return path.to_owned(),
        }
        let mut out: Vec<String> = parts.iter().map(|s| (*s).to_owned()).collect();
        out.extend(segs.map(str::to_owned));
        out.join("::")
    }

    fn collect_lets(&mut self, n: Node) {
        if n.kind() == "let_declaration"
            && let Some(p) = n.child_by_field_name("pattern")
            && p.kind() == "identifier"
        {
            self.lets.insert(self.text(p).to_owned());
        }
        for c in named_children(n) {
            self.collect_lets(c);
        }
    }

    fn collect_uses(&mut self, n: Node, module: &str) {
        for c in named_children(n) {
            if c.kind() == "use_declaration"
                && let Some(arg) = c.child_by_field_name("argument")
            {
                self.use_tree(arg, "", module);
            }
        }
    }

    fn use_tree(&mut self, n: Node, prefix: &str, module: &str) {
        let join = |a: &str, b: &str| {
            if a.is_empty() {
                b.to_owned()
            } else {
                format!("{a}::{b}")
            }
        };
        match n.kind() {
            "scoped_identifier" | "identifier" => {
                let path = join(prefix, self.text(n));
                let name = path.rsplit("::").next().unwrap_or(&path).to_owned();
                let full = self.absolute(&path, module);
                if full.contains("::") {
                    self.push_ref("rust_path", &full, n);
                }
                self.uses.insert(name, full);
            }
            "use_as_clause" => {
                if let (Some(p), Some(alias)) = (
                    n.child_by_field_name("path"),
                    n.child_by_field_name("alias"),
                ) {
                    let path = join(prefix, self.text(p));
                    let full = self.absolute(&path, module);
                    self.uses.insert(self.text(alias).to_owned(), full);
                }
            }
            "scoped_use_list" => {
                let base = n
                    .child_by_field_name("path")
                    .map(|p| join(prefix, self.text(p)))
                    .unwrap_or_else(|| prefix.to_owned());
                if let Some(list) = n.child_by_field_name("list") {
                    self.use_tree(list, &base, module);
                }
            }
            "use_list" => {
                for c in named_children(n) {
                    if c.kind() == "self" {
                        let name = prefix.rsplit("::").next().unwrap_or(prefix).to_owned();
                        let full = self.absolute(prefix, module);
                        self.uses.insert(name, full);
                    } else {
                        self.use_tree(c, prefix, module);
                    }
                }
            }
            _ => {}
        }
    }

    /// The path of a type named in an `impl`: declared here, imported, or
    /// else assumed to be in this module.
    fn resolve_type(&self, n: Node, module: &str) -> String {
        let n = match n.kind() {
            "generic_type" => n.child_by_field_name("type").unwrap_or(n),
            "reference_type" => {
                return self.resolve_type(n.child_by_field_name("type").unwrap_or(n), module);
            }
            _ => n,
        };
        let text = self.text(n);
        if n.kind() == "scoped_type_identifier" {
            return self.absolute(text, module);
        }
        if let Some(p) = self.types.get(text) {
            return p.clone();
        }
        if let Some(p) = self.uses.get(text) {
            return p.clone();
        }
        format!("{module}::{text}")
    }

    // ------------------------------------------------------------ items

    fn push(
        &mut self,
        n: Node,
        kind: SymbolKind,
        name: String,
        fqn: String,
        above: &Above,
        meta: SymbolMeta,
    ) {
        let visibility = self.visibility(n);
        self.out.symbols.push(NewSymbol {
            kind,
            name,
            fqn,
            parent_fqn: None,
            signature: self.header(n),
            visibility: Some(visibility),
            is_static: false,
            is_abstract: false,
            is_final: false,
            start_line: line(n),
            end_line: end_line(n),
            doc_summary: above.doc.clone(),
            meta,
        });
    }

    fn items(&mut self, n: Node, module: &str, in_test: bool) {
        for c in named_children(n) {
            let above = self.above(c);
            let name = c
                .child_by_field_name("name")
                .map(|x| self.text(x).to_owned());
            match (c.kind(), name) {
                ("struct_item" | "union_item", Some(name)) => {
                    let fqn = format!("{module}::{name}");
                    self.push(
                        c,
                        SymbolKind::Struct,
                        name,
                        fqn.clone(),
                        &above,
                        SymbolMeta {
                            attributes: above.attributes.clone(),
                            ..SymbolMeta::default()
                        },
                    );
                    if let Some(body) = c.child_by_field_name("body") {
                        self.fields(body, &fqn);
                    }
                }
                ("enum_item", Some(name)) => {
                    let fqn = format!("{module}::{name}");
                    self.push(
                        c,
                        SymbolKind::Enum,
                        name.clone(),
                        fqn.clone(),
                        &above,
                        SymbolMeta {
                            attributes: above.attributes.clone(),
                            ..SymbolMeta::default()
                        },
                    );
                    let commands = above.derives("Subcommand");
                    if let Some(body) = c.child_by_field_name("body") {
                        for v in named_children(body)
                            .into_iter()
                            .filter(|v| v.kind() == "enum_variant")
                        {
                            let Some(vn) = v.child_by_field_name("name") else {
                                continue;
                            };
                            let vname = self.text(vn).to_owned();
                            let va = self.above(v);
                            self.out.symbols.push(NewSymbol {
                                kind: SymbolKind::EnumCase,
                                name: vname.clone(),
                                fqn: format!("{fqn}::{vname}"),
                                parent_fqn: Some(fqn.clone()),
                                signature: squash(self.text(v)).chars().take(200).collect(),
                                visibility: None,
                                is_static: true,
                                is_abstract: false,
                                is_final: false,
                                start_line: line(v),
                                end_line: end_line(v),
                                doc_summary: va.doc.clone(),
                                meta: SymbolMeta {
                                    attributes: va.attributes.clone(),
                                    ..SymbolMeta::default()
                                },
                            });
                            if commands {
                                let renamed = va.attributes.iter().find_map(|a| {
                                    let rest = a.strip_prefix("command(")?;
                                    let at = rest.find("name")?;
                                    let q = rest[at..].find('"')? + at + 1;
                                    let e = rest[q..].find('"')? + q;
                                    Some(rest[q..e].to_owned())
                                });
                                self.out.facts.commands.push(Command {
                                    name: renamed.unwrap_or_else(|| kebab(&vname)),
                                    variant: format!("{name}::{vname}"),
                                    doc: va.doc.clone(),
                                    line: line(v),
                                });
                            }
                        }
                    }
                }
                ("trait_item", Some(name)) => {
                    let fqn = format!("{module}::{name}");
                    let supertraits = c
                        .child_by_field_name("bounds")
                        .map(|b| {
                            self.text(b)
                                .trim_start_matches(':')
                                .split('+')
                                .map(|s| s.trim().to_owned())
                                .filter(|s| !s.is_empty())
                                .collect()
                        })
                        .unwrap_or_default();
                    self.push(
                        c,
                        SymbolKind::Trait,
                        name,
                        fqn.clone(),
                        &above,
                        SymbolMeta {
                            extends: supertraits,
                            attributes: above.attributes.clone(),
                            ..SymbolMeta::default()
                        },
                    );
                    if let Some(body) = c.child_by_field_name("body") {
                        self.members(body, &fqn, None, true, in_test);
                    }
                }
                ("impl_item", _) => {
                    let Some(ty) = c.child_by_field_name("type") else {
                        continue;
                    };
                    let ty_path = self.resolve_type(ty, module);
                    let tr = c.child_by_field_name("trait").map(|t| {
                        let t = match t.kind() {
                            "generic_type" => t.child_by_field_name("type").unwrap_or(t),
                            _ => t,
                        };
                        self.text(t).to_owned()
                    });
                    self.out.facts.impls.push(Impl {
                        r#type: ty_path.clone(),
                        r#trait: tr.clone(),
                        line: line(c),
                    });
                    if let Some(body) = c.child_by_field_name("body") {
                        self.members(body, &ty_path, tr.as_deref(), false, in_test);
                    }
                }
                ("function_item", Some(name)) => {
                    let is_test = above.has("test") || above.has("tokio::test");
                    if is_test {
                        self.out.facts.test_ranges.push((line(c), end_line(c)));
                    }
                    if name == "main" && !module.contains("::") {
                        self.out.facts.main = Some(line(c));
                    }
                    let fqn = format!("{module}::{name}");
                    let meta = self.fn_meta(c, &above);
                    self.push(c, SymbolKind::Function, name, fqn, &above, meta);
                }
                ("const_item" | "static_item", Some(name)) => {
                    let fqn = format!("{module}::{name}");
                    let meta = SymbolMeta {
                        r#type: c.child_by_field_name("type").map(|t| squash(self.text(t))),
                        value: c
                            .child_by_field_name("value")
                            .map(|v| squash(self.text(v)).chars().take(120).collect()),
                        readonly: c.kind() == "const_item",
                        ..SymbolMeta::default()
                    };
                    self.push(c, SymbolKind::Const, name, fqn, &above, meta);
                }
                ("type_item", Some(name)) => {
                    let fqn = format!("{module}::{name}");
                    let meta = SymbolMeta {
                        r#type: Some("type alias".into()),
                        value: c.child_by_field_name("type").map(|t| squash(self.text(t))),
                        ..SymbolMeta::default()
                    };
                    self.push(c, SymbolKind::Const, name, fqn, &above, meta);
                }
                ("macro_definition", Some(name)) => {
                    let fqn = format!("{}::{name}", self.crate_name);
                    let meta = SymbolMeta {
                        attributes: vec!["macro".into()],
                        ..SymbolMeta::default()
                    };
                    self.out.symbols.push(NewSymbol {
                        kind: SymbolKind::Function,
                        name: name.clone(),
                        fqn,
                        parent_fqn: None,
                        signature: format!("macro_rules! {name}"),
                        visibility: Some(
                            if above.has("macro_export") {
                                "pub"
                            } else {
                                "private"
                            }
                            .into(),
                        ),
                        is_static: false,
                        is_abstract: false,
                        is_final: false,
                        start_line: line(c),
                        end_line: end_line(c),
                        doc_summary: above.doc.clone(),
                        meta,
                    });
                }
                ("mod_item", Some(name)) => {
                    let test = above.has("cfg(test)");
                    if test {
                        self.out.facts.test_ranges.push((line(c), end_line(c)));
                    }
                    if let Some(body) = c.child_by_field_name("body") {
                        let inner = format!("{module}::{name}");
                        self.items(body, &inner, in_test || test);
                    }
                }
                _ => {}
            }
        }
    }

    fn fields(&mut self, body: Node, owner: &str) {
        for f in named_children(body)
            .into_iter()
            .filter(|f| f.kind() == "field_declaration")
        {
            let Some(name) = f.child_by_field_name("name") else {
                continue;
            };
            let name = self.text(name).to_owned();
            let above = self.above(f);
            let ty = f.child_by_field_name("type").map(|t| squash(self.text(t)));
            self.out.symbols.push(NewSymbol {
                kind: SymbolKind::Property,
                name: name.clone(),
                fqn: format!("{owner}::{name}"),
                parent_fqn: Some(owner.to_owned()),
                signature: squash(self.text(f)),
                visibility: Some(self.visibility(f)),
                is_static: false,
                is_abstract: false,
                is_final: false,
                start_line: line(f),
                end_line: end_line(f),
                doc_summary: above.doc.clone(),
                meta: SymbolMeta {
                    r#type: ty,
                    attributes: above.attributes,
                    ..SymbolMeta::default()
                },
            });
        }
    }

    fn fn_meta(&self, n: Node, above: &Above) -> SymbolMeta {
        let mut params = Vec::new();
        if let Some(ps) = n.child_by_field_name("parameters") {
            for p in named_children(ps) {
                match p.kind() {
                    "self_parameter" => params.push(Param {
                        name: squash(self.text(p)),
                        ..Param::default()
                    }),
                    "parameter" => params.push(Param {
                        name: p
                            .child_by_field_name("pattern")
                            .map(|x| squash(self.text(x)))
                            .unwrap_or_default(),
                        r#type: p.child_by_field_name("type").map(|t| squash(self.text(t))),
                        ..Param::default()
                    }),
                    _ => {}
                }
            }
        }
        SymbolMeta {
            attributes: above.attributes.clone(),
            params,
            return_type: n
                .child_by_field_name("return_type")
                .map(|t| squash(self.text(t))),
            ..SymbolMeta::default()
        }
    }

    /// Items in an `impl` or `trait` body.
    fn members(
        &mut self,
        body: Node,
        owner: &str,
        trait_name: Option<&str>,
        in_trait: bool,
        in_test: bool,
    ) {
        for c in named_children(body) {
            let above = self.above(c);
            let Some(name) = c
                .child_by_field_name("name")
                .map(|x| self.text(x).to_owned())
            else {
                continue;
            };
            let fqn = format!("{owner}::{name}");
            let mut attributes = above.attributes.clone();
            if let Some(t) = trait_name {
                attributes.push(format!("impl {t}"));
            }
            match c.kind() {
                "function_item" | "function_signature_item" => {
                    if above.has("test") {
                        self.out.facts.test_ranges.push((line(c), end_line(c)));
                    }
                    let mut meta = self.fn_meta(c, &above);
                    meta.attributes = attributes;
                    let has_self = meta.params.first().is_some_and(|p| p.name.contains("self"));
                    let visibility = if in_trait || trait_name.is_some() {
                        "pub".to_owned()
                    } else {
                        self.visibility(c)
                    };
                    self.out.symbols.push(NewSymbol {
                        kind: SymbolKind::Method,
                        name,
                        fqn,
                        parent_fqn: Some(owner.to_owned()),
                        signature: self.header(c),
                        visibility: Some(visibility),
                        is_static: !has_self,
                        is_abstract: c.kind() == "function_signature_item",
                        is_final: false,
                        start_line: line(c),
                        end_line: end_line(c),
                        doc_summary: above.doc.clone(),
                        meta,
                    });
                }
                "const_item" | "associated_type" | "type_item" => {
                    self.out.symbols.push(NewSymbol {
                        kind: SymbolKind::Const,
                        name,
                        fqn,
                        parent_fqn: Some(owner.to_owned()),
                        signature: self.header(c),
                        visibility: Some(self.visibility(c)),
                        is_static: true,
                        is_abstract: false,
                        is_final: false,
                        start_line: line(c),
                        end_line: end_line(c),
                        doc_summary: above.doc.clone(),
                        meta: SymbolMeta {
                            attributes,
                            r#type: c.child_by_field_name("type").map(|t| squash(self.text(t))),
                            value: c
                                .child_by_field_name("value")
                                .map(|v| squash(self.text(v)).chars().take(120).collect()),
                            ..SymbolMeta::default()
                        },
                    });
                }
                _ => {}
            }
        }
        let _ = in_test;
    }

    // ------------------------------------------------------------ references

    fn push_ref(&mut self, kind: &'static str, target: &str, n: Node) {
        if target.is_empty() || target.len() > 120 || target == "self" || target == "Self" {
            return;
        }
        self.out.refs.push(Ref {
            kind,
            target: target.to_owned(),
            line: line(n),
        });
    }

    /// `rust_path` for a name written as `path`: through the file's `use`
    /// lines, its own items, or `crate::` / `self::` / `super::`. A bare name
    /// that none of these explain gets none (a glob import, the prelude).
    fn push_path(&mut self, path: &str, n: Node) {
        if path.contains(['<', ' ', '\n']) {
            return;
        }
        let (first, rest) = path
            .split_once("::")
            .map_or((path, None), |(a, b)| (a, Some(b)));
        let full = match self.uses.get(first).or_else(|| self.types.get(first)) {
            Some(full) => match rest {
                Some(r) => format!("{full}::{r}"),
                None => full.clone(),
            },
            // `load()` in a file with its own `fn load`.
            None if rest.is_none() => match self
                .out
                .symbols
                .iter()
                .find(|s| s.parent_fqn.is_none() && s.name == path)
            {
                Some(s) => s.fqn.clone(),
                // `let names = |t| ..; names(t)`: not any function's path.
                None if self.lets.contains(path) => format!("(local)::{path}"),
                None => return,
            },
            None => self.absolute(path, &self.out.facts.module.clone()),
        };
        if full.contains("::") {
            self.push_ref("rust_path", &full, n);
        }
    }

    /// Segments of `a::B::c` before the last: capitalised ones are types.
    fn path_types(&mut self, path: Node) {
        let mut cur = Some(path);
        while let Some(p) = cur {
            match p.kind() {
                "scoped_identifier" | "scoped_type_identifier" => {
                    if let Some(name) = p.child_by_field_name("name")
                        && p != path
                    {
                        let t = self.text(name).to_owned();
                        if is_capitalised(&t) {
                            self.push_ref("rust_type", &t, name);
                            let path = self.text(p).to_owned();
                            self.push_path(&path, name);
                        }
                    }
                    cur = p.child_by_field_name("path");
                }
                "identifier" | "type_identifier" => {
                    let t = self.text(p).to_owned();
                    if p != path && is_capitalised(&t) {
                        self.push_ref("rust_type", &t, p);
                        self.push_path(&t, p);
                    }
                    cur = None;
                }
                "generic_type" => cur = p.child_by_field_name("type"),
                _ => cur = None,
            }
        }
    }

    fn refs(&mut self, n: Node) {
        match n.kind() {
            "type_identifier" => {
                let t = self.text(n).to_owned();
                // A declaration's own name is not a use.
                let declares = n.parent().is_some_and(|p| {
                    matches!(
                        p.kind(),
                        "struct_item" | "enum_item" | "union_item" | "trait_item" | "type_item"
                    ) && p.child_by_field_name("name") == Some(n)
                });
                if !declares {
                    self.push_ref("rust_type", &t, n);
                    match n.parent() {
                        Some(p)
                            if p.kind() == "scoped_type_identifier"
                                && p.child_by_field_name("name") == Some(n) =>
                        {
                            let path = self.text(p).to_owned();
                            self.push_path(&path, n);
                        }
                        _ => self.push_path(&t, n),
                    }
                }
            }
            "call_expression" => {
                if let Some(f) = n.child_by_field_name("function") {
                    let f = if f.kind() == "generic_function" {
                        f.child_by_field_name("function").unwrap_or(f)
                    } else {
                        f
                    };
                    match f.kind() {
                        "identifier" => {
                            let t = self.text(f).to_owned();
                            self.push_ref(
                                if is_capitalised(&t) {
                                    "rust_type"
                                } else {
                                    "rust_call"
                                },
                                &t,
                                f,
                            );
                            self.push_path(&t, f);
                        }
                        "scoped_identifier" => {
                            if let Some(name) = f.child_by_field_name("name") {
                                let t = self.text(name).to_owned();
                                self.push_ref(
                                    if is_capitalised(&t) {
                                        "rust_type"
                                    } else {
                                        "rust_call"
                                    },
                                    &t,
                                    name,
                                );
                                let path = self.text(f).to_owned();
                                self.push_path(&path, name);
                            }
                            self.path_types(f);
                        }
                        "field_expression" => {
                            if let Some(field) = f.child_by_field_name("field") {
                                let t = self.text(field).to_owned();
                                self.push_ref("rust_member", &t, field);
                            }
                        }
                        _ => {}
                    }
                }
            }
            "scoped_identifier" => {
                // Not called: `Kind::A`, `MAX_LEN` through a path.
                let called = n.parent().is_some_and(|p| {
                    p.kind() == "call_expression" && p.child_by_field_name("function") == Some(n)
                }) || n.parent().is_some_and(|p| {
                    matches!(
                        p.kind(),
                        "scoped_identifier"
                            | "use_list"
                            | "scoped_use_list"
                            | "use_declaration"
                            | "use_as_clause"
                    )
                });
                if !called {
                    if let Some(name) = n.child_by_field_name("name") {
                        let t = self.text(name).to_owned();
                        self.push_ref("rust_member", &t, name);
                    }
                    self.path_types(n);
                }
            }
            "field_expression" => {
                let called = n.parent().is_some_and(|p| {
                    p.kind() == "call_expression" && p.child_by_field_name("function") == Some(n)
                });
                if !called && let Some(field) = n.child_by_field_name("field") {
                    let t = self.text(field).to_owned();
                    if !t.chars().all(|c| c.is_ascii_digit()) {
                        self.push_ref("rust_member", &t, field);
                    }
                }
            }
            "identifier" => {
                // A constant used as a value: `if n > MAX_LEN`.
                let t = self.text(n).to_owned();
                let declares = n
                    .parent()
                    .is_some_and(|p| p.child_by_field_name("name") == Some(n));
                if is_constant(&t) && !declares {
                    self.push_ref("rust_call", &t, n);
                    self.push_path(&t, n);
                }
            }
            "macro_invocation" => {
                if let Some(m) = n.child_by_field_name("macro") {
                    let name = match m.kind() {
                        "scoped_identifier" => m.child_by_field_name("name").unwrap_or(m),
                        _ => m,
                    };
                    let t = self.text(name).to_owned();
                    self.push_ref("rust_call", &t, name);
                }
                // Macro arguments are tokens: their names still count.
                for tt in named_children(n)
                    .into_iter()
                    .filter(|c| c.kind() == "token_tree")
                {
                    self.token_names(tt);
                }
                return;
            }
            "use_declaration" => {
                if let Some(arg) = n.child_by_field_name("argument") {
                    self.use_names(arg);
                }
                return;
            }
            _ => {}
        }
        for c in named_children(n) {
            self.refs(c);
        }
    }

    fn token_names(&mut self, n: Node) {
        for c in named_children(n) {
            match c.kind() {
                "identifier" => {
                    let t = self.text(c).to_owned();
                    // `format!("{}", s.replace(..))`: a method, not a function.
                    let kind = if c.prev_sibling().is_some_and(|p| p.kind() == ".") {
                        "rust_member"
                    } else {
                        name_kind(&t)
                    };
                    // `names.join(..)`, `views::x`: a variable or a path
                    // segment. Only `name(..)` calls a function.
                    let called = c
                        .next_sibling()
                        .is_some_and(|n| n.kind() == "token_tree" && self.text(n).starts_with('('));
                    if kind == "rust_call" && !called && !is_constant(&t) {
                        continue;
                    }
                    self.push_ref(kind, &t, c);
                    if kind != "rust_member" {
                        // `fancy::label(x)` as tokens: the path back to its start.
                        let mut path = t.clone();
                        let mut p = c.prev_sibling();
                        while let Some(sep) = p.filter(|p| p.kind() == "::") {
                            let Some(seg) = sep.prev_sibling().filter(|s| s.kind() == "identifier")
                            else {
                                break;
                            };
                            path = format!("{}::{path}", self.text(seg));
                            p = seg.prev_sibling();
                        }
                        self.push_path(&path, c);
                    }
                }
                "token_tree" => self.token_names(c),
                _ => {}
            }
        }
    }

    /// Names a `use` brings in: `use crate::store::{Store, open}`.
    fn use_names(&mut self, n: Node) {
        match n.kind() {
            "scoped_identifier" => {
                if let Some(name) = n.child_by_field_name("name") {
                    self.use_names(name);
                }
            }
            "identifier" => {
                let t = self.text(n).to_owned();
                self.push_ref(name_kind(&t), &t, n);
            }
            "use_as_clause" => {
                if let Some(p) = n.child_by_field_name("path") {
                    self.use_names(p);
                }
            }
            "scoped_use_list" => {
                if let Some(l) = n.child_by_field_name("list") {
                    self.use_names(l);
                }
            }
            "use_list" => {
                for c in named_children(n) {
                    self.use_names(c);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"use crate::store::{self, Store};
use std::path::Path;

/// A thing. More text.
#[derive(Debug, Clone)]
pub struct Item<T> { pub name: String, id: T }

pub enum Kind { A, B(u32), C { x: i32 } }

pub trait Load { fn load(&self) -> u32; const N: usize = 1; }

impl<T> Item<T> {
    pub fn new(name: &str) -> Self { Self { name: name.into(), id: todo!() } }
}

impl Load for Item<u8> { fn load(&self) -> u32 { Store::open().conn(); helper(1); 0 } }

impl Store { fn extra(&self) {} }

pub const MAX: usize = 3;
static NAME: &str = "x";
pub type R<T> = Result<T, Error>;
macro_rules! m { () => {} }
mod inner { pub fn f() {} }

/// Commands.
#[derive(Subcommand)]
enum Command {
    /// Index the project.
    Scan { project: String },
    #[command(name = "go")]
    ReportWrong,
}

fn main() { let k = Kind::A; println!("{}", MAX); x.items.len(); }

#[cfg(test)]
mod tests { #[test] fn t() { super::main(); } }
"#;

    #[test]
    fn calls_keep_the_path_they_were_written_with() {
        let x = extract_file(
            "demo::app",
            "use crate::store::{open, Store};\nuse codemap::views;\n\nfn f(s: &str) {\n    open();\n    views::replace();\n    let i: precise::Index = Store::new();\n    format!(\"{}\", s.trim());\n    local();\n    f(s);\n    format!(\"{} {}\", names.len(), fancy::label(s));\n    let g = |x| x;\n    g(1);\n}\n",
        );
        let has = |kind: &str, target: &str, line: u32| {
            x.refs
                .iter()
                .any(|r| r.kind == kind && r.target == target && r.line == line)
        };
        assert!(has("rust_path", "demo::store::open", 1), "{:?}", x.refs);
        assert!(has("rust_path", "demo::store::open", 5), "{:?}", x.refs);
        assert!(
            has("rust_path", "codemap::views::replace", 6),
            "{:?}",
            x.refs
        );
        assert!(has("rust_path", "precise::Index", 7), "{:?}", x.refs);
        assert!(has("rust_path", "demo::store::Store", 7), "{:?}", x.refs);
        assert!(has("rust_member", "trim", 8), "{:?}", x.refs);
        assert!(!has("rust_call", "trim", 8), "{:?}", x.refs);
        assert!(has("rust_path", "demo::app::f", 10), "{:?}", x.refs);
        assert!(!has("rust_call", "names", 11), "{:?}", x.refs);
        assert!(has("rust_path", "fancy::label", 11), "{:?}", x.refs);
        assert!(has("rust_path", "(local)::g", 13), "{:?}", x.refs);
        // Not explained by a `use`: no path, the name alone.
        assert!(!x.refs.iter().any(|r| r.kind == "rust_path" && r.line == 9));
    }

    #[test]
    fn rust_file() {
        let x = extract_file("my_app::things", SRC);
        let syms: Vec<String> = x
            .symbols
            .iter()
            .map(|s| format!("{} {}", s.kind.as_str(), s.fqn))
            .collect();
        for s in &syms {
            println!("{s}");
        }
        for want in [
            "struct my_app::things::Item",
            "property my_app::things::Item::name",
            "enum my_app::things::Kind",
            "enum_case my_app::things::Kind::B",
            "trait my_app::things::Load",
            "method my_app::things::Load::load",
            "const my_app::things::Load::N",
            "method my_app::things::Item::new",
            "method my_app::things::Item::load",
            "method my_app::store::Store::extra",
            "const my_app::things::MAX",
            "const my_app::things::NAME",
            "const my_app::things::R",
            "function my_app::m",
            "function my_app::things::inner::f",
            "function my_app::things::main",
            "function my_app::things::tests::t",
        ] {
            assert!(syms.iter().any(|s| s == want), "missing {want}");
        }
        let item = x.symbols.iter().find(|s| s.name == "Item").unwrap();
        assert_eq!(item.doc_summary.as_deref(), Some("A thing"));
        assert_eq!(item.meta.attributes, ["derive(Debug, Clone)"]);
        let new = x.symbols.iter().find(|s| s.name == "new").unwrap();
        assert!(new.is_static);
        assert_eq!(new.visibility.as_deref(), Some("pub"));
        let load = x
            .symbols
            .iter()
            .find(|s| s.fqn == "my_app::things::Item::load")
            .unwrap();
        assert!(load.meta.attributes.contains(&"impl Load".to_owned()));
        assert_eq!(
            x.facts.commands,
            [
                Command {
                    name: "scan".into(),
                    variant: "Command::Scan".into(),
                    doc: Some("Index the project".into()),
                    line: 30
                },
                Command {
                    name: "go".into(),
                    variant: "Command::ReportWrong".into(),
                    doc: None,
                    line: 32
                },
            ]
        );
        assert_eq!(x.facts.impls.len(), 3);
        assert_eq!(x.facts.impls[2].r#type, "my_app::store::Store");
        assert_eq!(x.facts.test_ranges.len(), 2);
        let refs: Vec<String> = x
            .refs
            .iter()
            .map(|r| format!("{}:{}", r.kind, r.target))
            .collect();
        for want in [
            "rust_type:Store",
            "rust_call:open",
            "rust_member:conn",
            "rust_call:helper",
            "rust_member:A",
            "rust_type:Kind",
            "rust_call:MAX",
            "rust_member:items",
            "rust_member:len",
            "rust_type:Load",
            "rust_call:println",
        ] {
            assert!(
                refs.contains(&want.to_owned()),
                "missing {want} in {refs:?}"
            );
        }
        // The struct's own name is not a use of it.
        assert!(!x.refs.iter().any(|r| r.target == "Item" && r.line == 6));
        assert_eq!(x.facts.errors, 0);
    }

    #[test]
    fn kebab_names() {
        assert_eq!(kebab("Scan"), "scan");
        assert_eq!(kebab("ReportWrong"), "report-wrong");
    }
}
