//! The project-wide pass: component names (Nuxt auto-imports, global
//! registrations, `require.context` / `import.meta.glob`), module kinds,
//! which component renders which, and Vuex namespaces. It depends on every
//! file, so it is rebuilt as a set whenever a JS/TS/Vue file changes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use codemap_core::symbols::{self, NewSymbol, SymbolKind, SymbolMeta};
use codemap_core::{Result, Store};
use codemap_js::template::pascal;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::detect::{UI_LIBRARIES, VueDetection};
use crate::extract::ModuleFacts;
use crate::naming;
use crate::paths::{Resolved, Resolver};

/// Bump when registry output changes.
pub const REGISTRY_VERSION: i64 = 21;

pub struct Module {
    pub file_id: i64,
    pub path: String,
    pub facts: ModuleFacts,
}

pub fn load_modules(store: &Store) -> Result<Vec<Module>> {
    let mut stmt = store
        .conn()
        .prepare("SELECT file_id, path, facts_json FROM js_modules ORDER BY path")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (file_id, path, json) = row?;
        let facts = serde_json::from_str(&json).unwrap_or_default();
        out.push(Module {
            file_id,
            path,
            facts,
        });
    }
    Ok(out)
}

/// One way a component name becomes available everywhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalName {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// `auto` (Nuxt), `global` (app/Vue.component), `glob` (require.context).
    pub via: String,
    /// `file:line` of the registration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Use {
    pub from_path: String,
    pub tag: String,
    pub line: u32,
    pub to_path: Option<String>,
    /// `local`, `global`, `auto`, `glob`, `external`, `unresolved`.
    pub via: String,
    pub package: Option<String>,
}

#[derive(Debug, Default)]
pub struct Registry {
    /// Pascal-cased name → registrations.
    pub globals: HashMap<String, Vec<GlobalName>>,
    /// path → (kind, primary name, other names).
    pub components: BTreeMap<String, (String, String, Vec<String>)>,
    pub uses: Vec<Use>,
    /// path → (namespace, module name). The root store is `("", "root")`;
    /// a non-namespaced module shares its parent's namespace.
    pub vuex_namespaces: BTreeMap<String, (String, String)>,
    pub pages: Vec<crate::pages::PageRoute>,
    pub api_calls: Vec<crate::api::CallRow>,
    /// Next.js route handlers, stored as routes (source `next`).
    pub next_handlers: Vec<crate::next::Handler>,
    /// `(file id, kind, target, line)`: resolved imports (`path#name`) and
    /// composable / store calls by name, for find usages.
    pub refs: Vec<(i64, &'static str, String, u32)>,
    pub notes: Vec<String>,
}

pub fn build(
    root: &Path,
    d: &VueDetection,
    modules: &[Module],
    files: &HashSet<String>,
) -> Registry {
    let resolver = Resolver::new(files, &d.aliases);
    let mut reg = Registry::default();
    let by_path: HashMap<&str, &Module> = modules.iter().map(|m| (m.path.as_str(), m)).collect();

    // Nuxt auto-imports: computed from the component dirs, plus the generated list.
    if let Some(n) = &d.nuxt
        && n.auto_import_components
    {
        for m in modules {
            for dir in &d.component_dirs {
                if let Some(name) = naming::nuxt_component_name(dir, &m.path) {
                    add_global(&mut reg, &name, Some(m.path.clone()), None, "auto", None);
                    break;
                }
            }
        }
        match naming::read_generated(root, ".nuxt") {
            Some(generated) => {
                for g in generated {
                    let known = reg.globals.get(&pascal(&g.name)).is_some_and(|v| !v.is_empty());
                    match (&g.path, &g.package) {
                        (Some(p), _) if !known && files.contains(p) => {
                            add_global(&mut reg, &g.name, Some(p.clone()), None, "auto", None)
                        }
                        (None, Some(pkg)) if !known => {
                            add_global(&mut reg, &g.name, None, Some(pkg.clone()), "auto", None)
                        }
                        _ => {}
                    }
                }
            }
            None => reg.notes.push(
                ".nuxt/components.d.ts not found (run `nuxi prepare` once); library components are matched by prefix only".into(),
            ),
        }
    }

    // `Vue.component('X', Comp)` / `app.component(...)` and require.context globs.
    for m in modules {
        let script = &m.facts.script;
        for r in &script.registrations {
            let at = format!("{}:{}", m.path, r.line);
            let spec = r
                .source
                .clone()
                .or_else(|| r.local.as_deref().and_then(|l| import_source(&m.facts, l)));
            let resolved = spec.map(|s| resolver.resolve(&m.path, &s));
            if r.plugin {
                // Only package plugins matter here: project plugins register
                // their components with `.component()` calls we already read.
                if let Some(Resolved::Package(p)) = resolved {
                    add_global(&mut reg, &r.name, None, Some(p), "plugin", Some(at));
                }
                continue;
            }
            let (path, package) = match resolved {
                Some(Resolved::File(p)) => (Some(p), None),
                Some(Resolved::Package(p)) => (None, Some(p)),
                // A component defined in the same file.
                None if r.local.is_some() => (Some(m.path.clone()), None),
                _ => (None, None),
            };
            add_global(&mut reg, &r.name, path, package, "global", Some(at));
        }
        for g in &script.registration_globs {
            let dir_pattern = g
                .pattern
                .split('*')
                .next()
                .unwrap_or(&g.pattern)
                .trim_end_matches('/');
            let base =
                crate::paths::join(m.path.rsplit_once('/').map_or("", |(d, _)| d), dir_pattern);
            let base = resolve_alias_dir(&d.aliases, dir_pattern).unwrap_or(base);
            let prefix = format!("{base}/");
            for f in files
                .iter()
                .filter(|f| f.ends_with(".vue") && f.starts_with(&prefix))
            {
                let rel = &f[prefix.len()..];
                if !g.recursive && rel.contains('/') {
                    continue;
                }
                let stem = rel
                    .rsplit('/')
                    .next()
                    .unwrap_or(rel)
                    .trim_end_matches(".vue");
                add_global(
                    &mut reg,
                    stem,
                    Some(f.clone()),
                    None,
                    "glob",
                    Some(format!("{}:{}", m.path, g.line)),
                );
            }
        }
    }

    // Kinds and names of component files.
    let pages = d.is_nuxt().then(|| format!("{}/", d.in_src("pages")));
    let layouts = d.is_nuxt().then(|| format!("{}/", d.in_src("layouts")));
    let mut names_by_path: HashMap<String, Vec<String>> = HashMap::new();
    for (key, entries) in &reg.globals {
        for e in entries {
            if let Some(p) = &e.path {
                let list = names_by_path.entry(p.clone()).or_default();
                if !list.contains(key) {
                    list.push(key.clone());
                }
            }
        }
    }
    for m in modules {
        // `.js`/`.ts` options objects are components only with a template or
        // render function; otherwise they are mixins or plain config.
        let is_component =
            m.facts.lang == "vue" || m.facts.script.options.as_ref().is_some_and(|o| o.renders);
        // React: a .tsx/.jsx file with a function component.
        let react = !is_component
            && (m.path.ends_with(".tsx") || m.path.ends_with(".jsx"))
            && !m.facts.script.react_components.is_empty();
        if !is_component && !react {
            continue;
        }
        if react {
            let (kind, primary) = if let Some(url) = crate::next::page_url(d, &m.path) {
                ("page", crate::next::page_name(&url))
            } else if let Some(url) = crate::next::layout_url(d, &m.path) {
                (
                    "layout",
                    format!(
                        "layout{}",
                        if url == "/" {
                            String::new()
                        } else {
                            url.replace('/', "-")
                        }
                    ),
                )
            } else {
                let main = m
                    .facts
                    .script
                    .react_components
                    .iter()
                    .find(|c| c.default_export && c.name != "default")
                    .or_else(|| m.facts.script.react_components.iter().find(|c| c.exported))
                    .map(|c| c.name.clone());
                ("component", main.unwrap_or_else(|| file_stem_name(&m.path)))
            };
            // Other exported components in the same file are names too.
            let others: Vec<String> = m
                .facts
                .script
                .react_components
                .iter()
                .filter(|c| c.exported && c.name != primary && c.name != "default")
                .map(|c| c.name.clone())
                .collect();
            reg.components
                .insert(m.path.clone(), (kind.to_owned(), primary, others));
            continue;
        }
        let kind = if pages
            .as_ref()
            .is_some_and(|p| m.path.starts_with(p.as_str()))
        {
            "page"
        } else if layouts
            .as_ref()
            .is_some_and(|p| m.path.starts_with(p.as_str()))
        {
            "layout"
        } else {
            "component"
        };
        let declared = m
            .facts
            .script
            .setup
            .name
            .clone()
            .or_else(|| m.facts.script.options.as_ref().and_then(|o| o.name.clone()));
        let mut names = names_by_path.remove(&m.path).unwrap_or_default();
        let primary = match kind {
            // Nuxt's route name (`orders-id`) and layout name (`admin`).
            "page" => nuxt_route_name(&m.path[pages.as_ref().map_or(0, |p| p.len())..]),
            "layout" => {
                let rel = &m.path[layouts.as_ref().map_or(0, |p| p.len())..];
                codemap_js::template::kebab(rel.rsplit_once('.').map_or(rel, |(s, _)| s))
                    .replace('/', "-")
            }
            _ => names
                .first()
                .cloned()
                .or_else(|| declared.as_deref().map(pascal))
                .unwrap_or_else(|| file_stem_name(&m.path)),
        };
        // Other names are real registrations or the declared `name`, never the file name.
        if let Some(extra) = declared.as_deref().map(pascal)
            && !names.contains(&extra)
        {
            names.push(extra);
        }
        names.retain(|n| *n != primary);
        reg.components
            .insert(m.path.clone(), (kind.to_owned(), primary, names));
    }

    // Which component each template tag renders.
    let ui_prefixes = ui_prefixes(d);
    for m in modules {
        for t in &m.facts.tags {
            let u = resolve_tag(&reg, d, &resolver, m, &t.tag, &ui_prefixes);
            reg.uses.push(Use {
                from_path: m.path.clone(),
                tag: t.tag.clone(),
                line: t.line,
                to_path: u.0,
                via: u.1,
                package: u.2,
            });
        }
    }

    vuex_namespaces(d, modules, &by_path, &resolver, &mut reg);
    reg.pages = crate::pages::build(d, modules, &resolver);
    reg.pages.extend(crate::next::pages(d, modules));
    reg.next_handlers = crate::next::handlers(d, modules);
    reg.api_calls = crate::api::rows(modules, &by_path, &resolver, &reg.uses);
    for m in modules {
        for imp in m.facts.script.imports.iter().filter(|i| !i.type_only) {
            let Resolved::File(target) = resolver.resolve(&m.path, &imp.source) else {
                continue;
            };
            let mut names: Vec<&str> = Vec::new();
            if imp.default.is_some() || imp.dynamic {
                names.push("default");
            }
            if imp.namespace.is_some() {
                names.push("*");
            }
            names.extend(imp.named.iter().map(|(i, _)| i.as_str()));
            if names.is_empty() {
                names.push("");
            }
            for n in names {
                reg.refs
                    .push((m.file_id, "import", format!("{target}#{n}"), imp.line));
            }
        }
        let calls = m
            .facts
            .script
            .composables
            .iter()
            .chain(&m.facts.script.unbound_calls);
        for c in calls {
            reg.refs
                .push((m.file_id, "js_call", c.name.clone(), c.line));
        }
        // `supabase.from('rooms').select()`: the table, with what is done to it.
        for t in &m.facts.script.db_tables {
            reg.refs
                .push((m.file_id, "db_table", t.name.clone(), t.line));
        }
    }
    if !d.is_nuxt() {
        // vue-router: files rendered by leaf routes are pages.
        for p in &reg.pages {
            if let Some(f) = &p.file
                && !p.has_children
                && let Some(c) = reg.components.get_mut(f)
                && c.0 == "component"
            {
                c.0 = "page".into();
            }
        }
    }
    reg
}

fn resolve_alias_dir(aliases: &[(String, String)], pattern: &str) -> Option<String> {
    aliases.iter().find_map(|(a, dir)| {
        let rest = pattern.strip_prefix(a.as_str())?.strip_prefix('/')?;
        Some(crate::paths::join(dir, rest))
    })
}

fn add_global(
    reg: &mut Registry,
    name: &str,
    path: Option<String>,
    package: Option<String>,
    via: &str,
    at: Option<String>,
) {
    let entry = GlobalName {
        name: name.to_owned(),
        path,
        package,
        via: via.to_owned(),
        registered_at: at,
    };
    let list = reg.globals.entry(pascal(name)).or_default();
    if !list.contains(&entry) {
        list.push(entry);
    }
}

/// Nuxt route names: `orders/[id].vue` → `orders-id`, `index.vue` → `index`,
/// Nuxt 2 `orders/_id.vue` → `orders-id`, `[...slug].vue` → `slug`.
pub fn nuxt_route_name(rel: &str) -> String {
    let stem = rel.rsplit_once('.').map_or(rel, |(s, _)| s);
    let parts: Vec<String> = stem
        .split('/')
        .filter(|seg| *seg != "index")
        .map(|seg| {
            seg.trim_start_matches('_')
                .replace(['[', ']'], "")
                .trim_start_matches("...")
                .to_owned()
        })
        .filter(|seg| !seg.is_empty())
        .collect();
    if parts.is_empty() {
        "index".into()
    } else {
        parts.join("-")
    }
}

/// `index.vue` → its folder's name; `order-card.vue` → `OrderCard`.
fn file_stem_name(path: &str) -> String {
    let mut segs = path.rsplit('/');
    let file = segs.next().unwrap_or(path);
    let stem = file.split('.').next().unwrap_or(file);
    if stem.eq_ignore_ascii_case("index")
        && let Some(dir) = segs.next()
    {
        return pascal(dir);
    }
    pascal(stem)
}

/// The import source that binds `local` in this module.
pub fn import_source(facts: &ModuleFacts, local: &str) -> Option<String> {
    facts
        .script
        .imports
        .iter()
        .find(|i| !i.dynamic && i.binds(local))
        .map(|i| i.source.clone())
}

fn resolve_tag(
    reg: &Registry,
    d: &VueDetection,
    resolver: &Resolver<'_>,
    m: &Module,
    tag: &str,
    ui_prefixes: &[(&str, &str)],
) -> (Option<String>, String, Option<String>) {
    let key = pascal(tag);
    let from_spec = |spec: &str, via: &str| match resolver.resolve(&m.path, spec) {
        Resolved::File(p) => (Some(p), via.to_owned(), None),
        Resolved::Package(p) => (None, "external".to_owned(), Some(p)),
        Resolved::Missing(p) => (
            None,
            "unresolved".to_owned(),
            Some(format!("import not found: {p}")),
        ),
    };
    // Local registration (Options API `components: {}`).
    if let Some(o) = &m.facts.script.options {
        for (name, local) in &o.components {
            if pascal(name) == key {
                if local == "inline" {
                    return (None, "inline".to_owned(), None);
                }
                if let Some(src) = local.strip_prefix("import:") {
                    return from_spec(src, "local");
                }
                if let Some(src) = import_source(&m.facts, local) {
                    return from_spec(&src, "local");
                }
            }
        }
    }
    // `<script setup>`: imported components are usable directly.
    if m.facts.has_script_setup || m.facts.lang != "vue" {
        for imp in m
            .facts
            .script
            .imports
            .iter()
            .filter(|i| !i.dynamic && !i.type_only)
        {
            let bound = imp
                .default
                .iter()
                .chain(imp.named.iter().map(|(_, l)| l))
                .any(|l| pascal(l) == key);
            if bound {
                return from_spec(&imp.source, "local");
            }
        }
    }
    // React: a component defined in the same file (`function SaveButton()`),
    // and context providers (`<ThemeContext.Provider>`).
    if m.facts.lang != "vue" {
        if m.facts
            .script
            .react_components
            .iter()
            .any(|c| c.name == tag)
            || m.facts.script.decls.iter().any(|d| d.name == tag)
        {
            return (Some(m.path.clone()), "local".into(), None);
        }
        if tag.ends_with(".Provider") || tag.ends_with(".Consumer") {
            return (None, "context".into(), None);
        }
    }
    // A component may render itself by its file name (recursion).
    if m.facts.lang == "vue" && file_stem_name(&m.path) == key {
        return (None, "self".into(), None);
    }
    // `<script setup>` bindings: `const Row = defineComponent(...)`,
    // `const { DefineX, ReuseX } = createReusableTemplate()`.
    if m.facts
        .script
        .setup_bindings
        .iter()
        .any(|b| pascal(b) == key)
    {
        return (None, "local-binding".into(), None);
    }
    // Options API files that import a component from a package and register it
    // through a plugin (`Vue.use(VueSignaturePad)` then `<VueSignaturePad>`).
    for imp in m
        .facts
        .script
        .imports
        .iter()
        .filter(|i| !i.dynamic && !i.type_only)
    {
        let bound = imp
            .default
            .iter()
            .chain(imp.named.iter().map(|(_, l)| l))
            .any(|l| pascal(l) == key);
        if bound && let Resolved::Package(p) = resolver.resolve(&m.path, &imp.source) {
            return (None, "external".into(), Some(p));
        }
    }
    let lookup = |k: &str| reg.globals.get(k).and_then(|v| v.first());
    let entry = lookup(&key).or_else(|| key.strip_prefix("Lazy").and_then(lookup));
    if let Some(g) = entry {
        let via = if g.package.is_some() {
            "external".to_owned()
        } else {
            g.via.clone()
        };
        return (g.path.clone(), via, g.package.clone());
    }
    if let Some(pkg) = library_tag(d, &key, ui_prefixes) {
        return (None, "external".into(), Some(pkg));
    }
    (None, "unresolved".into(), None)
}

/// Component libraries the project depends on: `UButton` → `@nuxt/ui`,
/// `VBtn` → `vuetify`, `NuxtLink` → `nuxt`, `I18nT` → `vue-i18n`.
pub fn library_tag(d: &VueDetection, key: &str, ui_prefixes: &[(&str, &str)]) -> Option<String> {
    for (pkg, prefix) in ui_prefixes {
        if key.starts_with(prefix)
            && key[prefix.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_uppercase())
        {
            return Some((*pkg).to_owned());
        }
    }
    if d.is_nuxt()
        && (key.starts_with("Nuxt")
            || matches!(key, "ClientOnly" | "DevOnly" | "Icon" | "ColorScheme"))
    {
        return Some("nuxt".into());
    }
    if matches!(key, "I18nT" | "I18nN" | "I18nD" | "I18n")
        && (d.library("vue-i18n").is_some()
            || d.library("@nuxtjs/i18n").is_some()
            || d.library("nuxt-i18n").is_some())
    {
        return Some("vue-i18n".into());
    }
    if d.library("@inertiajs/vue3")
        .or(d.library("@inertiajs/vue2"))
        .is_some()
        && matches!(key, "Link" | "Head" | "Deferred" | "WhenVisible")
    {
        return Some("@inertiajs".into());
    }
    None
}

/// The prefixes of the UI libraries this project depends on.
pub fn ui_prefixes(d: &VueDetection) -> Vec<(&'static str, &'static str)> {
    UI_LIBRARIES
        .iter()
        .filter(|(pkg, prefix)| !prefix.is_empty() && d.ui.iter().any(|u| u.name == *pkg))
        .map(|(pkg, prefix)| (*pkg, *prefix))
        .collect()
}

fn vuex_namespaces(
    d: &VueDetection,
    modules: &[Module],
    by_path: &HashMap<&str, &Module>,
    resolver: &Resolver<'_>,
    reg: &mut Registry,
) {
    // Nuxt 2: every file under store/ is a module named by its path.
    if d.nuxt.as_ref().is_some_and(|n| n.major == Some(2)) {
        let prefix = format!("{}/", d.in_src("store"));
        for m in modules {
            if m.facts.script.vuex_module.is_some()
                && let Some(rel) = m.path.strip_prefix(&prefix)
            {
                let ns = rel.rsplit_once('.').map_or(rel, |(s, _)| s);
                let ns = ns.strip_suffix("/index").unwrap_or(ns);
                let (ns, name) = if ns == "index" {
                    (String::new(), "root".to_owned())
                } else {
                    (ns.to_owned(), ns.to_owned())
                };
                reg.vuex_namespaces.insert(m.path.clone(), (ns, name));
            }
        }
    }
    // `new Vuex.Store({ modules })` / `createStore`, then nested `modules`.
    let mut queue: Vec<(String, String, String)> = modules
        .iter()
        .filter(|m| m.facts.script.vuex_module.as_ref().is_some_and(|v| v.root))
        .map(|m| (m.path.clone(), String::new(), "root".to_owned()))
        .collect();
    let mut seen = HashSet::new();
    while let Some((path, ns, label)) = queue.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        reg.vuex_namespaces
            .entry(path.clone())
            .or_insert_with(|| (ns.clone(), label));
        let Some(m) = by_path.get(path.as_str()) else {
            continue;
        };
        let Some(v) = &m.facts.script.vuex_module else {
            continue;
        };
        for (name, local) in &v.modules {
            let Some(src) = import_source(&m.facts, local) else {
                continue;
            };
            if let Resolved::File(target) = resolver.resolve(&m.path, &src) {
                let namespaced = by_path
                    .get(target.as_str())
                    .and_then(|t| t.facts.script.vuex_module.as_ref())
                    .is_some_and(|t| t.namespaced);
                let child = if namespaced {
                    if ns.is_empty() {
                        name.clone()
                    } else {
                        format!("{ns}/{name}")
                    }
                } else {
                    ns.clone()
                };
                let label = if ns.is_empty() {
                    name.clone()
                } else {
                    format!("{ns}/{name}")
                };
                queue.push((target, child, label));
            }
        }
    }
}

/// Writes the registry: module kinds/names, component and Vuex symbols, uses.
pub fn store(store: &mut Store, modules: &[Module], reg: &Registry) -> Result<usize> {
    let ids: HashMap<&str, i64> = modules
        .iter()
        .map(|m| (m.path.as_str(), m.file_id))
        .collect();
    let tx = store.conn_mut().transaction()?;
    tx.execute("UPDATE js_modules SET kind = 'module', name = NULL", [])?;
    tx.execute(
        "DELETE FROM symbols WHERE kind = 'component' OR (kind = 'store' AND meta_json LIKE '%\"type\":\"vuex\"%')",
        [],
    )?;
    tx.execute("DELETE FROM component_uses", [])?;
    crate::pages::store(&tx, &reg.pages)?;
    tx.execute("DELETE FROM routes WHERE source = 'next'", [])?;
    {
        let mut ins = tx.prepare(
            "INSERT INTO routes (seq, methods, uri, action_kind, action_raw, middleware_json, file, line, source)
             VALUES (?1, ?2, ?3, 'handler', ?4, '[]', ?5, ?6, 'next')",
        )?;
        for (i, h) in reg.next_handlers.iter().enumerate() {
            ins.execute(rusqlite::params![
                100_000 + i as i64,
                h.methods.join("|"),
                h.uri,
                format!("{}#{}", h.file, h.methods.join(",")),
                h.file,
                h.line,
            ])?;
        }
    }
    crate::api::store(&tx, &reg.api_calls)?;
    tx.execute(
        "DELETE FROM refs WHERE file_id IN (SELECT file_id FROM js_modules)",
        [],
    )?;
    {
        let mut ins =
            tx.prepare("INSERT INTO refs (file_id, kind, target, line) VALUES (?1, ?2, ?3, ?4)")?;
        for (id, kind, target, line) in &reg.refs {
            ins.execute(params![id, kind, target, line])?;
        }
    }
    tx.execute("DELETE FROM i18n_keys WHERE source = 'vue'", [])?;
    tx.execute("DELETE FROM i18n_uses WHERE source = 'vue'", [])?;
    {
        let mut key = tx.prepare(
            "INSERT INTO i18n_keys (file_id, source, locale, key) VALUES (?1, 'vue', ?2, ?3)",
        )?;
        let mut used = tx.prepare(
            "INSERT INTO i18n_uses (file_id, source, key, line, dynamic, guarded) VALUES (?1, 'vue', ?2, ?3, ?4, ?5)",
        )?;
        for m in modules {
            for l in m.facts.locale.iter().chain(&m.facts.inverted_locales) {
                for k in &l.keys {
                    key.execute(params![m.file_id, l.locale, k])?;
                }
            }
            for u in m
                .facts
                .script
                .i18n_uses
                .iter()
                .chain(&m.facts.template_i18n)
            {
                used.execute(params![m.file_id, u.key, u.line, u.dynamic, u.guarded])?;
            }
        }
    }
    let bulk = reg.components.len() >= symbols::BULK_FTS_THRESHOLD;
    if bulk {
        symbols::bulk_insert_begin(&tx)?;
    }
    let by_path: HashMap<&str, &Module> = modules.iter().map(|m| (m.path.as_str(), m)).collect();
    {
        let mut upd = tx.prepare("UPDATE js_modules SET kind = ?2, name = ?3 WHERE path = ?1")?;
        for (path, (kind, name, others)) in &reg.components {
            upd.execute(params![path, kind, name])?;
            let Some(m) = by_path.get(path.as_str()) else {
                continue;
            };
            let Some(&file_id) = ids.get(path.as_str()) else {
                continue;
            };
            let (props, _) = crate::component::props_of(&m.facts);
            let mut attrs = String::new();
            for p in &props {
                attrs.push_str(&format!(" :{}", p.name));
            }
            for e in m.facts.emits() {
                attrs.push_str(&format!(" @{e}"));
            }
            let mut sig = match kind.as_str() {
                "page" | "layout" if attrs.is_empty() => format!("{kind} '{name}'"),
                "page" | "layout" => format!("{kind} '{name}' <{}>", attrs.trim_start()),
                _ => format!("<{name}{attrs}>"),
            };
            if sig.chars().count() > 160 {
                sig = format!("{}…>", sig.chars().take(155).collect::<String>());
            }
            let doc = match kind.as_str() {
                "page" | "layout" => Some(format!("{kind} component")),
                _ => None,
            };
            let doc = match (doc, others.is_empty()) {
                (d, true) => d,
                (None, false) => Some(format!("also registered as {}", others.join(", "))),
                (Some(d), false) => Some(format!("{d}; also registered as {}", others.join(", "))),
            };
            symbols::insert(
                &tx,
                file_id,
                &NewSymbol {
                    kind: SymbolKind::Component,
                    name: name.clone(),
                    fqn: path.clone(),
                    parent_fqn: None,
                    signature: sig,
                    visibility: None,
                    is_static: false,
                    is_abstract: false,
                    is_final: false,
                    start_line: 1,
                    end_line: m.facts.lines.max(1),
                    doc_summary: doc,
                    meta: SymbolMeta {
                        r#type: Some(kind.clone()),
                        ..SymbolMeta::default()
                    },
                },
            )?;
        }
        for (path, (ns, label)) in &reg.vuex_namespaces {
            let Some(m) = by_path.get(path.as_str()) else {
                continue;
            };
            let Some(v) = &m.facts.script.vuex_module else {
                continue;
            };
            let Some(&file_id) = ids.get(path.as_str()) else {
                continue;
            };
            let label = label.clone();
            let scope = if label == "root" {
                String::new()
            } else if *ns == label {
                " (namespaced)".to_owned()
            } else if ns.is_empty() {
                " (not namespaced: names are global)".to_owned()
            } else {
                format!(" (not namespaced: names are under '{ns}/')")
            };
            let mut parts = Vec::new();
            for (l, list) in [
                ("state", &v.state),
                ("getters", &v.getters),
                ("mutations", &v.mutations),
                ("actions", &v.actions),
            ] {
                if !list.is_empty() {
                    parts.push(format!("{l}: {}", list.join(", ")));
                }
            }
            symbols::insert(
                &tx,
                file_id,
                &NewSymbol {
                    kind: SymbolKind::Store,
                    name: label.clone(),
                    fqn: path.clone(),
                    parent_fqn: None,
                    signature: format!("Vuex module '{label}'{scope}"),
                    visibility: None,
                    is_static: false,
                    is_abstract: false,
                    is_final: false,
                    start_line: v.line,
                    end_line: m.facts.lines.max(v.line),
                    doc_summary: (!parts.is_empty()).then(|| parts.join(" · ")),
                    meta: SymbolMeta {
                        value: Some(label),
                        r#type: Some("vuex".into()),
                        ..SymbolMeta::default()
                    },
                },
            )?;
        }
        let mut ins = tx.prepare(
            "INSERT INTO component_uses (from_path, tag, line, to_path, via, package) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for u in &reg.uses {
            ins.execute(params![
                u.from_path,
                u.tag,
                u.line,
                u.to_path,
                u.via,
                u.package
            ])?;
        }
    }
    if bulk {
        symbols::bulk_insert_end(&tx)?;
    }
    tx.commit()?;
    Ok(reg.components.len())
}
