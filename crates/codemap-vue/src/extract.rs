//! Per-file extraction for `.vue`, `.js` and `.ts` files: facts are stored as
//! JSON in `js_modules`, and exported functions / composables / Pinia stores
//! become symbols. Files are re-parsed only when new, changed, or extracted by
//! an older extractor version (same mechanism as PHP).

use std::path::Path;
use std::time::Instant;

use codemap_core::symbols::{self, NewSymbol, SymbolKind, SymbolMeta};
use codemap_core::{Result, Store};
use codemap_js::template::{self, is_native_or_builtin};
use codemap_js::{DeclKind, NamedLine, ScriptFacts, ScriptKind, analyze, sfc};
use rayon::prelude::*;
use rusqlite::{TransactionBehavior, params};
use serde::{Deserialize, Serialize};

/// Extractor name in `extractor_state` and `warnings.adapter`.
pub const EXTRACTOR: &str = "js";

/// Bump when per-file output changes; every JS/TS/Vue file is then re-parsed.
pub const JS_EXTRACTOR_VERSION: i64 = 27;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TagUse {
    pub tag: String,
    /// First occurrence.
    pub line: u32,
    pub count: u32,
}

/// A template value passed to a child component's prop.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PropPass {
    pub tag: String,
    /// camelCase prop name; empty for `v-bind="obj"` (all of obj's keys).
    pub prop: String,
    /// The value as a path, `v-for` aliases resolved (`malls[]`, `order.items`).
    pub path: String,
}

/// Everything known about one file, stored as `js_modules.facts_json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModuleFacts {
    pub lang: String,
    #[serde(default)]
    pub script: ScriptFacts,
    #[serde(default)]
    pub has_template: bool,
    #[serde(default)]
    pub has_script_setup: bool,
    /// Non-HTML tags in the template, in order of first use.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<TagUse>,
    /// `<slot>` / `<slot name="x">` outlets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slots: Vec<NamedLine>,
    /// `$emit('x')` in template handlers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub template_emits: Vec<NamedLine>,
    /// Property paths read in the template, with `v-for` aliases resolved:
    /// `malls[].name`, `order.total`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub template_paths: Vec<String>,
    /// Values handed to child components: `<MallCard v-for="m in malls" :mall="m">`
    /// → `MallCard`, `mall`, `malls[]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prop_passes: Vec<PropPass>,
    /// Translation keys used in the template.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub template_i18n: Vec<codemap_js::I18nUse>,
    /// Messages, when this file is a locale file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<crate::i18n::LocaleFacts>,
    /// Messages of every language, when the file keeps them per text
    /// (`checkIn: { en: "Check-in", ms: "Daftar masuk" }`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inverted_locales: Vec<crate::i18n::LocaleFacts>,
    /// Custom SFC blocks (`i18n`, `docs`, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom_blocks: Vec<String>,
    /// Functions called by bare name in the template (`{{ formatMyr(x) }}`),
    /// often Nuxt auto-imports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub template_calls: Vec<NamedLine>,
    pub lines: u32,
}

/// Names called bare in a template expression: `formatMyr(x)` but not
/// `item.format(x)`, `$t('a')` or `String(x)`.
pub fn bare_calls(expr: &str) -> Vec<String> {
    const SKIP: &[&str] = &[
        "if",
        "for",
        "while",
        "switch",
        "return",
        "typeof",
        "new",
        "String",
        "Number",
        "Boolean",
        "Array",
        "Object",
        "Date",
        "Math",
        "JSON",
        "parseInt",
        "parseFloat",
        "isNaN",
        "encodeURIComponent",
        "decodeURIComponent",
        "t",
        "tc",
        "te",
        "tm",
        "d",
        "n",
        "emit",
        "useRoute",
        "useRouter",
        "navigateTo",
        "alert",
        "confirm",
        "console",
    ];
    let b = expr.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < b.len() {
        let c = b[i];
        if let Some(q) = quote {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if matches!(c, b'\'' | b'"' | b'`') {
            quote = Some(c);
            i += 1;
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$') {
                i += 1;
            }
            let name = &expr[start..i];
            let before = start.checked_sub(1).map(|j| b[j]);
            let member = matches!(before, Some(b'.') | Some(b'$'))
                || (start >= 2 && &b[start - 2..start] == b"?.");
            let mut k = i;
            while k < b.len() && b[k] == b' ' {
                k += 1;
            }
            if !member
                && k < b.len()
                && b[k] == b'('
                && !SKIP.contains(&name)
                && !out.iter().any(|n| n == name)
            {
                out.push(name.to_owned());
            }
            continue;
        }
        i += 1;
    }
    out
}

impl ModuleFacts {
    /// Every event the component emits, declared or called.
    pub fn emits(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let declared = self
            .script
            .options
            .iter()
            .flat_map(|o| o.emits.iter())
            .chain(&self.script.setup.emits);
        for n in declared
            .chain(&self.script.emitted)
            .chain(&self.template_emits)
        {
            if !out.contains(&n.name) {
                out.push(n.name.clone());
            }
        }
        out
    }
}

pub fn extract_file(path: &str, src: &str) -> ModuleFacts {
    let ext = path.rsplit('.').next().unwrap_or("js");
    let lines = src.lines().count() as u32;
    if let Some(locale) = crate::i18n::locale_facts(path, src) {
        return ModuleFacts {
            lang: if ext == "json" {
                "json".into()
            } else {
                "js".into()
            },
            locale: Some(locale),
            lines,
            ..ModuleFacts::default()
        };
    }
    if ext != "vue" {
        let mut facts = ModuleFacts {
            lang: if matches!(ext, "ts" | "tsx" | "mts" | "cts") {
                "ts".into()
            } else {
                "js".into()
            },
            script: analyze(src, ext, 1, ScriptKind::Module),
            lines,
            ..ModuleFacts::default()
        };
        // React: JSX tags are the component's children, the main component's
        // props are the file's props.
        if !facts.script.jsx_tags.is_empty() {
            facts.has_template = true;
            for t in &facts.script.jsx_tags {
                match facts.tags.iter_mut().find(|u| u.tag == t.name) {
                    Some(u) => u.count += 1,
                    None => facts.tags.push(TagUse {
                        tag: t.name.clone(),
                        line: t.line,
                        count: 1,
                    }),
                }
            }
        }
        let stem = path
            .rsplit('/')
            .next()
            .and_then(|n| n.split('.').next())
            .unwrap_or_default()
            .to_owned();
        let main = facts
            .script
            .react_components
            .iter()
            .find(|c| c.default_export)
            .or_else(|| {
                facts
                    .script
                    .react_components
                    .iter()
                    .find(|c| c.name == stem)
            })
            .or_else(|| facts.script.react_components.iter().find(|c| c.exported))
            .cloned();
        if let Some(c) = main {
            facts.script.setup.props = c.props;
        }
        facts.inverted_locales = crate::i18n::inverted_locales(path, src);
        return facts;
    }
    let parts = sfc::split(src);
    let mut facts = ModuleFacts {
        lang: "vue".into(),
        lines,
        ..ModuleFacts::default()
    };
    let classic = parts
        .script()
        .map(|b| analyze(b.content, b.lang(), b.line, ScriptKind::VueScript));
    let setup = parts.script_setup().map(|b| {
        facts.has_script_setup = true;
        analyze(b.content, b.lang(), b.line, ScriptKind::VueSetup)
    });
    facts.script = merge(classic.unwrap_or_default(), setup.unwrap_or_default());
    if let Some(t) = parts.template() {
        facts.has_template = true;
        if t.attr("lang").is_some_and(|l| l != "html") {
            // Pug and other preprocessors: tags can't be read.
            facts
                .custom_blocks
                .push(format!("template lang={}", t.lang()));
        } else {
            template_facts(&template::scan(t.content, t.line), &mut facts);
        }
    }
    for b in &parts.blocks {
        if !matches!(b.tag.as_str(), "template" | "script" | "style")
            && !facts.custom_blocks.contains(&b.tag)
        {
            facts.custom_blocks.push(b.tag.clone());
        }
    }
    facts
}

/// `(item, index) in orders` / `item of data.items` → (`item`, `orders`).
fn v_for(value: &str) -> Option<(String, String)> {
    let (lhs, rhs) = value
        .split_once(" in ")
        .or_else(|| value.split_once(" of "))?;
    let alias = lhs
        .trim()
        .trim_start_matches('(')
        .split(',')
        .next()?
        .trim()
        .to_owned();
    let alias = alias.trim_start_matches('{').trim().to_owned();
    // `wallet.pending_reloads ?? []`, `(items || [])`: the list is the left side.
    let mut source = rhs.trim();
    while let Some(inner) = source.strip_prefix('(').and_then(|x| x.strip_suffix(')')) {
        source = inner.trim();
    }
    let left = source
        .split("??")
        .next()
        .and_then(|x| x.split("||").next())
        .unwrap_or(source)
        .trim()
        .trim_start_matches('(')
        .trim();
    let source = if is_simple_path(left) { left } else { source }.to_owned();
    (!alias.is_empty()
        && alias
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$'))
    .then_some((alias, source))
}

/// Member chains in an expression: `a.b.c`, `a?.b`, `a[0].b` (strings skipped).
pub fn chains(expr: &str) -> Vec<String> {
    let b = expr.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'$';
    while i < b.len() {
        let c = b[i];
        if matches!(c, b'\'' | b'"' | b'`') {
            let q = c;
            i += 1;
            while i < b.len() && b[i] != q {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            i += 1;
            continue;
        }
        let prev_dot = i > 0 && (b[i - 1] == b'.' || ident(b[i - 1]));
        if (c.is_ascii_alphabetic() || c == b'_' || c == b'$') && !prev_dot {
            let start = i;
            while i < b.len() {
                if ident(b[i]) || (b[i] == b'.' && i + 1 < b.len() && ident(b[i + 1])) {
                    i += 1;
                } else if b[i] == b'?'
                    && b.get(i + 1) == Some(&b'.')
                    && b.get(i + 2).is_some_and(|x| ident(*x))
                {
                    i += 2;
                } else if b[i] == b'[' && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
                    while i < b.len() && b[i] != b']' {
                        i += 1;
                    }
                    i += 1;
                } else {
                    break;
                }
            }
            let chain = expr[start..i.min(expr.len())].replace("?.", ".");
            let chain: String = chain
                .split('.')
                .map(|seg| {
                    if seg.contains('[') {
                        format!("{}[]", seg.split('[').next().unwrap_or(""))
                    } else {
                        seg.to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join(".");
            if chain.contains('.') {
                out.push(chain);
            }
            continue;
        }
        i += 1;
    }
    out
}

/// `mall`, `m.offers`, `order?.items`: a plain property path.
fn is_simple_path(v: &str) -> bool {
    let v = v.replace("?.", ".");
    !v.is_empty()
        && v.split('.').all(|seg| {
            let mut c = seg.chars();
            c.next()
                .is_some_and(|f| f.is_ascii_alphabetic() || f == '_' || f == '$')
                && c.all(|x| x.is_ascii_alphanumeric() || x == '_' || x == '$')
        })
}

/// `mall-item` → `mallItem`.
fn camel(name: &str) -> String {
    let mut out = String::new();
    let mut up = false;
    for c in name.chars() {
        if c == '-' {
            up = true;
        } else if up {
            out.extend(c.to_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn template_facts(t: &template::Template, facts: &mut ModuleFacts) {
    // `v-for` aliases visible at each element: alias → source path + `[]`.
    let mut aliases: Vec<Vec<(String, String)>> = Vec::with_capacity(t.elements.len());
    let resolve = |chain: &str, scope: &[(String, String)]| -> String {
        let (root, rest) = chain.split_once('.').unwrap_or((chain, ""));
        for (alias, source) in scope.iter().rev() {
            if alias == root {
                return if rest.is_empty() {
                    format!("{source}[]")
                } else {
                    format!("{source}[].{rest}")
                };
            }
        }
        chain.to_owned()
    };
    for el in &t.elements {
        let mut scope = el.parent.map(|p| aliases[p].clone()).unwrap_or_default();
        if let Some(v) = el
            .attr("v-for")
            .and_then(|a| a.value.as_deref())
            .and_then(v_for)
        {
            let source = resolve(&v.1, &scope);
            scope.push((v.0, source));
        }
        if !is_native_or_builtin(&el.tag) {
            for a in &el.attrs {
                let prop = match a.name.as_str() {
                    "v-bind" => Some(""),
                    n => n.strip_prefix(':').or_else(|| n.strip_prefix("v-bind:")),
                };
                let (Some(prop), Some(v)) = (prop, &a.value) else {
                    continue;
                };
                let prop = prop.split('.').next().unwrap_or(prop);
                let v = v.trim();
                if prop.starts_with('[') || !is_simple_path(v) || facts.prop_passes.len() >= 200 {
                    continue;
                }
                let pass = PropPass {
                    tag: el.tag.clone(),
                    prop: camel(prop),
                    path: resolve(&v.replace("?.", "."), &scope),
                };
                if !facts.prop_passes.contains(&pass) {
                    facts.prop_passes.push(pass);
                }
            }
        }
        for a in &el.attrs {
            let bound = a.name.starts_with(':')
                || a.name.starts_with('@')
                || a.name.starts_with("v-")
                || a.name.starts_with('#');
            if bound
                && a.name != "v-for"
                && let Some(v) = &a.value
            {
                for c in chains(v) {
                    let p = resolve(&c, &scope);
                    if !facts.template_paths.contains(&p) {
                        facts.template_paths.push(p);
                    }
                }
            }
        }
        aliases.push(scope);
    }
    // Bare calls in bound attributes and interpolations.
    let mut calls: Vec<NamedLine> = Vec::new();
    let mut add_calls = |expr: &str, line: u32| {
        for name in bare_calls(expr) {
            if !calls.iter().any(|c| c.name == name) {
                calls.push(NamedLine { name, line });
            }
        }
    };
    for el in &t.elements {
        for a in &el.attrs {
            let bound = a.name.starts_with(':')
                || a.name.starts_with('@')
                || (a.name.starts_with("v-") && a.name != "v-for" && a.name != "v-slot");
            if bound && let Some(v) = &a.value {
                add_calls(v, a.line);
            }
        }
    }
    for it in &t.interpolations {
        add_calls(&it.expr, it.line);
    }
    facts.template_calls = calls;
    for it in &t.interpolations {
        for (key, dynamic) in crate::i18n::template_uses(&it.expr) {
            facts.template_i18n.push(codemap_js::I18nUse {
                guarded: false,
                key,
                dynamic,
                line: it.line,
            });
        }
        let scope = it.parent.map(|p| aliases[p].clone()).unwrap_or_default();
        for c in chains(&it.expr) {
            let p = resolve(&c, &scope);
            if !facts.template_paths.contains(&p) {
                facts.template_paths.push(p);
            }
        }
    }

    for el in &t.elements {
        if el.tag.eq_ignore_ascii_case("slot") {
            let name = match el.attr("name") {
                Some(a) => match &a.value {
                    Some(v) => v.clone(),
                    None => continue,
                },
                None if el
                    .attrs
                    .iter()
                    .any(|a| a.name == ":name" || a.name == "v-bind:name") =>
                {
                    continue;
                }
                None => "default".into(),
            };
            if !facts.slots.iter().any(|s| s.name == name) {
                facts.slots.push(NamedLine {
                    name,
                    line: el.line,
                });
            }
        } else if !is_native_or_builtin(&el.tag) {
            match facts.tags.iter_mut().find(|u| u.tag == el.tag) {
                Some(u) => u.count += 1,
                None => facts.tags.push(TagUse {
                    tag: el.tag.clone(),
                    line: el.line,
                    count: 1,
                }),
            }
        }
        for a in &el.attrs {
            if let Some(v) = &a.value {
                let bound = a.name.starts_with(':')
                    || a.name.starts_with('@')
                    || a.name.starts_with("v-")
                    || a.name.starts_with('#');
                if bound {
                    for (key, dynamic) in crate::i18n::template_uses(v) {
                        facts.template_i18n.push(codemap_js::I18nUse {
                            guarded: false,
                            key,
                            dynamic,
                            line: a.line,
                        });
                    }
                }
                if a.name == "v-t"
                    && let Some(key) = crate::i18n::v_t_key(v)
                {
                    facts.template_i18n.push(codemap_js::I18nUse {
                        guarded: false,
                        key,
                        dynamic: false,
                        line: a.line,
                    });
                }
                if matches!(a.name.as_str(), "keypath" | "path")
                    && matches!(el.tag.as_str(), "i18n-t" | "I18nT" | "i18n" | "i18n-n")
                {
                    facts.template_i18n.push(codemap_js::I18nUse {
                        guarded: false,
                        key: v.clone(),
                        dynamic: false,
                        line: a.line,
                    });
                }
                for name in emitted_in(v) {
                    if !facts.template_emits.iter().any(|e| e.name == name) {
                        facts.template_emits.push(NamedLine { name, line: a.line });
                    }
                }
            }
        }
    }
}

/// Event names in `$emit('name', …)` calls inside an expression.
fn emitted_in(expr: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = expr;
    while let Some(i) = rest.find("$emit(") {
        rest = &rest[i + 6..];
        let r = rest.trim_start();
        if let Some(q) = r.chars().next().filter(|c| matches!(c, '\'' | '"' | '`'))
            && let Some(end) = r[1..].find(q)
        {
            out.push(r[1..1 + end].to_owned());
        }
    }
    out
}

fn merge(mut a: ScriptFacts, b: ScriptFacts) -> ScriptFacts {
    a.imports.extend(b.imports);
    a.decls.extend(b.decls);
    a.reexports.extend(b.reexports);
    if a.options.is_none() {
        a.options = b.options;
    }
    a.setup = b.setup;
    for c in b.composables {
        if !a.composables.iter().any(|x| x.name == c.name) {
            a.composables.push(c);
        }
    }
    a.emitted.extend(b.emitted);
    a.registrations.extend(b.registrations);
    a.registration_globs.extend(b.registration_globs);
    a.pinia_stores.extend(b.pinia_stores);
    a.vuex_uses.extend(b.vuex_uses);
    a.setup_bindings.extend(b.setup_bindings);
    a.unbound_calls.extend(b.unbound_calls);
    a.route_groups.extend(b.route_groups);
    a.api_calls.extend(b.api_calls);
    a.i18n_uses.extend(b.i18n_uses);
    a.fn_calls.extend(b.fn_calls);
    a.db_tables.extend(b.db_tables);
    a.url_templates.extend(b.url_templates);
    for t in b.types {
        if !a.types.iter().any(|x| x.name == t.name) {
            a.types.push(t);
        }
    }
    for p in b.prop_reads {
        if !a.prop_reads.contains(&p) {
            a.prop_reads.push(p);
        }
    }
    a.errors.extend(b.errors);
    a
}

/// Symbols a JS/TS module contributes on its own (components and Vuex
/// modules are added by the registry pass, which knows names and namespaces).
pub fn module_symbols(path: &str, facts: &ModuleFacts) -> Vec<NewSymbol> {
    if facts.lang == "vue" {
        return Vec::new();
    }
    let stem = path.rsplit('/').next().unwrap_or(path);
    let stem = stem.split('.').next().unwrap_or(stem);
    let store_exports: Vec<&str> = facts
        .script
        .pinia_stores
        .iter()
        .filter_map(|s| s.export.as_deref())
        .collect();
    let mut out = Vec::new();
    // Nuxt 2 store files export `state`, `mutations`, …: the module is the symbol.
    let vuex_parts = facts.script.vuex_module.is_some();
    for d in &facts.script.decls {
        if !d.exported && !d.default_export {
            continue;
        }
        if vuex_parts
            && matches!(
                d.name.as_str(),
                "state" | "getters" | "mutations" | "actions" | "modules"
            )
        {
            continue;
        }
        if store_exports.contains(&d.name.as_str()) {
            continue;
        }
        let name = if d.name == "default" {
            stem.to_owned()
        } else {
            d.name.clone()
        };
        let kind = match d.kind {
            DeclKind::Function if codemap_js::script::is_composable_name(&name) => {
                SymbolKind::Composable
            }
            DeclKind::Function => SymbolKind::Function,
            DeclKind::Class => SymbolKind::Class,
            DeclKind::Const | DeclKind::Let => SymbolKind::Const,
            DeclKind::Interface | DeclKind::Type | DeclKind::Enum => continue,
        };
        let signature = if d.name == "default" {
            d.signature.replacen("default", &name, 1)
        } else {
            d.signature.clone()
        };
        out.push(NewSymbol {
            kind,
            name: name.clone(),
            fqn: format!("{path}#{name}"),
            parent_fqn: None,
            signature,
            visibility: None,
            is_static: false,
            is_abstract: false,
            is_final: false,
            start_line: d.line,
            end_line: d.end_line,
            doc_summary: d.doc.clone(),
            meta: SymbolMeta::default(),
        });
    }
    for s in &facts.script.pinia_stores {
        let name = s
            .export
            .clone()
            .unwrap_or_else(|| format!("use{}Store", upper_first(&s.id)));
        let mut parts = Vec::new();
        for (label, list) in [
            ("state", &s.state),
            ("getters", &s.getters),
            ("actions", &s.actions),
        ] {
            if !list.is_empty() {
                parts.push(format!("{label}: {}", list.join(", ")));
            }
        }
        out.push(NewSymbol {
            kind: SymbolKind::Store,
            name: name.clone(),
            fqn: format!("{path}#{name}"),
            parent_fqn: None,
            signature: format!("defineStore('{}') ({} store)", s.id, s.style),
            visibility: None,
            is_static: false,
            is_abstract: false,
            is_final: false,
            start_line: s.line,
            end_line: s.end_line,
            doc_summary: (!parts.is_empty()).then(|| parts.join(" · ")),
            meta: SymbolMeta {
                value: Some(s.id.clone()),
                r#type: Some("pinia".into()),
                ..SymbolMeta::default()
            },
        });
    }
    out
}

fn upper_first(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

/// `(file id, path)`.
type FileRef = (i64, String);

/// Facts and symbols of one parsed file.
type Parsed = (ModuleFacts, Vec<NewSymbol>);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsReport {
    pub parsed: usize,
    pub symbols: usize,
    pub warnings: usize,
    pub failed: usize,
    pub duration_ms: u64,
}

/// Parses pending `.vue` / `.js` / `.ts` files.
pub fn index_pending(store: &mut Store, root: &Path) -> Result<JsReport> {
    let started = Instant::now();
    let tx = store
        .conn_mut()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let pending: Vec<(i64, String)> = {
        let mut stmt = tx.prepare(
            "SELECT f.id, f.path FROM files f
             WHERE f.lang IN ('vue', 'js', 'ts', 'json') AND NOT EXISTS (
                 SELECT 1 FROM extractor_state e
                 WHERE e.file_id = f.id AND e.extractor = ?1 AND e.version = ?2)
             ORDER BY f.path",
        )?;
        let rows = stmt.query_map(params![EXTRACTOR, JS_EXTRACTOR_VERSION], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    // JSON files matter only when they hold translations; mark the rest done.
    let (pending, other_json): (Vec<FileRef>, Vec<FileRef>) = pending
        .into_iter()
        .partition(|(_, p)| !p.ends_with(".json") || crate::i18n::locale_of(p).is_some());
    for (id, _) in &other_json {
        tx.execute(
            "INSERT OR REPLACE INTO extractor_state (file_id, extractor, version) VALUES (?1, ?2, ?3)",
            params![id, EXTRACTOR, JS_EXTRACTOR_VERSION],
        )?;
    }
    let parsed: Vec<(i64, String, std::io::Result<Parsed>)> = pending
        .into_par_iter()
        .map(|(id, path)| {
            let result = std::fs::read(root.join(&path)).map(|bytes| {
                let src = String::from_utf8_lossy(&bytes);
                // A bug in one file's analysis must not stop the whole index.
                let analysed = std::panic::catch_unwind(|| {
                    let facts = extract_file(&path, &src);
                    let syms = module_symbols(&path, &facts);
                    (facts, syms)
                });
                analysed.unwrap_or_else(|_| {
                    let mut facts = ModuleFacts {
                        lang: path.rsplit('.').next().unwrap_or("js").to_owned(),
                        ..ModuleFacts::default()
                    };
                    facts.script.errors.push(NamedLine {
                        name:
                            "codemap could not analyse this file (internal error); please report it"
                                .into(),
                        line: 1,
                    });
                    (facts, Vec::new())
                })
            });
            (id, path, result)
        })
        .collect();
    let total: usize = parsed
        .iter()
        .filter_map(|(_, _, r)| r.as_ref().ok())
        .map(|(_, s)| s.len())
        .sum();
    let bulk = total >= symbols::BULK_FTS_THRESHOLD;
    if bulk {
        symbols::bulk_insert_begin(&tx)?;
    }
    let mut report = JsReport::default();
    {
        let mut insert_module = tx.prepare(
            "INSERT OR REPLACE INTO js_modules (file_id, path, kind, name, facts_json)
             VALUES (?1, ?2, 'module', NULL, ?3)",
        )?;
        for (file_id, path, result) in &parsed {
            let (facts, syms) = match result {
                Ok(r) => r,
                Err(e) => {
                    report.failed += 1;
                    tracing::warn!(path, error = %e, "could not read JS file");
                    continue;
                }
            };
            tx.execute("DELETE FROM symbols WHERE file_id = ?1", [file_id])?;
            tx.execute(
                "DELETE FROM warnings WHERE file_id = ?1 AND adapter = ?2",
                params![file_id, EXTRACTOR],
            )?;
            insert_module.execute(params![file_id, path, serde_json::to_string(facts)?])?;
            for s in syms {
                symbols::insert(&tx, *file_id, s)?;
            }
            for e in &facts.script.errors {
                tx.execute(
                    "INSERT INTO warnings (file_id, path, line, adapter, message)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        file_id,
                        path,
                        e.line,
                        EXTRACTOR,
                        format!("could not parse this code: {}", e.name)
                    ],
                )?;
                report.warnings += 1;
            }
            tx.execute(
                "INSERT OR REPLACE INTO extractor_state (file_id, extractor, version) VALUES (?1, ?2, ?3)",
                params![file_id, EXTRACTOR, JS_EXTRACTOR_VERSION],
            )?;
            report.parsed += 1;
            report.symbols += syms.len();
        }
    }
    if bulk {
        symbols::bulk_insert_end(&tx)?;
    }
    tx.commit()?;
    report.duration_ms = started.elapsed().as_millis() as u64;
    if report.parsed > 0 {
        tracing::info!(
            parsed = report.parsed,
            symbols = report.symbols,
            ms = report.duration_ms,
            "js extraction finished"
        );
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vue_file_facts() {
        let src = "<template>\n  <div>\n    <BaseCard v-for=\"o in orders\" :key=\"o.id\" @click=\"$emit('pick', o)\">\n      <slot name=\"actions\" />\n    </BaseCard>\n    <base-card />\n    <slot />\n    <router-link to=\"/\">x</router-link>\n  </div>\n</template>\n<script setup lang=\"ts\">\nconst props = defineProps<{ orders: Order[] }>()\nconst emit = defineEmits(['close'])\nconst { data } = useFetch('/api/orders')\n</script>\n<i18n>{}</i18n>\n";
        let f = extract_file("app/components/OrderList.vue", src);
        assert!(f.has_template && f.has_script_setup);
        let tags: Vec<(String, u32, u32)> = f
            .tags
            .iter()
            .map(|t| (t.tag.clone(), t.line, t.count))
            .collect();
        assert_eq!(
            tags,
            [("BaseCard".into(), 3, 1), ("base-card".into(), 6, 1)]
        );
        let slots: Vec<&str> = f.slots.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(slots, ["actions", "default"]);
        assert_eq!(f.emits(), ["close", "pick"]);
        assert_eq!(f.script.setup.props[0].name, "orders");
        assert_eq!(f.script.composables[0].name, "useFetch");
        assert_eq!(f.custom_blocks, ["i18n"]);
    }

    #[test]
    fn bare_calls_in_templates() {
        assert_eq!(
            bare_calls(
                "formatMyr(option.price_delta) + item.format(x) + $t('a', { d: formatDistance(p.m) }) + 'notMe(1)' + a?.b(1) + String(x)"
            ),
            ["formatMyr", "formatDistance"]
        );
    }

    #[test]
    fn template_paths_resolve_v_for() {
        let src = "<template>\n  <ul>\n    <li v-for=\"(m, i) in malls\" :key=\"m.id\">\n      {{ m.name }} {{ m?.address.city }} {{ $t('x.y') }}\n      <span v-for=\"o in m.offers\">{{ o.price }}</span>\n    </li>\n  </ul>\n  <p>{{ mall.value.title }} {{ 'a.b' }}</p>\n</template>";
        let f = extract_file("a.vue", src);
        assert_eq!(
            f.template_paths,
            [
                "malls[].id",
                "malls[].name",
                "malls[].address.city",
                "malls[].offers[].price",
                "mall.value.title"
            ]
        );
    }

    #[test]
    fn module_symbols_kinds() {
        let src = "export function useCart() {}\nexport const formatPrice = (n) => n\nexport const API = '/api'\nfunction internal() {}\nexport const useCartStore = defineStore('cart', { actions: { add() {} } })\nexport default function () {}\n";
        let f = extract_file("src/composables/useThing.js", src);
        let syms: Vec<(String, SymbolKind, String)> =
            module_symbols("src/composables/useThing.js", &f)
                .into_iter()
                .map(|s| (s.name, s.kind, s.fqn))
                .collect();
        assert_eq!(
            syms,
            [
                (
                    "useCart".into(),
                    SymbolKind::Composable,
                    "src/composables/useThing.js#useCart".into()
                ),
                (
                    "formatPrice".into(),
                    SymbolKind::Function,
                    "src/composables/useThing.js#formatPrice".into()
                ),
                (
                    "API".into(),
                    SymbolKind::Const,
                    "src/composables/useThing.js#API".into()
                ),
                (
                    "useThing".into(),
                    SymbolKind::Composable,
                    "src/composables/useThing.js#useThing".into()
                ),
                (
                    "useCartStore".into(),
                    SymbolKind::Store,
                    "src/composables/useThing.js#useCartStore".into()
                ),
            ]
        );
    }
}
