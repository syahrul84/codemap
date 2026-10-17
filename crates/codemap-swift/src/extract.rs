//! Per-file extraction for `.swift` files and Apple string catalogs: types and
//! their members become symbols, names used become references, and SwiftUI
//! views, screens and translation keys are kept as per-file facts.

use codemap_core::symbols::{NewSymbol, Param, SymbolKind, SymbolMeta};
use serde::{Deserialize, Serialize};
use tree_sitter::Node;

/// A name and where it appears.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedLine {
    pub name: String,
    pub line: u32,
}

/// A stored property of a type: `@State private var items: [Item] = []`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prop {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    /// Property wrapper without `@`: `State`, `Binding`, `Environment`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapper: Option<String>,
    /// The wrapper's argument: `\.dismiss`, `AppModel.self`, `"key"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapper_arg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default)]
    pub is_let: bool,
    #[serde(default)]
    pub is_static: bool,
    #[serde(default)]
    pub computed: bool,
    pub line: u32,
}

/// A class, struct, enum, actor, protocol or extension.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeFacts {
    /// `Outer.Inner`; for an extension, the extended type.
    pub name: String,
    /// class | struct | enum | actor | protocol | extension
    pub kind: String,
    pub line: u32,
    pub end_line: u32,
    /// Superclass and protocols, as written.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conforms: Vec<String>,
    /// Attributes without `@`: `main`, `Observable`, `MainActor`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attributes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub props: Vec<Prop>,
    /// Capitalised names called in the body (`ContentView()`), in order:
    /// child views once matched with the project's views.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<NamedLine>,
}

/// Where the app opens a screen: a window, a navigation push, a sheet, a tab.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Screen {
    /// window | settings | menu_bar | document | navigation | sheet | cover |
    /// popover | inspector | tab | alert
    pub kind: String,
    /// The SwiftUI call that opens it: `WindowGroup`, `NavigationLink`, `.sheet`.
    pub via: String,
    /// Capitalised calls inside it, outermost first; the screen is the first
    /// that is a project view.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The type it is declared in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub line: u32,
}

/// A translation key used in code.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct I18nUse {
    pub key: String,
    pub line: u32,
    /// Shown as written when no translation exists (`Text("Save")`), so a
    /// missing key is not an error.
    #[serde(default)]
    pub guarded: bool,
}

/// Keys of one language from a `.strings` file or a string catalog.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocaleKeys {
    pub locale: String,
    pub keys: Vec<String>,
}

/// A reference to a name: kind is `swift_type`, `swift_call` or `swift_member`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Ref {
    pub kind: &'static str,
    pub target: String,
    pub line: u32,
}

/// Everything known about one file, stored as `swift_files.facts_json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SwiftFacts {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<TypeFacts>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screens: Vec<Screen>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub i18n_uses: Vec<I18nUse>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locales: Vec<LocaleKeys>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<NamedLine>,
    pub lines: u32,
}

/// Facts, symbols and references of one file.
#[derive(Debug, Default)]
pub struct Extracted {
    pub facts: SwiftFacts,
    pub symbols: Vec<NewSymbol>,
    pub refs: Vec<Ref>,
}

/// SwiftUI calls whose first unlabelled string is a `LocalizedStringKey`.
const LOCALIZED_VIEWS: &[&str] = &[
    "Text",
    "Label",
    "Button",
    "Toggle",
    "Link",
    "Picker",
    "Section",
    "NavigationLink",
    "Menu",
    "TextField",
    "SecureField",
    "Stepper",
    "Tab",
    "LabeledContent",
    "ControlGroup",
    "DisclosureGroup",
    "ShareLink",
    "ContentUnavailableView",
    "LocalizedStringKey",
    "LocalizedStringResource",
];

/// View modifiers whose first string is a `LocalizedStringKey`.
const LOCALIZED_MODIFIERS: &[&str] = &[
    "navigationTitle",
    "navigationSubtitle",
    "help",
    "alert",
    "confirmationDialog",
    "accessibilityLabel",
    "accessibilityHint",
    "badge",
    "tabItem",
    "searchable",
];

/// Scene and navigation calls that open a screen.
fn screen_kind(call: &str) -> Option<&'static str> {
    Some(match call {
        "WindowGroup" | "Window" | "UtilityWindow" => "window",
        "Settings" => "settings",
        "MenuBarExtra" => "menu_bar",
        "DocumentGroup" => "document",
        "ImmersiveSpace" => "immersive",
        "NavigationLink" => "navigation",
        "navigationDestination" => "navigation",
        "sheet" => "sheet",
        "fullScreenCover" => "cover",
        "popover" => "popover",
        "inspector" => "inspector",
        "Tab" => "tab",
        _ => return None,
    })
}

pub fn extract_file(path: &str, src: &str) -> Extracted {
    let lines = src.lines().count() as u32;
    if path.ends_with(".strings") {
        return Extracted {
            facts: SwiftFacts {
                locales: strings_file(path, src).into_iter().collect(),
                lines,
                ..SwiftFacts::default()
            },
            ..Extracted::default()
        };
    }
    if path.ends_with(".xcstrings") {
        if is_info_plist_table(path) {
            return Extracted::default();
        }
        return Extracted {
            facts: SwiftFacts {
                locales: string_catalog(src),
                lines,
                ..SwiftFacts::default()
            },
            ..Extracted::default()
        };
    }
    let mut out = Extracted::default();
    out.facts.lines = lines;
    let Some(tree) = crate::parse::parse(src.as_bytes()) else {
        return out;
    };
    let mut cx = Cx {
        src: src.as_bytes(),
        out: &mut out,
        props: Vec::new(),
    };
    let root = tree.root_node();
    cx.errors(root);
    cx.decls(root, None);
    cx.walk_refs(root);
    dedup_refs(&mut out.refs);
    out
}

fn dedup_refs(refs: &mut Vec<Ref>) {
    let mut seen = std::collections::HashSet::new();
    refs.retain(|r| seen.insert((r.kind, r.target.clone(), r.line)));
}

struct Cx<'a> {
    src: &'a [u8],
    out: &'a mut Extracted,
    /// Properties seen while walking a type body, with their type.
    props: Vec<(String, Prop)>,
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

fn is_capitalised(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

/// Collapses whitespace: `func  load(\n id: Int)` → `func load( id: Int)`.
fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Generic arguments off a type name: `Array<Item>` → `Array`.
fn base_type(s: &str) -> String {
    s.split(['<', ' ', '?', '!'])
        .next()
        .unwrap_or(s)
        .trim()
        .to_owned()
}

impl Cx<'_> {
    fn text(&self, n: Node) -> &str {
        n.utf8_text(self.src).unwrap_or("")
    }

    fn errors(&mut self, n: Node) {
        if !n.has_error() {
            return;
        }
        if n.is_error() || n.is_missing() {
            if self.out.facts.errors.len() < 5 {
                self.out.facts.errors.push(NamedLine {
                    name: if n.is_missing() {
                        format!("missing {}", n.kind())
                    } else {
                        "unexpected code".into()
                    },
                    line: line(n),
                });
            }
            return;
        }
        for c in named_children(n) {
            self.errors(c);
        }
    }

    /// Doc comment right above a declaration: its first sentence.
    fn doc(&self, n: Node) -> Option<String> {
        let mut lines = Vec::new();
        let mut prev = n.prev_sibling();
        while let Some(p) = prev {
            if !p.kind().contains("comment") {
                break;
            }
            let t = self.text(p);
            if let Some(rest) = t.strip_prefix("///") {
                lines.push(rest.trim().to_owned());
            } else if t.starts_with("/**") {
                let body = t.trim_start_matches("/**").trim_end_matches("*/");
                lines.push(squash(&body.replace('*', " ")));
            } else {
                break;
            }
            prev = p.prev_sibling();
        }
        lines.reverse();
        let doc = lines.join(" ").trim().to_owned();
        if doc.is_empty() {
            return None;
        }
        let first = doc
            .split(". ")
            .next()
            .unwrap_or(&doc)
            .trim_end_matches('.')
            .to_owned();
        Some(first.chars().take(200).collect())
    }

    /// Attributes (without `@`), visibility, and `static`/`class`/`final`.
    fn modifiers(&self, n: Node) -> Modifiers {
        let mut m = Modifiers::default();
        for c in named_children(n) {
            if c.kind() != "modifiers" {
                continue;
            }
            for k in named_children(c) {
                let t = self.text(k).trim();
                match k.kind() {
                    "attribute" => {
                        let name = t.trim_start_matches('@');
                        let (name, arg) = match name.split_once('(') {
                            Some((n, rest)) => (
                                n.trim(),
                                Some(squash(rest.trim_end().trim_end_matches(')'))),
                            ),
                            None => (name.trim(), None),
                        };
                        m.attributes.push(name.to_owned());
                        m.attribute_args.push(arg);
                    }
                    "visibility_modifier" => {
                        m.visibility = Some(t.split('(').next().unwrap_or(t).to_owned())
                    }
                    "property_modifier" | "function_modifier" | "member_modifier" => match t {
                        "static" | "class" => m.is_static = true,
                        _ => {}
                    },
                    "inheritance_modifier" if t == "final" => m.is_final = true,
                    _ => {
                        if t == "static" || t == "class" {
                            m.is_static = true;
                        }
                    }
                }
            }
        }
        m
    }

    /// Declarations at the top level or in a type body.
    fn decls(&mut self, n: Node, parent: Option<&str>) {
        for c in named_children(n) {
            match c.kind() {
                "import_declaration" => {
                    let t = self.text(c).trim_start_matches("import").trim();
                    let name = t.split_whitespace().last().unwrap_or(t).to_owned();
                    if !name.is_empty() {
                        self.out.facts.imports.push(name);
                    }
                }
                "class_declaration" | "protocol_declaration" => self.type_decl(c, parent),
                "function_declaration" | "protocol_function_declaration" | "init_declaration" => {
                    self.function(c, parent)
                }
                "property_declaration" | "protocol_property_declaration" => {
                    self.property(c, parent)
                }
                "enum_entry" => self.enum_cases(c, parent),
                "typealias_declaration" => self.typealias(c, parent),
                // `#if os(iOS)` blocks and the like hold declarations too.
                "class_body" | "enum_class_body" | "protocol_body" | "statements" => {
                    self.decls(c, parent)
                }
                k if k.starts_with("directive") || k == "compiler_directive" => {
                    self.decls(c, parent)
                }
                _ => {}
            }
        }
    }

    fn type_decl(&mut self, n: Node, parent: Option<&str>) {
        let kind = if n.kind() == "protocol_declaration" {
            "protocol"
        } else {
            let mut cur = n.walk();
            n.children(&mut cur)
                .find(|c| {
                    !c.is_named()
                        && matches!(
                            c.kind(),
                            "class" | "struct" | "enum" | "extension" | "actor"
                        )
                })
                .map(|c| c.kind())
                .unwrap_or("class")
        };
        let Some(name_node) = n.child_by_field_name("name") else {
            return;
        };
        let short = base_type(self.text(name_node));
        if short.is_empty() {
            return;
        }
        let fqn = if kind == "extension" {
            short.clone()
        } else {
            match parent {
                Some(p) => format!("{p}.{short}"),
                None => short.clone(),
            }
        };
        let m = self.modifiers(n);
        let mut conforms = Vec::new();
        for c in named_children(n) {
            if c.kind() == "inheritance_specifier" {
                let t = c
                    .child_by_field_name("inherits_from")
                    .map(|t| self.text(t))
                    .unwrap_or_else(|| self.text(c));
                let t = base_type(t);
                if !t.is_empty() {
                    conforms.push(t);
                }
            }
        }
        let body = n.child_by_field_name("body");
        let header_end = body.map(|b| b.start_byte()).unwrap_or(n.end_byte());
        let signature = squash(
            std::str::from_utf8(&self.src[n.start_byte()..header_end])
                .unwrap_or("")
                .trim(),
        );
        // Drop leading attributes from the signature: `@main struct App` → `struct App`.
        let signature = signature
            .split_whitespace()
            .skip_while(|w| w.starts_with('@'))
            .collect::<Vec<_>>()
            .join(" ");
        if kind != "extension" {
            let symbol_kind = match kind {
                "struct" => SymbolKind::Struct,
                "enum" => SymbolKind::Enum,
                "protocol" => SymbolKind::Protocol,
                _ => SymbolKind::Class,
            };
            self.out.symbols.push(NewSymbol {
                kind: symbol_kind,
                name: short.clone(),
                fqn: fqn.clone(),
                parent_fqn: parent.map(str::to_owned),
                signature,
                visibility: Some(m.visibility.clone().unwrap_or_else(|| "internal".into())),
                is_static: false,
                is_abstract: false,
                is_final: m.is_final,
                start_line: line(n),
                end_line: end_line(n),
                doc_summary: self.doc(n),
                meta: SymbolMeta {
                    implements: conforms.clone(),
                    attributes: m.attributes.clone(),
                    ..SymbolMeta::default()
                },
            });
        }
        let idx = self.out.facts.types.len();
        self.out.facts.types.push(TypeFacts {
            name: fqn.clone(),
            kind: kind.to_owned(),
            line: line(n),
            end_line: end_line(n),
            conforms,
            attributes: m.attributes,
            ..TypeFacts::default()
        });
        if let Some(b) = body {
            self.decls(b, Some(&fqn));
        }
        // Properties were gathered while walking the members.
        let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.props)
            .into_iter()
            .partition(|(o, _)| *o == fqn);
        self.props = rest;
        self.out.facts.types[idx].props = mine.into_iter().map(|(_, p)| p).collect();
    }

    fn params(&self, n: Node) -> Vec<Param> {
        let mut out: Vec<Param> = Vec::new();
        let mut cur = n.walk();
        if !cur.goto_first_child() {
            return out;
        }
        loop {
            let c = cur.node();
            let field = cur.field_name();
            if c.kind() == "parameter" {
                let name = c
                    .child_by_field_name("name")
                    .map(|x| self.text(x).to_owned())
                    .unwrap_or_default();
                let external = c
                    .child_by_field_name("external_name")
                    .map(|x| self.text(x).to_owned());
                let text = self.text(c);
                let ty = text.split_once(':').map(|(_, t)| squash(t.trim()));
                let shown = match external {
                    Some(e) if e != name => format!("{e} {name}"),
                    _ => name,
                };
                out.push(Param {
                    name: shown,
                    r#type: ty,
                    ..Param::default()
                });
            } else if field == Some("default_value")
                && let Some(last) = out.last_mut()
            {
                last.default = Some(squash(self.text(c)));
            }
            if !cur.goto_next_sibling() {
                break;
            }
        }
        out
    }

    fn function(&mut self, n: Node, parent: Option<&str>) {
        let name = if n.kind() == "init_declaration" {
            "init".to_owned()
        } else {
            match n.child_by_field_name("name") {
                Some(x) => self.text(x).to_owned(),
                None => return,
            }
        };
        let m = self.modifiers(n);
        let body = n.child_by_field_name("body");
        let header_end = body.map(|b| b.start_byte()).unwrap_or(n.end_byte());
        let header =
            squash(std::str::from_utf8(&self.src[n.start_byte()..header_end]).unwrap_or(""));
        let signature = header
            .split_whitespace()
            .skip_while(|w| w.starts_with('@'))
            .collect::<Vec<_>>()
            .join(" ");
        let return_type = signature
            .rsplit_once("->")
            .map(|(_, r)| r.split(" where ").next().unwrap_or(r).trim().to_owned())
            .filter(|r| !r.is_empty());
        let fqn = match parent {
            Some(p) => format!("{p}.{name}"),
            None => name.clone(),
        };
        self.out.symbols.push(NewSymbol {
            kind: if parent.is_some() {
                SymbolKind::Method
            } else {
                SymbolKind::Function
            },
            name,
            fqn,
            parent_fqn: parent.map(str::to_owned),
            signature,
            visibility: Some(m.visibility.unwrap_or_else(|| "internal".into())),
            is_static: m.is_static,
            is_abstract: n.kind() == "protocol_function_declaration",
            is_final: m.is_final,
            start_line: line(n),
            end_line: end_line(n),
            doc_summary: self.doc(n),
            meta: SymbolMeta {
                attributes: m.attributes,
                params: self.params(n),
                return_type,
                ..SymbolMeta::default()
            },
        });
    }

    fn property(&mut self, n: Node, parent: Option<&str>) {
        let m = self.modifiers(n);
        let is_let = {
            let mut cur = n.walk();
            n.children(&mut cur)
                .any(|c| c.kind() == "value_binding_pattern" && self.text(c).starts_with("let"))
        };
        let wrapper_at = m.attributes.iter().position(|a| is_capitalised(a));
        let wrapper = wrapper_at.map(|i| m.attributes[i].clone());
        let wrapper_arg = wrapper_at.and_then(|i| m.attribute_args.get(i).cloned().flatten());
        let mut cur = n.walk();
        let names: Vec<Node> = n.children_by_field_name("name", &mut cur).collect();
        let ty = named_children(n)
            .into_iter()
            .find(|c| c.kind() == "type_annotation")
            .map(|t| {
                let t = t.child_by_field_name("name").unwrap_or(t);
                squash(self.text(t).trim_start_matches(':').trim())
            });
        let value = n
            .child_by_field_name("value")
            .map(|v| squash(self.text(v)))
            .map(|v| v.chars().take(120).collect::<String>());
        let computed = n.child_by_field_name("computed_value").is_some()
            || named_children(n)
                .iter()
                .any(|c| c.kind() == "computed_property");
        let header_end = n
            .child_by_field_name("computed_value")
            .map(|b| b.start_byte())
            .unwrap_or(n.end_byte());
        let signature =
            squash(std::str::from_utf8(&self.src[n.start_byte()..header_end]).unwrap_or(""));
        let signature = signature
            .split_whitespace()
            .filter(|w| !w.starts_with('@') || parent.is_none())
            .collect::<Vec<_>>()
            .join(" ");
        let signature: String = signature.chars().take(200).collect();
        for name_node in names {
            let name = self
                .text(name_node)
                .trim_start_matches('(')
                .trim()
                .to_owned();
            if name.is_empty() || name.contains(',') {
                continue;
            }
            let kind = match (parent, is_let) {
                (None, true) => SymbolKind::Const,
                (Some(_), true) if m.is_static => SymbolKind::Const,
                _ => SymbolKind::Property,
            };
            let fqn = match parent {
                Some(p) => format!("{p}.{name}"),
                None => name.clone(),
            };
            self.out.symbols.push(NewSymbol {
                kind,
                name: name.clone(),
                fqn,
                parent_fqn: parent.map(str::to_owned),
                signature: signature.clone(),
                visibility: Some(m.visibility.clone().unwrap_or_else(|| "internal".into())),
                is_static: m.is_static,
                is_abstract: n.kind() == "protocol_property_declaration",
                is_final: false,
                start_line: line(n),
                end_line: end_line(n),
                doc_summary: self.doc(n),
                meta: SymbolMeta {
                    attributes: m.attributes.clone(),
                    r#type: ty.clone(),
                    value: value.clone(),
                    readonly: is_let,
                    ..SymbolMeta::default()
                },
            });
            if let Some(p) = parent {
                self.props.push((
                    p.to_owned(),
                    Prop {
                        name,
                        r#type: ty.clone(),
                        wrapper: wrapper.clone(),
                        wrapper_arg: wrapper_arg.clone(),
                        default: value.clone(),
                        is_let,
                        is_static: m.is_static,
                        computed,
                        line: line(n),
                    },
                ));
            }
        }
    }

    fn enum_cases(&mut self, n: Node, parent: Option<&str>) {
        let Some(p) = parent else { return };
        let mut cur = n.walk();
        let names: Vec<Node> = n.children_by_field_name("name", &mut cur).collect();
        let mut cur = n.walk();
        let raws: Vec<Node> = n.children_by_field_name("raw_value", &mut cur).collect();
        for (i, name_node) in names.iter().enumerate() {
            let name = self.text(*name_node).to_owned();
            self.out.symbols.push(NewSymbol {
                kind: SymbolKind::EnumCase,
                name: name.clone(),
                fqn: format!("{p}.{name}"),
                parent_fqn: Some(p.to_owned()),
                signature: format!("case {name}"),
                visibility: None,
                is_static: true,
                is_abstract: false,
                is_final: false,
                start_line: line(n),
                end_line: end_line(n),
                doc_summary: self.doc(n),
                meta: SymbolMeta {
                    value: raws.get(i).map(|r| squash(self.text(*r))),
                    ..SymbolMeta::default()
                },
            });
        }
    }

    fn typealias(&mut self, n: Node, parent: Option<&str>) {
        let Some(name_node) = n.child_by_field_name("name") else {
            return;
        };
        let name = self.text(name_node).to_owned();
        let value = n.child_by_field_name("value").map(|v| squash(self.text(v)));
        self.out.symbols.push(NewSymbol {
            kind: SymbolKind::Const,
            name: name.clone(),
            fqn: match parent {
                Some(p) => format!("{p}.{name}"),
                None => name,
            },
            parent_fqn: parent.map(str::to_owned),
            signature: squash(self.text(n)),
            visibility: Some(
                self.modifiers(n)
                    .visibility
                    .unwrap_or_else(|| "internal".into()),
            ),
            is_static: true,
            is_abstract: false,
            is_final: false,
            start_line: line(n),
            end_line: end_line(n),
            doc_summary: self.doc(n),
            meta: SymbolMeta {
                r#type: Some("typealias".into()),
                value,
                ..SymbolMeta::default()
            },
        });
    }

    /// The type a node is declared in, by line range.
    fn owner_at(&self, n: Node) -> Option<usize> {
        let l = line(n);
        // Innermost: the matching type with the latest start.
        self.out
            .facts
            .types
            .iter()
            .enumerate()
            .filter(|(_, t)| t.line <= l && l <= t.end_line)
            .max_by_key(|(_, t)| t.line)
            .map(|(i, _)| i)
    }

    fn push_ref(&mut self, kind: &'static str, target: &str, n: Node) {
        if target.is_empty() || target.len() > 120 {
            return;
        }
        self.out.refs.push(Ref {
            kind,
            target: target.to_owned(),
            line: line(n),
        });
    }

    /// References, SwiftUI screens and translation keys, over the whole tree.
    fn walk_refs(&mut self, n: Node) {
        match n.kind() {
            "user_type" => {
                if let Some(t) = named_children(n)
                    .into_iter()
                    .find(|c| c.kind() == "type_identifier")
                {
                    let name = self.text(t).to_owned();
                    if is_capitalised(&name) {
                        self.push_ref("swift_type", &name, t);
                    }
                }
            }
            "call_expression" => self.call(n),
            "navigation_expression" => {
                if let Some(s) = n
                    .child_by_field_name("suffix")
                    .and_then(|s| s.child_by_field_name("suffix"))
                {
                    let name = self.text(s).to_owned();
                    self.push_ref("swift_member", &name, s);
                }
                if let Some(t) = n.child_by_field_name("target")
                    && t.kind() == "simple_identifier"
                {
                    let name = self.text(t).to_owned();
                    if is_capitalised(&name) {
                        self.push_ref("swift_type", &name, t);
                    }
                }
            }
            _ => {}
        }
        // A declaration's own name is a `type_identifier`, not a `user_type`,
        // so it is not counted; an extension's name is a use of the type.
        for c in named_children(n) {
            self.walk_refs(c);
        }
    }

    fn call(&mut self, n: Node) {
        let Some(callee) = n.named_child(0) else {
            return;
        };
        // `at` is the name itself, so a modifier's line is its own, not the
        // line where the chain of modifiers starts.
        let (name, is_modifier, at) = match callee.kind() {
            "simple_identifier" => (self.text(callee).to_owned(), false, callee),
            "navigation_expression" => match callee
                .child_by_field_name("suffix")
                .and_then(|s| s.child_by_field_name("suffix"))
            {
                Some(s) => (self.text(s).to_owned(), true, s),
                None => return,
            },
            _ => return,
        };
        if !is_modifier {
            let kind = if is_capitalised(&name) {
                "swift_type"
            } else {
                "swift_call"
            };
            self.push_ref(kind, &name, callee);
            if kind == "swift_type"
                && let Some(i) = self.owner_at(n)
            {
                let t = &mut self.out.facts.types[i];
                if !t.calls.iter().any(|c| c.name == name) {
                    t.calls.push(NamedLine {
                        name: name.clone(),
                        line: line(n),
                    });
                }
            }
        }
        let Some(suffix) = named_children(n)
            .into_iter()
            .find(|c| c.kind() == "call_suffix")
        else {
            return;
        };
        let args: Vec<(Option<String>, Node)> = named_children(suffix)
            .into_iter()
            .filter(|c| c.kind() == "value_arguments")
            .flat_map(named_children)
            .filter(|a| a.kind() == "value_argument")
            .map(|a| {
                let label = a
                    .child_by_field_name("name")
                    .map(|l| self.text(l).trim_end_matches(':').trim().to_owned());
                let value = a.child_by_field_name("value").unwrap_or(a);
                (label, value)
            })
            .collect();
        let lambdas: Vec<Node> = descendants_shallow(suffix, "lambda_literal");
        self.i18n(&name, is_modifier, &args, at);
        if let Some(kind) = screen_kind(&name) {
            // `.sheet` and friends are modifiers; `Tab`, `WindowGroup` are calls.
            let is_scene_call = !matches!(
                name.as_str(),
                "navigationDestination" | "sheet" | "fullScreenCover" | "popover" | "inspector"
            );
            if is_scene_call == is_modifier {
                return;
            }
            let mut candidates = Vec::new();
            let arg = |label: &str| args.iter().find(|(l, _)| l.as_deref() == Some(label));
            if name == "NavigationLink" {
                if arg("value").is_some() {
                    // Pushes whatever `.navigationDestination` shows; its
                    // closure is only the label.
                    return;
                }
                match arg("destination") {
                    Some((_, v)) => self.capitalised_calls(*v, &mut candidates),
                    // `NavigationLink { Destination() } label: { … }`
                    None => {
                        if let Some(l) = lambdas.first() {
                            self.capitalised_calls(*l, &mut candidates)
                        }
                    }
                }
            } else {
                for l in &lambdas {
                    self.capitalised_calls(*l, &mut candidates);
                }
            }
            if candidates.is_empty() {
                return;
            }
            let title = args
                .iter()
                .find(|(l, _)| l.is_none())
                .and_then(|(_, v)| self.string_literal(*v));
            let owner = self
                .owner_at(n)
                .map(|i| self.out.facts.types[i].name.clone());
            self.out.facts.screens.push(Screen {
                kind: kind.to_owned(),
                via: if is_modifier {
                    format!(".{name}")
                } else {
                    name.clone()
                },
                candidates,
                title,
                owner,
                line: line(at),
            });
        } else if name == "TabView" && !is_modifier {
            // Each child of a `TabView` is a tab.
            for l in &lambdas {
                let Some(stmts) = named_children(*l)
                    .into_iter()
                    .find(|c| c.kind() == "statements")
                else {
                    continue;
                };
                for s in named_children(stmts) {
                    let mut candidates = Vec::new();
                    if let Some(root) = root_call(s, self) {
                        candidates.push(root);
                    }
                    self.capitalised_calls(s, &mut candidates);
                    if candidates.is_empty() || candidates[0] == "Tab" {
                        continue;
                    }
                    let owner = self
                        .owner_at(n)
                        .map(|i| self.out.facts.types[i].name.clone());
                    self.out.facts.screens.push(Screen {
                        kind: "tab".into(),
                        via: "TabView".into(),
                        candidates,
                        title: None,
                        owner,
                        line: line(s),
                    });
                }
            }
        }
    }

    /// Capitalised calls in a subtree, outermost first, no repeats.
    fn capitalised_calls(&self, n: Node, out: &mut Vec<String>) {
        if n.kind() == "call_expression"
            && let Some(c) = n.named_child(0)
            && c.kind() == "simple_identifier"
        {
            let name = self.text(c);
            if is_capitalised(name) && !out.iter().any(|o| o == name) {
                out.push(name.to_owned());
            }
        }
        for c in named_children(n) {
            self.capitalised_calls(c, out);
        }
    }

    /// A plain string literal's text; `None` when it has interpolation.
    fn string_literal(&self, n: Node) -> Option<String> {
        if n.kind() != "line_string_literal" && n.kind() != "multi_line_string_literal" {
            return None;
        }
        let parts = named_children(n);
        if parts.iter().any(|p| p.kind().contains("interpolat")) {
            return None;
        }
        let mut s = String::new();
        for p in parts {
            match p.kind() {
                "line_str_text" | "multi_line_str_text" => s.push_str(self.text(p)),
                "str_escaped_char" => s.push_str(self.text(p)),
                _ => {}
            }
        }
        Some(s)
    }

    fn i18n(&mut self, name: &str, is_modifier: bool, args: &[(Option<String>, Node)], n: Node) {
        let first_plain = || args.iter().find(|(l, _)| l.is_none()).map(|(_, v)| *v);
        let (value, guarded) = if !is_modifier && name == "NSLocalizedString" {
            (first_plain(), false)
        } else if !is_modifier && name == "String" {
            match args.iter().find(|(l, _)| l.as_deref() == Some("localized")) {
                Some((_, v)) => (Some(*v), false),
                None => return,
            }
        } else if (!is_modifier && LOCALIZED_VIEWS.contains(&name))
            || (is_modifier && LOCALIZED_MODIFIERS.contains(&name))
        {
            // `Text(verbatim:)` and `Text(someString)` are not looked up.
            (first_plain(), true)
        } else {
            return;
        };
        let Some(v) = value else { return };
        let Some(key) = self.string_literal(v) else {
            return;
        };
        if key.trim().is_empty() {
            return;
        }
        self.out.facts.i18n_uses.push(I18nUse {
            key,
            line: line(n),
            guarded,
        });
    }
}

/// `ContentView().padding().tabItem { … }` → `ContentView`.
fn root_call(n: Node, cx: &Cx) -> Option<String> {
    let mut cur = n;
    loop {
        match cur.kind() {
            "call_expression" => {
                let c = cur.named_child(0)?;
                if c.kind() == "simple_identifier" {
                    let name = cx.text(c);
                    return is_capitalised(name).then(|| name.to_owned());
                }
                cur = c;
            }
            "navigation_expression" => cur = cur.child_by_field_name("target")?,
            _ => return None,
        }
    }
}

/// Nodes of `kind` under `n`, not looking inside a match.
fn descendants_shallow<'t>(n: Node<'t>, kind: &str) -> Vec<Node<'t>> {
    let mut out = Vec::new();
    for c in named_children(n) {
        if c.kind() == kind {
            out.push(c);
        } else if c.kind() == "value_arguments" || c.kind() == "value_argument" {
            continue;
        } else {
            out.extend(descendants_shallow(c, kind));
        }
    }
    out
}

#[derive(Default)]
struct Modifiers {
    attributes: Vec<String>,
    /// Argument of each attribute, by position in `attributes`.
    attribute_args: Vec<Option<String>>,
    visibility: Option<String>,
    is_static: bool,
    is_final: bool,
}

fn is_info_plist_table(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    file.starts_with("InfoPlist.")
}

/// The language of `xx.lproj/Name.strings`, and its keys.
pub fn strings_file(path: &str, src: &str) -> Option<LocaleKeys> {
    // `InfoPlist.strings` translates Info.plist entries (the app's name,
    // permission prompts), not texts used in code.
    if is_info_plist_table(path) {
        return None;
    }
    let dir = path.rsplit('/').nth(1)?;
    let locale = dir.strip_suffix(".lproj")?;
    let locale = if locale == "Base" { "base" } else { locale };
    let mut keys = Vec::new();
    let b = src.as_bytes();
    let mut i = 0;
    let mut expect_key = true;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            b'"' => {
                let start = i + 1;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                if expect_key {
                    let raw = &src[start..i.min(src.len())];
                    keys.push(raw.replace("\\\"", "\"").replace("\\n", "\n"));
                }
                expect_key = false;
                i += 1;
            }
            b';' => {
                expect_key = true;
                i += 1;
            }
            _ => i += 1,
        }
    }
    Some(LocaleKeys {
        locale: locale.to_owned(),
        keys,
    })
}

/// Keys per language from a `.xcstrings` catalog. The source language holds
/// every key, translated or not.
pub fn string_catalog(src: &str) -> Vec<LocaleKeys> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(src) else {
        return Vec::new();
    };
    let source = v
        .get("sourceLanguage")
        .and_then(|s| s.as_str())
        .unwrap_or("en")
        .to_owned();
    let mut by_locale: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    let Some(strings) = v.get("strings").and_then(|s| s.as_object()) else {
        return Vec::new();
    };
    for (key, entry) in strings {
        if key.is_empty() {
            continue;
        }
        by_locale
            .entry(source.clone())
            .or_default()
            .push(key.clone());
        if let Some(locs) = entry.get("localizations").and_then(|l| l.as_object()) {
            for loc in locs.keys() {
                if loc != &source {
                    by_locale.entry(loc.clone()).or_default().push(key.clone());
                }
            }
        }
    }
    by_locale
        .into_iter()
        .map(|(locale, mut keys)| {
            keys.sort();
            LocaleKeys { locale, keys }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP: &str = r#"import SwiftUI

@main
struct DemoApp: App {
    @StateObject private var store = ItemStore()
    var body: some Scene {
        WindowGroup("Items") {
            NavigationStack { ContentView() }
        }
        Settings { SettingsView() }
    }
}

/// Loads and saves items.
protocol Store { func load() async throws -> [Item] }

struct ContentView: View {
    @State private var items: [Item] = []
    @Binding var selected: Item?
    let title: String
    @State private var showAdd = false

    var body: some View {
        List(items) { item in
            NavigationLink(destination: DetailView(item: item)) {
                Text(item.name)
            }
        }
        .navigationTitle("Items")
        .sheet(isPresented: $showAdd) { AddItemView() }
        .toolbar { Button("Add") { showAdd = true } }
        Text(NSLocalizedString("items.empty", comment: ""))
        Text(String(localized: "items.count \(items.count)"))
    }

    private func reload(id: Int, force: Bool = false) -> Bool {
        store.load()
        return ItemStore.shared.isReady
    }
}

enum Filter: String, CaseIterable {
    case all = "all", done
    case open
}

extension ContentView {
    struct Row: View {
        var body: some View { TabView { HomeView().tabItem { Label("Home", systemImage: "house") }; ProfileView() } }
    }
}
"#;

    #[test]
    fn swiftui_file() {
        let x = extract_file("App/ContentView.swift", APP);
        let syms: Vec<String> = x
            .symbols
            .iter()
            .map(|s| format!("{} {} [{}]", s.kind.as_str(), s.fqn, s.signature))
            .collect();
        for s in &syms {
            println!("{s}");
        }
        println!("{}", serde_json::to_string_pretty(&x.facts).unwrap());
        let mut refs: Vec<String> = x
            .refs
            .iter()
            .map(|r| format!("{}:{}:{}", r.kind, r.target, r.line))
            .collect();
        refs.sort();
        println!("{refs:?}");
        assert!(syms.contains(&"struct DemoApp [struct DemoApp: App]".to_owned()));
        assert!(syms.iter().any(|s| s.starts_with("protocol Store ")));
        assert!(syms.iter().any(|s| s.starts_with("method Store.load ")));
        assert!(
            syms.iter()
                .any(|s| s.starts_with("struct ContentView.Row "))
        );
        assert!(syms.iter().any(|s| s.starts_with("enum_case Filter.done ")));
        assert!(
            syms.iter()
                .any(|s| s.starts_with("method ContentView.reload "))
        );
        let screens: Vec<(String, String)> = x
            .facts
            .screens
            .iter()
            .map(|s| (s.kind.clone(), s.candidates.join(",")))
            .collect();
        assert_eq!(
            screens,
            [
                ("window".into(), "NavigationStack,ContentView".into()),
                ("settings".into(), "SettingsView".into()),
                ("sheet".into(), "AddItemView".into()),
                ("navigation".into(), "DetailView".into()),
                ("tab".into(), "HomeView,Label".into()),
                ("tab".into(), "ProfileView".into()),
            ]
        );
        let keys: Vec<(String, bool)> = x
            .facts
            .i18n_uses
            .iter()
            .map(|u| (u.key.clone(), u.guarded))
            .collect();
        assert_eq!(
            keys,
            [
                ("Items".into(), true),
                ("Add".into(), true),
                ("items.empty".into(), false),
                ("Home".into(), true),
            ]
        );
        let view = x
            .facts
            .types
            .iter()
            .find(|t| t.name == "ContentView")
            .unwrap();
        assert_eq!(view.conforms, ["View"]);
        let props: Vec<(&str, Option<&str>)> = view
            .props
            .iter()
            .map(|p| (p.name.as_str(), p.wrapper.as_deref()))
            .collect();
        assert_eq!(
            props,
            [
                ("items", Some("State")),
                ("selected", Some("Binding")),
                ("title", None),
                ("showAdd", Some("State")),
                ("body", None),
            ]
        );
        assert!(refs.contains(&"swift_type:ItemStore:38".to_owned()));
        assert!(refs.contains(&"swift_member:load:37".to_owned()));
        assert!(!refs.iter().any(|r| r.starts_with("swift_type:main")));
        assert_eq!(x.facts.i18n_uses[0].line, 29);
    }

    #[test]
    fn navigation_link_forms() {
        let src = "struct L: View { var body: some View {\n NavigationLink(value: b) { BookRow(b) }\n NavigationLink { Detail() } label: { Row() }\n NavigationLink(\"Go\", destination: Other())\n} }\n";
        let x = extract_file("L.swift", src);
        let got: Vec<(String, Option<String>)> = x
            .facts
            .screens
            .iter()
            .map(|s| (s.candidates.join(","), s.title.clone()))
            .collect();
        assert_eq!(
            got,
            [("Detail".into(), None), ("Other".into(), Some("Go".into()))]
        );
    }

    #[test]
    fn strings_and_catalogs() {
        let s = strings_file(
            "App/ms.lproj/Localizable.strings",
            "/* c */\n\"save\" = \"Simpan\";\n// x\n\"say \\\"hi\\\"\" = \"x\";\n",
        )
        .unwrap();
        assert_eq!(s.locale, "ms");
        assert_eq!(s.keys, ["save", "say \"hi\""]);
        let c = string_catalog(
            r#"{"sourceLanguage":"en","strings":{"Save":{"localizations":{"ms":{"stringUnit":{"value":"Simpan"}}}},"Cancel":{}},"version":"1.0"}"#,
        );
        assert_eq!(
            c,
            [
                LocaleKeys {
                    locale: "en".into(),
                    keys: vec!["Cancel".into(), "Save".into()]
                },
                LocaleKeys {
                    locale: "ms".into(),
                    keys: vec!["Save".into()]
                },
            ]
        );
    }
}
