//! Per-file extraction for Kotlin, Java and Android resource files: types and
//! members become symbols, names used become references, and Compose
//! functions, screens, manifest components and translation keys are kept as
//! per-file facts.

use codemap_core::symbols::{NewSymbol, Param, SymbolKind, SymbolMeta};
use serde::{Deserialize, Serialize};
use tree_sitter::Node;

/// A name and where it appears.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedLine {
    pub name: String,
    pub line: u32,
}

/// A class, interface, object or enum.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeFacts {
    pub name: String,
    pub fqn: String,
    /// class | interface | object | enum | annotation
    pub kind: String,
    pub line: u32,
    pub end_line: u32,
    /// Superclass and interfaces, as written (`ComponentActivity`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supertypes: Vec<String>,
}

/// A local held across recompositions: `var tab by remember { … }`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateVar {
    pub name: String,
    /// The call that makes it: `remember`, `rememberSaveable`, `collectAsState`, `viewModel`.
    pub via: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial: Option<String>,
    pub line: u32,
}

/// A `@Composable` function.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Composable {
    pub name: String,
    pub fqn: String,
    pub line: u32,
    pub end_line: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<Param>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state: Vec<StateVar>,
    /// Capitalised names called in the body, in order: child composables
    /// once matched with the project's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<NamedLine>,
    #[serde(default)]
    pub private: bool,
}

/// Where the app shows a screen.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Screen {
    /// navigation | dialog | sheet | switch | content
    pub kind: String,
    /// What opens it: `composable`, `ModalBottomSheet`, `when`, `setContent`.
    pub via: String,
    /// Capitalised calls inside it, outermost first; the screen is the first
    /// that is a project composable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<String>,
    /// The route (`"orders/{id}"`, `Detail`) or the `when` branch (`is Route.Detail`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The function or class it is declared in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Branches of one `when` share its line: a `when` is a screen switch only
    /// when two or more branches show project composables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<u32>,
    pub line: u32,
}

/// A component declared in `AndroidManifest.xml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestComponent {
    /// activity | activity-alias | service | receiver | provider
    pub kind: String,
    /// As written: `.MainActivity`, `com.example.MainActivity`.
    pub name: String,
    #[serde(default)]
    pub launcher: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exported: Option<bool>,
    pub line: u32,
}

/// A translation key used in code or XML.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct I18nUse {
    pub key: String,
    pub line: u32,
}

/// Keys of one language from a `res/values*/` file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocaleKeys {
    pub locale: String,
    pub keys: Vec<String>,
}

/// A reference to a name: kind is `jvm_type`, `jvm_call` or `jvm_member`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Ref {
    pub kind: &'static str,
    pub target: String,
    pub line: u32,
}

/// Everything known about one file, stored as `android_files.facts_json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AndroidFacts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<TypeFacts>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub composables: Vec<Composable>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screens: Vec<Screen>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub manifest: Vec<ManifestComponent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub i18n_uses: Vec<I18nUse>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locales: Vec<LocaleKeys>,
    /// Strings marked `translatable="false"`: not expected in other languages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub untranslatable: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<NamedLine>,
    pub lines: u32,
}

#[derive(Debug, Default)]
pub struct Extracted {
    pub facts: AndroidFacts,
    pub symbols: Vec<NewSymbol>,
    pub refs: Vec<Ref>,
}

/// Calls whose value lives across recompositions.
const STATE_CALLS: &[&str] = &[
    "remember",
    "rememberSaveable",
    "mutableStateOf",
    "mutableIntStateOf",
    "mutableLongStateOf",
    "mutableFloatStateOf",
    "mutableDoubleStateOf",
    "mutableStateListOf",
    "mutableStateMapOf",
    "derivedStateOf",
    "produceState",
    "collectAsState",
    "collectAsStateWithLifecycle",
    "observeAsState",
    "viewModel",
    "hiltViewModel",
    "koinViewModel",
];

/// Compose calls that show a dialog or a sheet.
fn overlay_kind(call: &str) -> Option<&'static str> {
    Some(match call {
        "AlertDialog" | "BasicAlertDialog" | "Dialog" | "DatePickerDialog" | "TimePickerDialog" => {
            "dialog"
        }
        "ModalBottomSheet" | "BottomSheetScaffold" | "ModalBottomSheetLayout" => "sheet",
        "Popup" | "DropdownMenu" => return None,
        _ => return None,
    })
}

pub fn extract_file(path: &str, src: &str) -> Extracted {
    let lines = src.lines().count() as u32;
    let mut out = Extracted::default();
    out.facts.lines = lines;
    if path.ends_with(".xml") {
        xml_file(path, src, &mut out.facts);
        return out;
    }
    let java = path.ends_with(".java");
    let tree = if java {
        crate::parse::java(src.as_bytes())
    } else {
        crate::parse::kotlin(src.as_bytes())
    };
    let Some(tree) = tree else { return out };
    let mut cx = Cx {
        src: src.as_bytes(),
        out: &mut out,
        package: None,
    };
    let root = tree.root_node();
    cx.errors(root);
    if java {
        cx.java_decls(root, None);
        cx.java_refs(root);
    } else {
        cx.kt_decls(root, None, false);
        cx.kt_refs(root);
    }
    let mut seen = std::collections::HashSet::new();
    out.refs
        .retain(|r| seen.insert((r.kind, r.target.clone(), r.line)));
    out
}

struct Cx<'a> {
    src: &'a [u8],
    out: &'a mut Extracted,
    package: Option<String>,
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

fn children(n: Node) -> Vec<Node> {
    let mut c = n.walk();
    n.children(&mut c).collect()
}

fn is_capitalised(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `List<Item>?` → `List`.
fn base_type(s: &str) -> String {
    s.split(['<', '(', ' ', '?'])
        .next()
        .unwrap_or(s)
        .trim()
        .to_owned()
}

#[derive(Default)]
struct Mods {
    annotations: Vec<String>,
    visibility: Option<String>,
    words: Vec<String>,
}

impl Mods {
    fn has(&self, w: &str) -> bool {
        self.words.iter().any(|x| x == w)
    }
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

    fn doc(&self, n: Node) -> Option<String> {
        let p = n.prev_named_sibling()?;
        if !p.kind().contains("comment") || end_line(p) + 1 < line(n) {
            return None;
        }
        let t = self.text(p);
        if !t.starts_with("/**") {
            return None;
        }
        let body = t.trim_start_matches("/**").trim_end_matches("*/");
        let body: String = body
            .lines()
            .map(|l| l.trim().trim_start_matches('*').trim())
            .filter(|l| !l.starts_with('@'))
            .collect::<Vec<_>>()
            .join(" ");
        let doc = squash(&body);
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

    /// Text from the start of `n` to the start of `stop` (its body), squashed,
    /// without annotations.
    fn header(&self, n: Node, stop: Option<Node>) -> String {
        let end = stop.map(|b| b.start_byte()).unwrap_or(n.end_byte());
        let mut start = n.start_byte();
        // Skip modifiers that are only annotations.
        if let Some(m) = named_children(n)
            .into_iter()
            .find(|c| c.kind() == "modifiers")
        {
            let only_annotations = named_children(m)
                .iter()
                .all(|c| c.kind().contains("annotation"));
            if only_annotations {
                start = m.end_byte();
            }
        }
        let s = std::str::from_utf8(&self.src[start.min(end)..end]).unwrap_or("");
        let s = squash(s);
        let s: String = s
            .split_whitespace()
            .filter(|w| !w.starts_with('@'))
            .collect::<Vec<_>>()
            .join(" ");
        s.chars().take(240).collect()
    }

    fn fqn(&self, parent: Option<&str>, name: &str) -> String {
        match (parent, &self.package) {
            (Some(p), _) => format!("{p}.{name}"),
            (None, Some(pkg)) => format!("{pkg}.{name}"),
            (None, None) => name.to_owned(),
        }
    }

    // ------------------------------------------------------------ Kotlin

    fn kt_mods(&self, n: Node) -> Mods {
        let mut m = Mods::default();
        for c in named_children(n) {
            if c.kind() != "modifiers" {
                continue;
            }
            for k in named_children(c) {
                let t = self.text(k).trim();
                match k.kind() {
                    "annotation" => {
                        let name = t.trim_start_matches('@');
                        let name = name.split(['(', ' ']).next().unwrap_or(name);
                        let name = name.rsplit(':').next().unwrap_or(name);
                        m.annotations.push(name.to_owned());
                    }
                    "visibility_modifier" => m.visibility = Some(t.to_owned()),
                    _ => m.words.push(t.to_owned()),
                }
            }
        }
        m
    }

    fn kt_decls(&mut self, n: Node, parent: Option<&str>, in_object: bool) {
        for c in named_children(n) {
            match c.kind() {
                "package_header" => {
                    if let Some(q) = named_children(c).first() {
                        self.package = Some(self.text(*q).to_owned());
                        self.out.facts.package = self.package.clone();
                    }
                }
                "import" | "import_header" => {
                    if let Some(q) = named_children(c).first() {
                        self.out.facts.imports.push(self.text(*q).to_owned());
                    }
                }
                "import_list" => self.kt_decls(c, parent, in_object),
                "class_declaration" | "object_declaration" => self.kt_type(c, parent),
                "companion_object" => {
                    if let Some(b) = named_children(c)
                        .into_iter()
                        .find(|x| x.kind() == "class_body")
                    {
                        self.kt_decls(b, parent, true);
                    }
                }
                "function_declaration" => self.kt_function(c, parent, in_object),
                "property_declaration" => self.kt_property(c, parent, in_object),
                "type_alias" => self.kt_typealias(c, parent),
                "class_body" | "enum_class_body" => self.kt_decls(c, parent, in_object),
                "enum_entry" => {
                    if let (Some(p), Some(id)) = (
                        parent,
                        named_children(c)
                            .into_iter()
                            .find(|x| x.kind() == "identifier" || x.kind() == "simple_identifier"),
                    ) {
                        let name = self.text(id).to_owned();
                        let value = named_children(c)
                            .into_iter()
                            .find(|x| x.kind() == "value_arguments")
                            .map(|v| squash(self.text(v)));
                        self.out.symbols.push(NewSymbol {
                            kind: SymbolKind::EnumCase,
                            name: name.clone(),
                            fqn: format!("{p}.{name}"),
                            parent_fqn: Some(p.to_owned()),
                            signature: name,
                            visibility: None,
                            is_static: true,
                            is_abstract: false,
                            is_final: false,
                            start_line: line(c),
                            end_line: end_line(c),
                            doc_summary: self.doc(c),
                            meta: SymbolMeta {
                                value,
                                ..SymbolMeta::default()
                            },
                        });
                    }
                }
                _ => {}
            }
        }
    }

    fn kt_type(&mut self, n: Node, parent: Option<&str>) {
        let Some(name_node) = n.child_by_field_name("name") else {
            return;
        };
        let name = self.text(name_node).to_owned();
        let m = self.kt_mods(n);
        let keywords: Vec<&str> = children(n)
            .into_iter()
            .filter(|c| !c.is_named())
            .map(|c| c.kind())
            .collect();
        let kind = if n.kind() == "object_declaration" {
            "object"
        } else if keywords.contains(&"interface") {
            "interface"
        } else if m.has("enum") {
            "enum"
        } else if m.has("annotation") {
            "annotation"
        } else {
            "class"
        };
        let fqn = self.fqn(parent, &name);
        let mut supertypes = Vec::new();
        for c in named_children(n) {
            if c.kind() == "delegation_specifiers" {
                for d in named_children(c) {
                    let t = base_type(self.text(d));
                    if !t.is_empty() {
                        supertypes.push(t);
                    }
                }
            } else if c.kind() == "delegation_specifier" {
                supertypes.push(base_type(self.text(c)));
            }
        }
        let body = named_children(n)
            .into_iter()
            .find(|c| c.kind() == "class_body" || c.kind() == "enum_class_body");
        let mut attributes = m.annotations.clone();
        for w in ["data", "sealed", "value", "inner", "abstract", "open"] {
            if m.has(w) {
                attributes.push(w.to_owned());
            }
        }
        if kind == "object" {
            attributes.push("object".into());
        }
        self.out.symbols.push(NewSymbol {
            kind: match kind {
                "interface" | "annotation" => SymbolKind::Interface,
                "enum" => SymbolKind::Enum,
                _ => SymbolKind::Class,
            },
            name: name.clone(),
            fqn: fqn.clone(),
            parent_fqn: parent.map(str::to_owned),
            signature: self.header(n, body),
            visibility: Some(m.visibility.clone().unwrap_or_else(|| "public".into())),
            is_static: false,
            is_abstract: m.has("abstract") || m.has("sealed"),
            is_final: !(m.has("open") || m.has("abstract") || m.has("sealed")) && kind == "class",
            start_line: line(n),
            end_line: end_line(n),
            doc_summary: self.doc(n),
            meta: SymbolMeta {
                extends: supertypes.first().cloned().into_iter().collect(),
                implements: supertypes.iter().skip(1).cloned().collect(),
                attributes,
                ..SymbolMeta::default()
            },
        });
        self.out.facts.types.push(TypeFacts {
            name: name.clone(),
            fqn: fqn.clone(),
            kind: kind.to_owned(),
            line: line(n),
            end_line: end_line(n),
            supertypes,
        });
        // `class X(val a: Int)`: constructor properties.
        for pc in named_children(n)
            .into_iter()
            .filter(|c| c.kind() == "primary_constructor")
        {
            for cp in named_children(pc)
                .into_iter()
                .filter(|c| c.kind() == "class_parameters")
                .flat_map(named_children)
                .filter(|c| c.kind() == "class_parameter")
            {
                let binding = children(cp)
                    .into_iter()
                    .find(|c| !c.is_named() && matches!(c.kind(), "val" | "var"));
                let Some(binding) = binding else { continue };
                let Some(id) = named_children(cp)
                    .into_iter()
                    .find(|c| c.kind() == "identifier" || c.kind() == "simple_identifier")
                else {
                    continue;
                };
                let pname = self.text(id).to_owned();
                let ty = self
                    .text(cp)
                    .split_once(':')
                    .map(|(_, t)| squash(t.split('=').next().unwrap_or(t).trim()));
                let cm = self.kt_mods(cp);
                self.out.symbols.push(NewSymbol {
                    kind: SymbolKind::Property,
                    name: pname.clone(),
                    fqn: format!("{fqn}.{pname}"),
                    parent_fqn: Some(fqn.clone()),
                    signature: self.header(cp, None),
                    visibility: Some(cm.visibility.unwrap_or_else(|| "public".into())),
                    is_static: false,
                    is_abstract: false,
                    is_final: false,
                    start_line: line(cp),
                    end_line: end_line(cp),
                    doc_summary: None,
                    meta: SymbolMeta {
                        r#type: ty,
                        readonly: binding.kind() == "val",
                        ..SymbolMeta::default()
                    },
                });
            }
        }
        if let Some(b) = body {
            self.kt_decls(b, Some(&fqn), kind == "object");
        }
    }

    fn kt_params(&self, n: Node) -> Vec<Param> {
        let mut out: Vec<Param> = Vec::new();
        let Some(list) = named_children(n)
            .into_iter()
            .find(|c| c.kind() == "function_value_parameters")
        else {
            return out;
        };
        for c in named_children(list) {
            match c.kind() {
                "parameter" => {
                    let text = self.text(c);
                    let (name, ty) = match text.split_once(':') {
                        Some((a, b)) => (a.trim().to_owned(), Some(squash(b.trim()))),
                        None => (text.trim().to_owned(), None),
                    };
                    out.push(Param {
                        name,
                        r#type: ty,
                        ..Param::default()
                    });
                }
                "parameter_modifiers" | "line_comment" | "block_comment" => {}
                _ => {
                    if let Some(last) = out.last_mut() {
                        last.default = Some(squash(self.text(c)).chars().take(80).collect());
                    }
                }
            }
        }
        out
    }

    fn kt_function(&mut self, n: Node, parent: Option<&str>, in_object: bool) {
        let Some(name_node) = n.child_by_field_name("name") else {
            return;
        };
        let name = self.text(name_node).to_owned();
        let m = self.kt_mods(n);
        let body = named_children(n)
            .into_iter()
            .find(|c| c.kind() == "function_body");
        let signature = self.header(n, body);
        let fqn = self.fqn(parent, &name);
        let params = self.kt_params(n);
        let return_type = signature
            .rsplit_once("):")
            .map(|(_, r)| r.trim().to_owned())
            .filter(|r| !r.is_empty() && !r.contains('{'));
        let composable = m.annotations.iter().any(|a| a == "Composable");
        self.out.symbols.push(NewSymbol {
            kind: if parent.is_some() {
                SymbolKind::Method
            } else {
                SymbolKind::Function
            },
            name: name.clone(),
            fqn: fqn.clone(),
            parent_fqn: parent.map(str::to_owned),
            signature,
            visibility: Some(m.visibility.clone().unwrap_or_else(|| "public".into())),
            is_static: in_object,
            is_abstract: m.has("abstract") || body.is_none() && parent.is_some(),
            is_final: false,
            start_line: line(n),
            end_line: end_line(n),
            doc_summary: self.doc(n),
            meta: SymbolMeta {
                attributes: m.annotations.clone(),
                params: params.clone(),
                return_type,
                ..SymbolMeta::default()
            },
        });
        if composable {
            let mut state = Vec::new();
            if let Some(b) = body {
                let block = named_children(b)
                    .into_iter()
                    .find(|c| c.kind() == "block")
                    .unwrap_or(b);
                for st in named_children(block)
                    .into_iter()
                    .filter(|c| c.kind() == "property_declaration")
                {
                    if let Some(s) = self.kt_state(st) {
                        state.push(s);
                    }
                }
            }
            self.out.facts.composables.push(Composable {
                name,
                fqn,
                line: line(n),
                end_line: end_line(n),
                params,
                state,
                calls: Vec::new(),
                private: m.visibility.as_deref() == Some("private"),
            });
        }
    }

    /// `var tab by rememberSaveable { mutableIntStateOf(0) }` → `tab` via
    /// `rememberSaveable`, starting at `0`.
    fn kt_state(&self, n: Node) -> Option<StateVar> {
        let var = named_children(n)
            .into_iter()
            .find(|c| c.kind() == "variable_declaration")?;
        let name_node = named_children(var).into_iter().next()?;
        let name = self.text(name_node).to_owned();
        let value = named_children(n)
            .into_iter()
            .find(|c| c.kind() == "property_delegate" || c.kind().ends_with("expression"))?;
        let value = if value.kind() == "property_delegate" {
            named_children(value).into_iter().next()?
        } else {
            value
        };
        let via = self.call_name(value)?;
        if !STATE_CALLS.contains(&via.as_str()) {
            return None;
        }
        let initial = self
            .state_initial(value)
            .map(|v| squash(self.text(v)).chars().take(80).collect());
        Some(StateVar {
            name,
            via,
            initial,
            line: line(n),
        })
    }

    /// What a state call starts with: `remember { mutableStateOf(false) }` →
    /// `false`, `remember { SujudDetector(context) }` → `SujudDetector(context)`.
    fn state_initial<'t>(&self, call: Node<'t>) -> Option<Node<'t>> {
        let mut cur = call;
        for _ in 0..4 {
            // The trailing lambda's first statement, else the first argument.
            let lambda = lambdas_of(cur).into_iter().next();
            let content = match lambda {
                Some(l) => {
                    let first = named_children(l).into_iter().next()?;
                    if first.kind() == "statements" {
                        named_children(first).into_iter().next()?
                    } else {
                        first
                    }
                }
                None => {
                    let arg = descendant_kind(cur, "value_argument")?;
                    named_children(arg).into_iter().last()?
                }
            };
            match self.call_name(content) {
                Some(n) if STATE_CALLS.contains(&n.as_str()) => cur = content,
                _ => return Some(content),
            }
        }
        None
    }

    fn kt_property(&mut self, n: Node, parent: Option<&str>, in_object: bool) {
        let m = self.kt_mods(n);
        let Some(var) = named_children(n)
            .into_iter()
            .find(|c| c.kind() == "variable_declaration")
        else {
            return;
        };
        let Some(name_node) = named_children(var).into_iter().next() else {
            return;
        };
        let name = self.text(name_node).to_owned();
        let is_val = children(n)
            .into_iter()
            .any(|c| !c.is_named() && c.kind() == "val");
        let ty = self
            .text(var)
            .split_once(':')
            .map(|(_, t)| squash(t.trim()));
        let value = named_children(n)
            .into_iter()
            .skip_while(|c| c.kind() != "variable_declaration")
            .nth(1)
            .filter(|c| !matches!(c.kind(), "getter" | "setter" | "property_delegate"))
            .map(|v| squash(self.text(v)).chars().take(120).collect::<String>());
        let kind = if m.has("const") || (parent.is_none() && is_val) {
            SymbolKind::Const
        } else {
            SymbolKind::Property
        };
        self.out.symbols.push(NewSymbol {
            kind,
            name: name.clone(),
            fqn: self.fqn(parent, &name),
            parent_fqn: parent.map(str::to_owned),
            signature: self.header(n, None).chars().take(160).collect(),
            visibility: Some(m.visibility.clone().unwrap_or_else(|| "public".into())),
            is_static: in_object || m.has("const"),
            is_abstract: m.has("abstract"),
            is_final: false,
            start_line: line(n),
            end_line: end_line(n),
            doc_summary: self.doc(n),
            meta: SymbolMeta {
                attributes: m.annotations,
                r#type: ty,
                value,
                readonly: is_val,
                ..SymbolMeta::default()
            },
        });
    }

    fn kt_typealias(&mut self, n: Node, parent: Option<&str>) {
        let name_node = n.child_by_field_name("type").or_else(|| {
            named_children(n)
                .into_iter()
                .find(|c| c.kind() == "identifier")
        });
        let Some(name_node) = name_node else { return };
        let name = self.text(name_node).to_owned();
        let value = self.text(n).split_once('=').map(|(_, v)| squash(v.trim()));
        self.out.symbols.push(NewSymbol {
            kind: SymbolKind::Const,
            name: name.clone(),
            fqn: self.fqn(parent, &name),
            parent_fqn: parent.map(str::to_owned),
            signature: squash(self.text(n)),
            visibility: Some(
                self.kt_mods(n)
                    .visibility
                    .unwrap_or_else(|| "public".into()),
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

    /// The called name: `Foo(…)`, `Foo(…) { … }` → `Foo`; `a.b.c(…)` → `c`.
    fn call_name(&self, n: Node) -> Option<String> {
        if n.kind() != "call_expression" {
            return None;
        }
        let callee = n.named_child(0)?;
        match callee.kind() {
            "identifier" | "simple_identifier" => Some(self.text(callee).to_owned()),
            "call_expression" => self.call_name(callee),
            "navigation_expression" => {
                let last = named_children(callee).into_iter().last()?;
                Some(self.text(last).trim_start_matches('.').to_owned())
            }
            _ => None,
        }
    }

    /// `Foo.bar` → is it a member access of `navigation_expression` form.
    fn is_member_call(&self, n: Node) -> bool {
        let mut cur = n;
        while cur.kind() == "call_expression" {
            match cur.named_child(0) {
                Some(c) if c.kind() == "call_expression" => cur = c,
                Some(c) => return c.kind() == "navigation_expression",
                None => return false,
            }
        }
        false
    }

    /// The composable function a node is inside, else the type.
    fn owner_at(&self, n: Node) -> Option<String> {
        let l = line(n);
        self.out
            .facts
            .composables
            .iter()
            .filter(|c| c.line <= l && l <= c.end_line)
            .max_by_key(|c| c.line)
            .map(|c| c.name.clone())
            .or_else(|| {
                self.out
                    .facts
                    .types
                    .iter()
                    .filter(|t| t.line <= l && l <= t.end_line)
                    .max_by_key(|t| t.line)
                    .map(|t| t.name.clone())
            })
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

    /// `R.string.app_name` → `app_name`.
    fn resource_key(&self, n: Node) -> Option<String> {
        let t: String = self.text(n).split_whitespace().collect();
        let rest = t.strip_prefix("R.")?;
        let (kind, key) = rest.split_once('.')?;
        (matches!(kind, "string" | "plurals" | "array")
            && !key.is_empty()
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .then(|| key.to_owned())
    }

    fn capitalised_calls(&self, n: Node, out: &mut Vec<String>) {
        if n.kind() == "call_expression"
            && !self.is_member_call(n)
            && let Some(name) = self.call_name(n)
            && is_capitalised(&name)
            && !out.contains(&name)
        {
            out.push(name);
        }
        for c in named_children(n) {
            self.capitalised_calls(c, out);
        }
    }

    fn kt_refs(&mut self, n: Node) {
        match n.kind() {
            "user_type" => {
                for id in named_children(n)
                    .into_iter()
                    .filter(|c| c.kind() == "identifier" || c.kind() == "type_identifier")
                {
                    let name = self.text(id).to_owned();
                    if is_capitalised(&name) {
                        self.push_ref("jvm_type", &name, id);
                    }
                }
            }
            "navigation_expression" => {
                if let Some(key) = self.resource_key(n) {
                    // Only the whole `R.string.x`, not its `R.string` part.
                    if n.parent()
                        .is_none_or(|p| p.kind() != n.kind() || self.resource_key(p).is_none())
                    {
                        self.out
                            .facts
                            .i18n_uses
                            .push(I18nUse { key, line: line(n) });
                    }
                } else {
                    let parts = named_children(n);
                    if let Some(last) = parts.last() {
                        let name = self.text(*last).trim_start_matches('.').to_owned();
                        let is_call = n.parent().is_some_and(|p| p.kind() == "call_expression");
                        if !is_call {
                            self.push_ref("jvm_member", &name, *last);
                        }
                    }
                    if let Some(first) = parts.first()
                        && matches!(first.kind(), "identifier" | "simple_identifier")
                    {
                        let name = self.text(*first).to_owned();
                        if is_capitalised(&name) {
                            self.push_ref("jvm_type", &name, *first);
                        }
                    }
                }
            }
            "call_expression" => self.kt_call(n),
            "callable_reference" => {
                // `::onClick`, `Foo::bar`
                if let Some(last) = named_children(n).into_iter().last() {
                    let name = self.text(last).to_owned();
                    self.push_ref("jvm_call", &name, last);
                }
            }
            "when_expression" => self.kt_when(n),
            "binary_expression" => self.kt_typed_destination(n),
            _ => {}
        }
        for c in named_children(n) {
            self.kt_refs(c);
        }
    }

    fn kt_call(&mut self, n: Node) {
        let Some(name) = self.call_name(n) else {
            return;
        };
        // A trailing lambda wraps the call: count the inner call only.
        if n.named_child(0)
            .is_some_and(|c| c.kind() == "call_expression")
        {
            return;
        }
        let member = self.is_member_call(n);
        let callee_node = n.named_child(0).unwrap_or(n);
        if member {
            self.push_ref("jvm_member", &name, callee_node);
        } else if is_capitalised(&name) {
            self.push_ref("jvm_type", &name, callee_node);
        } else {
            self.push_ref("jvm_call", &name, callee_node);
        }
        // The outer node holds the trailing lambda, when there is one.
        let outer = {
            let mut o = n;
            while let Some(p) = o.parent() {
                if p.kind() == "call_expression" && p.named_child(0) == Some(o) {
                    o = p;
                } else {
                    break;
                }
            }
            o
        };
        if !member && is_capitalised(&name) {
            let l = line(n);
            if let Some(c) = self
                .out
                .facts
                .composables
                .iter_mut()
                .filter(|c| c.line <= l && l <= c.end_line)
                .max_by_key(|c| c.line)
                && !c.calls.iter().any(|x| x.name == name)
            {
                c.calls.push(NamedLine {
                    name: name.clone(),
                    line: l,
                });
            }
        }
        let lambdas = lambdas_of(outer);
        let first_string = || {
            descendant_kind(n, "value_arguments")
                .and_then(|a| named_children(a).into_iter().next())
                .and_then(|a| named_children(a).into_iter().last())
                .filter(|v| v.kind() == "string_literal")
                .map(|v| self.text(v).trim_matches('"').to_owned())
        };
        let screen = match name.as_str() {
            "composable" | "dialog" if !member || name == "composable" => Some((
                if name == "dialog" {
                    "dialog"
                } else {
                    "navigation"
                },
                first_string(),
            )),
            "setContent" => Some(("content", None)),
            _ => overlay_kind(&name).filter(|_| !member).map(|k| (k, None)),
        };
        if let Some((kind, title)) = screen {
            let mut candidates = Vec::new();
            for l in &lambdas {
                self.capitalised_calls(*l, &mut candidates);
            }
            // `AlertDialog(text = { Text(…) })`: content in named arguments.
            if (kind == "dialog" || kind == "sheet")
                && let Some(args) = descendant_kind(n, "value_arguments")
            {
                self.capitalised_calls(args, &mut candidates);
            }
            if candidates.is_empty() {
                return;
            }
            let owner = self.owner_at(n);
            self.out.facts.screens.push(Screen {
                kind: kind.to_owned(),
                via: name,
                candidates,
                title,
                owner,
                group: None,
                line: line(n),
            });
        }
    }

    /// `composable<Detail> { DetailScreen() }` reads as `composable < Detail > {…}`.
    fn kt_typed_destination(&mut self, n: Node) {
        let (Some(left), Some(right)) = (
            n.child_by_field_name("left"),
            n.child_by_field_name("right"),
        ) else {
            return;
        };
        if left.kind() != "binary_expression" || right.kind() != "lambda_literal" {
            return;
        }
        let (Some(ll), Some(lr)) = (
            left.child_by_field_name("left"),
            left.child_by_field_name("right"),
        ) else {
            return;
        };
        let callee = self.text(ll).to_owned();
        if !matches!(callee.as_str(), "composable" | "dialog") {
            return;
        }
        let mut candidates = Vec::new();
        self.capitalised_calls(right, &mut candidates);
        if candidates.is_empty() {
            return;
        }
        let route = self.text(lr).to_owned();
        self.push_ref("jvm_type", &route, lr);
        let owner = self.owner_at(n);
        self.out.facts.screens.push(Screen {
            kind: if callee == "dialog" {
                "dialog"
            } else {
                "navigation"
            }
            .to_owned(),
            via: callee,
            candidates,
            title: Some(route),
            owner,
            group: None,
            line: line(n),
        });
    }

    /// `when (tab) { 0 -> TimesScreen(); 1 -> RakaatScreen() }`: each branch
    /// that shows a composable is a possible screen.
    fn kt_when(&mut self, n: Node) {
        let group = line(n);
        let owner = self.owner_at(n);
        for entry in named_children(n)
            .into_iter()
            .filter(|c| c.kind() == "when_entry")
        {
            let cond = entry.child_by_field_name("condition");
            let body = named_children(entry)
                .into_iter()
                .rfind(|c| Some(*c) != cond && c.kind() != "when_condition");
            let Some(body) = body else { continue };
            let body = if body.kind() == "control_structure_body" {
                named_children(body).into_iter().next().unwrap_or(body)
            } else {
                body
            };
            let body = if body.kind() == "block" || body.kind() == "statements" {
                match named_children(body).into_iter().next() {
                    Some(b) => b,
                    None => continue,
                }
            } else {
                body
            };
            if body.kind() != "call_expression" || self.is_member_call(body) {
                continue;
            }
            let Some(name) = self.call_name(body) else {
                continue;
            };
            if !is_capitalised(&name) {
                continue;
            }
            let title = match cond {
                Some(c) => squash(self.text(c)),
                None => {
                    let t = self.text(entry);
                    squash(t.split("->").next().unwrap_or("else"))
                }
            };
            self.out.facts.screens.push(Screen {
                kind: "switch".into(),
                via: "when".into(),
                candidates: vec![name],
                title: Some(title),
                owner: owner.clone(),
                group: Some(group),
                line: line(entry),
            });
        }
    }

    // ------------------------------------------------------------ Java

    fn java_mods(&self, n: Node) -> Mods {
        let mut m = Mods::default();
        if let Some(mods) = named_children(n)
            .into_iter()
            .find(|c| c.kind() == "modifiers")
        {
            for k in children(mods) {
                let t = self.text(k).trim();
                if k.kind().contains("annotation") {
                    let name = t.trim_start_matches('@');
                    m.annotations
                        .push(name.split('(').next().unwrap_or(name).to_owned());
                } else if matches!(t, "public" | "private" | "protected") {
                    m.visibility = Some(t.to_owned());
                } else if !t.is_empty() {
                    m.words.push(t.to_owned());
                }
            }
        }
        m
    }

    fn java_decls(&mut self, n: Node, parent: Option<&str>) {
        for c in named_children(n) {
            match c.kind() {
                "package_declaration" => {
                    if let Some(q) = named_children(c)
                        .into_iter()
                        .find(|x| x.kind().contains("identifier"))
                    {
                        self.package = Some(self.text(q).to_owned());
                        self.out.facts.package = self.package.clone();
                    }
                }
                "import_declaration" => {
                    let t = self
                        .text(c)
                        .trim_start_matches("import")
                        .trim()
                        .trim_end_matches(';')
                        .trim_start_matches("static ")
                        .to_owned();
                    self.out.facts.imports.push(t);
                }
                "class_declaration"
                | "interface_declaration"
                | "enum_declaration"
                | "record_declaration"
                | "annotation_type_declaration" => self.java_type(c, parent),
                "method_declaration" | "constructor_declaration" => self.java_method(c, parent),
                "field_declaration" | "constant_declaration" => self.java_field(c, parent),
                "enum_body_declarations" | "class_body" | "interface_body" | "enum_body" => {
                    self.java_decls(c, parent)
                }
                "enum_constant" => {
                    if let (Some(p), Some(id)) = (parent, c.child_by_field_name("name")) {
                        let name = self.text(id).to_owned();
                        self.out.symbols.push(NewSymbol {
                            kind: SymbolKind::EnumCase,
                            name: name.clone(),
                            fqn: format!("{p}.{name}"),
                            parent_fqn: Some(p.to_owned()),
                            signature: name,
                            visibility: None,
                            is_static: true,
                            is_abstract: false,
                            is_final: true,
                            start_line: line(c),
                            end_line: end_line(c),
                            doc_summary: self.doc(c),
                            meta: SymbolMeta::default(),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    fn java_type(&mut self, n: Node, parent: Option<&str>) {
        let Some(name_node) = n.child_by_field_name("name") else {
            return;
        };
        let name = self.text(name_node).to_owned();
        let m = self.java_mods(n);
        let kind = match n.kind() {
            "interface_declaration" | "annotation_type_declaration" => "interface",
            "enum_declaration" => "enum",
            _ => "class",
        };
        let fqn = self.fqn(parent, &name);
        let extends: Vec<String> = n
            .child_by_field_name("superclass")
            .map(|s| base_type(self.text(s).trim_start_matches("extends").trim()))
            .into_iter()
            .collect();
        let mut implements: Vec<String> = Vec::new();
        for field in ["interfaces", "extends_interfaces"] {
            if let Some(i) = n.child_by_field_name(field) {
                for t in self
                    .text(i)
                    .trim_start_matches("implements")
                    .trim_start_matches("extends")
                    .split(',')
                {
                    let t = base_type(t.trim());
                    if !t.is_empty() {
                        implements.push(t);
                    }
                }
            }
        }
        let body = n.child_by_field_name("body");
        self.out.symbols.push(NewSymbol {
            kind: match kind {
                "interface" => SymbolKind::Interface,
                "enum" => SymbolKind::Enum,
                _ => SymbolKind::Class,
            },
            name: name.clone(),
            fqn: fqn.clone(),
            parent_fqn: parent.map(str::to_owned),
            signature: self.header(n, body),
            visibility: m.visibility.clone().or(Some("package".into())),
            is_static: m.has("static"),
            is_abstract: m.has("abstract"),
            is_final: m.has("final"),
            start_line: line(n),
            end_line: end_line(n),
            doc_summary: self.doc(n),
            meta: SymbolMeta {
                extends: extends.clone(),
                implements: implements.clone(),
                attributes: m.annotations,
                ..SymbolMeta::default()
            },
        });
        let mut supertypes = extends;
        supertypes.extend(implements);
        self.out.facts.types.push(TypeFacts {
            name,
            fqn: fqn.clone(),
            kind: kind.to_owned(),
            line: line(n),
            end_line: end_line(n),
            supertypes,
        });
        if let Some(b) = body {
            self.java_decls(b, Some(&fqn));
        }
    }

    fn java_method(&mut self, n: Node, parent: Option<&str>) {
        let Some(name_node) = n.child_by_field_name("name") else {
            return;
        };
        let name = self.text(name_node).to_owned();
        let m = self.java_mods(n);
        let body = n.child_by_field_name("body");
        let params: Vec<Param> = n
            .child_by_field_name("parameters")
            .map(|p| {
                named_children(p)
                    .into_iter()
                    .filter(|c| c.kind().ends_with("parameter"))
                    .map(|c| Param {
                        name: c
                            .child_by_field_name("name")
                            .map(|x| self.text(x).to_owned())
                            .unwrap_or_default(),
                        r#type: c.child_by_field_name("type").map(|t| squash(self.text(t))),
                        ..Param::default()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let return_type = n.child_by_field_name("type").map(|t| squash(self.text(t)));
        self.out.symbols.push(NewSymbol {
            kind: SymbolKind::Method,
            name: name.clone(),
            fqn: self.fqn(parent, &name),
            parent_fqn: parent.map(str::to_owned),
            signature: self.header(n, body),
            visibility: m.visibility.clone().or(Some("package".into())),
            is_static: m.has("static"),
            is_abstract: m.has("abstract") || body.is_none(),
            is_final: m.has("final"),
            start_line: line(n),
            end_line: end_line(n),
            doc_summary: self.doc(n),
            meta: SymbolMeta {
                attributes: m.annotations,
                params,
                return_type,
                ..SymbolMeta::default()
            },
        });
    }

    fn java_field(&mut self, n: Node, parent: Option<&str>) {
        let m = self.java_mods(n);
        let ty = n.child_by_field_name("type").map(|t| squash(self.text(t)));
        let mut cur = n.walk();
        let declarators: Vec<Node> = n.children_by_field_name("declarator", &mut cur).collect();
        for d in declarators {
            let Some(id) = d.child_by_field_name("name") else {
                continue;
            };
            let name = self.text(id).to_owned();
            let constant = m.has("static") && m.has("final");
            self.out.symbols.push(NewSymbol {
                kind: if constant {
                    SymbolKind::Const
                } else {
                    SymbolKind::Property
                },
                name: name.clone(),
                fqn: self.fqn(parent, &name),
                parent_fqn: parent.map(str::to_owned),
                signature: self.header(n, None).chars().take(160).collect(),
                visibility: m.visibility.clone().or(Some("package".into())),
                is_static: m.has("static"),
                is_abstract: false,
                is_final: m.has("final"),
                start_line: line(n),
                end_line: end_line(n),
                doc_summary: self.doc(n),
                meta: SymbolMeta {
                    r#type: ty.clone(),
                    value: d
                        .child_by_field_name("value")
                        .map(|v| squash(self.text(v)).chars().take(120).collect()),
                    readonly: m.has("final"),
                    ..SymbolMeta::default()
                },
            });
        }
    }

    fn java_refs(&mut self, n: Node) {
        match n.kind() {
            "type_identifier" => {
                let name = self.text(n).to_owned();
                // A declaration's own name is an `identifier`, so this is a use.
                if is_capitalised(&name) {
                    self.push_ref("jvm_type", &name, n);
                }
            }
            "method_invocation" => {
                if let Some(name) = n.child_by_field_name("name") {
                    let t = self.text(name).to_owned();
                    let kind = if n.child_by_field_name("object").is_some() {
                        "jvm_member"
                    } else {
                        "jvm_call"
                    };
                    self.push_ref(kind, &t, name);
                }
                if let Some(o) = n.child_by_field_name("object")
                    && o.kind() == "identifier"
                {
                    let t = self.text(o).to_owned();
                    if is_capitalised(&t) {
                        self.push_ref("jvm_type", &t, o);
                    }
                }
            }
            "field_access" => {
                if let Some(key) = self.resource_key(n) {
                    if n.parent()
                        .is_none_or(|p| p.kind() != n.kind() || self.resource_key(p).is_none())
                    {
                        self.out
                            .facts
                            .i18n_uses
                            .push(I18nUse { key, line: line(n) });
                    }
                } else if let Some(f) = n.child_by_field_name("field") {
                    let t = self.text(f).to_owned();
                    self.push_ref("jvm_member", &t, f);
                    if let Some(o) = n.child_by_field_name("object")
                        && o.kind() == "identifier"
                    {
                        let t = self.text(o).to_owned();
                        if is_capitalised(&t) {
                            self.push_ref("jvm_type", &t, o);
                        }
                    }
                }
            }
            "method_reference" => {
                if let Some(last) = named_children(n).into_iter().last() {
                    let t = self.text(last).to_owned();
                    self.push_ref("jvm_call", &t, last);
                }
            }
            _ => {}
        }
        for c in named_children(n) {
            self.java_refs(c);
        }
    }
}

/// Lambdas passed to a call: trailing (`Foo { … }`) and as arguments.
fn lambdas_of(call: Node) -> Vec<Node> {
    let mut out = Vec::new();
    for c in named_children(call) {
        match c.kind() {
            "annotated_lambda" => {
                out.extend(
                    named_children(c)
                        .into_iter()
                        .filter(|x| x.kind() == "lambda_literal"),
                );
            }
            "lambda_literal" => out.push(c),
            "call_suffix" => out.extend(lambdas_of(c)),
            _ => {}
        }
    }
    out
}

fn descendant_kind<'t>(n: Node<'t>, kind: &str) -> Option<Node<'t>> {
    for c in named_children(n) {
        if c.kind() == kind {
            return Some(c);
        }
        if c.kind() == "lambda_literal" || c.kind() == "annotated_lambda" {
            continue;
        }
        if let Some(x) = descendant_kind(c, kind) {
            return Some(x);
        }
    }
    None
}

// ---------------------------------------------------------------- XML

/// The language of a `res/values-xx/` folder: `values` → `default`,
/// `values-ms` → `ms`, `values-pt-rBR` → `pt-BR`, `values-b+sr+Latn` →
/// `sr-Latn`; `None` for folders that are not about language (`values-night`).
pub fn values_locale(path: &str) -> Option<String> {
    let dir = path.rsplit('/').nth(1)?;
    if dir == "values" {
        return Some("default".into());
    }
    let quals = dir.strip_prefix("values-")?;
    let first = quals.split('-').next()?;
    if let Some(bcp) = first.strip_prefix("b+") {
        return Some(bcp.replace('+', "-"));
    }
    let is_lang = (2..=3).contains(&first.len()) && first.chars().all(|c| c.is_ascii_lowercase());
    if !is_lang {
        return None;
    }
    let region = quals
        .split('-')
        .nth(1)
        .and_then(|r| r.strip_prefix('r'))
        .filter(|r| r.len() == 2 && r.chars().all(|c| c.is_ascii_uppercase()));
    Some(match region {
        Some(r) => format!("{first}-{r}"),
        None => first.to_owned(),
    })
}

fn line_at(src: &str, byte: usize) -> u32 {
    src[..byte.min(src.len())].matches('\n').count() as u32 + 1
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let at = tag.find(&format!("{name}=\""))? + name.len() + 2;
    let end = tag[at..].find('"')?;
    Some(tag[at..at + end].to_owned())
}

fn xml_file(path: &str, src: &str, facts: &mut AndroidFacts) {
    // Comments must not count.
    let clean = strip_xml_comments(src);
    let src = clean.as_str();
    if path.contains("/values")
        && path
            .rsplit('/')
            .nth(1)
            .is_some_and(|d| d.starts_with("values"))
        && let Some(locale) = values_locale(path)
    {
        {
            let mut keys = Vec::new();
            for tag in ["<string ", "<plurals ", "<string-array "] {
                for (at, _) in src.match_indices(tag) {
                    let end = src[at..].find('>').map(|e| at + e).unwrap_or(src.len());
                    let t = &src[at..end];
                    if let Some(name) = attr(t, "name") {
                        if attr(t, "translatable").as_deref() == Some("false") {
                            facts.untranslatable.push(name.clone());
                        }
                        keys.push(name);
                    }
                }
            }
            if !keys.is_empty() {
                facts.locales.push(LocaleKeys { locale, keys });
            }
        }
    }
    for prefix in ["@string/", "@plurals/", "@array/"] {
        for (at, _) in src.match_indices(prefix) {
            let rest = &src[at + prefix.len()..];
            let key: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !key.is_empty() {
                facts.i18n_uses.push(I18nUse {
                    key,
                    line: line_at(src, at),
                });
            }
        }
    }
    if path.ends_with("AndroidManifest.xml") {
        for kind in [
            "activity-alias",
            "activity",
            "service",
            "receiver",
            "provider",
        ] {
            let open = format!("<{kind}");
            for (at, _) in src.match_indices(&open) {
                // `<activity` also matches `<activity-alias`.
                let next = src[at + open.len()..].chars().next();
                if !matches!(next, Some(c) if c.is_whitespace() || c == '>' || c == '/') {
                    continue;
                }
                let tag_end = src[at..].find('>').map(|e| at + e).unwrap_or(src.len());
                let tag = &src[at..tag_end];
                let Some(name) = attr(tag, "android:name") else {
                    continue;
                };
                let self_closing = tag.ends_with('/');
                let body = if self_closing {
                    ""
                } else {
                    let close = format!("</{kind}>");
                    let end = src[tag_end..]
                        .find(&close)
                        .map(|e| tag_end + e)
                        .unwrap_or(tag_end);
                    &src[tag_end..end]
                };
                facts.manifest.push(ManifestComponent {
                    kind: kind.to_owned(),
                    name,
                    launcher: body.contains("android.intent.category.LAUNCHER"),
                    exported: attr(tag, "android:exported").map(|v| v == "true"),
                    line: line_at(src, at),
                });
            }
        }
        facts.manifest.sort_by_key(|c| c.line);
    }
}

/// Comments blanked out, keeping line breaks so lines stay right.
fn strip_xml_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        let end = after.find("-->").map(|e| e + 3).unwrap_or(after.len());
        out.extend(after[..end].chars().filter(|c| *c == '\n'));
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const KT: &str = r#"package com.example.app

import androidx.compose.runtime.Composable

/** Screens pushed on top of the tabs. */
sealed interface Route {
    data class Detail(val id: String) : Route
    data object Home : Route
}

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent { AppTheme { Root(state) } }
    }
}

@Composable
fun Root(state: AppState, onBack: () -> Unit = {}) {
    var tab by rememberSaveable { mutableIntStateOf(0) }
    when (val top = stack.lastOrNull()) {
        is Route.Detail -> DetailScreen(state, top.id)
        Route.Home -> HomeScreen(state)
        else -> when (tab) {
            0 -> TimesScreen(state)
            else -> SettingsScreen(state)
        }
    }
    Text(stringResource(R.string.title))
    NavHost(nav, startDestination = "home") {
        composable("home") { HomeScreen(state) }
        composable<Detail> { DetailScreen(state, it.id) }
    }
    if (show) ModalBottomSheet(onDismissRequest = {}) { AdjustSheet() }
    Store.load()
}

object Store {
    const val KEY = "k"
    fun load(): Int = 1
}

enum class Lang(val code: String) { EN("en"), MS("ms") }
"#;

    #[test]
    fn kotlin_compose_file() {
        let x = extract_file("app/src/main/java/com/example/app/Root.kt", KT);
        let syms: Vec<String> = x
            .symbols
            .iter()
            .map(|s| format!("{} {}", s.kind.as_str(), s.fqn))
            .collect();
        for s in &syms {
            println!("{s}");
        }
        println!("{}", serde_json::to_string_pretty(&x.facts).unwrap());
        for want in [
            "interface com.example.app.Route",
            "class com.example.app.Route.Detail",
            "property com.example.app.Route.Detail.id",
            "class com.example.app.MainActivity",
            "method com.example.app.MainActivity.onCreate",
            "function com.example.app.Root",
            "class com.example.app.Store",
            "const com.example.app.Store.KEY",
            "method com.example.app.Store.load",
            "enum com.example.app.Lang",
            "enum_case com.example.app.Lang.MS",
        ] {
            assert!(syms.iter().any(|s| s == want), "missing {want}");
        }
        let root = &x.facts.composables[0];
        assert_eq!(root.name, "Root");
        assert_eq!(root.params.len(), 2);
        assert_eq!(root.params[1].default.as_deref(), Some("{}"));
        assert_eq!(root.state[0].name, "tab");
        assert_eq!(root.state[0].via, "rememberSaveable");
        assert_eq!(root.state[0].initial.as_deref(), Some("0"));
        let screens: Vec<(String, String, Option<String>)> = x
            .facts
            .screens
            .iter()
            .map(|s| (s.kind.clone(), s.candidates.join(","), s.title.clone()))
            .collect();
        for want in [
            ("content".to_owned(), "AppTheme,Root".to_owned(), None),
            (
                "switch".into(),
                "DetailScreen".into(),
                Some("is Route.Detail".into()),
            ),
            (
                "switch".into(),
                "HomeScreen".into(),
                Some("Route.Home".into()),
            ),
            ("switch".into(), "TimesScreen".into(), Some("0".into())),
            (
                "switch".into(),
                "SettingsScreen".into(),
                Some("else".into()),
            ),
            (
                "navigation".into(),
                "HomeScreen".into(),
                Some("home".into()),
            ),
            (
                "navigation".into(),
                "DetailScreen".into(),
                Some("Detail".into()),
            ),
            ("sheet".into(), "AdjustSheet".into(), None),
        ] {
            assert!(screens.contains(&want), "missing {want:?} in {screens:?}");
        }
        assert_eq!(x.facts.i18n_uses[0].key, "title");
        let refs: Vec<String> = x
            .refs
            .iter()
            .map(|r| format!("{}:{}", r.kind, r.target))
            .collect();
        assert!(refs.contains(&"jvm_type:Store".to_owned()));
        assert!(refs.contains(&"jvm_member:load".to_owned()));
        assert!(refs.contains(&"jvm_type:DetailScreen".to_owned()));
    }

    #[test]
    fn java_file() {
        let src = "package a.b;\n/** Main. */\npublic class MainActivity extends AppCompatActivity implements Foo {\n  public static final String KEY = \"k\";\n  private int x = 1;\n  @Override protected void onCreate(Bundle b) { super.onCreate(b); setContentView(R.layout.main); getString(R.string.hi); new Helper().go(); }\n}\n";
        let x = extract_file("app/src/main/java/a/b/MainActivity.java", src);
        let syms: Vec<String> = x
            .symbols
            .iter()
            .map(|s| format!("{} {}", s.kind.as_str(), s.fqn))
            .collect();
        assert_eq!(
            syms,
            [
                "class a.b.MainActivity",
                "const a.b.MainActivity.KEY",
                "property a.b.MainActivity.x",
                "method a.b.MainActivity.onCreate"
            ]
        );
        assert_eq!(x.symbols[0].meta.extends, ["AppCompatActivity"]);
        assert_eq!(x.symbols[0].doc_summary.as_deref(), Some("Main"));
        assert_eq!(x.facts.i18n_uses[0].key, "hi");
        let refs: Vec<String> = x
            .refs
            .iter()
            .map(|r| format!("{}:{}", r.kind, r.target))
            .collect();
        assert!(refs.contains(&"jvm_type:Helper".to_owned()));
        assert!(refs.contains(&"jvm_member:go".to_owned()));
    }

    #[test]
    fn resources_and_manifest() {
        assert_eq!(
            values_locale("app/src/main/res/values/strings.xml").as_deref(),
            Some("default")
        );
        assert_eq!(
            values_locale("res/values-ms/strings.xml").as_deref(),
            Some("ms")
        );
        assert_eq!(
            values_locale("res/values-pt-rBR/strings.xml").as_deref(),
            Some("pt-BR")
        );
        assert_eq!(
            values_locale("res/values-b+sr+Latn/strings.xml").as_deref(),
            Some("sr-Latn")
        );
        assert_eq!(values_locale("res/values-night/themes.xml"), None);
        let x = extract_file(
            "app/src/main/res/values/strings.xml",
            "<resources>\n<!-- <string name=\"old\">x</string> -->\n<string name=\"hi\">Hi</string>\n<string name=\"app\" translatable=\"false\">A</string>\n<plurals name=\"n\"><item quantity=\"one\">1</item></plurals>\n</resources>\n",
        );
        assert_eq!(x.facts.locales[0].keys, ["hi", "app", "n"]);
        assert_eq!(x.facts.untranslatable, ["app"]);
        let m = extract_file(
            "app/src/main/AndroidManifest.xml",
            "<manifest>\n<application android:label=\"@string/app\">\n<activity android:name=\".MainActivity\" android:exported=\"true\">\n<intent-filter><category android:name=\"android.intent.category.LAUNCHER\" /></intent-filter>\n</activity>\n<activity-alias android:name=\".Alias\" />\n<receiver android:name=\".R\" android:exported=\"false\"/>\n</application></manifest>\n",
        );
        let c: Vec<(String, String, bool, u32)> = m
            .facts
            .manifest
            .iter()
            .map(|c| (c.kind.clone(), c.name.clone(), c.launcher, c.line))
            .collect();
        assert_eq!(
            c,
            [
                ("activity".into(), ".MainActivity".into(), true, 3),
                ("activity-alias".into(), ".Alias".into(), false, 6),
                ("receiver".into(), ".R".into(), false, 7),
            ]
        );
        assert_eq!(m.facts.i18n_uses[0].key, "app");
    }
}
