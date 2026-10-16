//! Answers for one component: its API (props, emits, v-model, slots), what it
//! renders, who renders it, and which stores / composables it uses.

use codemap_core::envelope::Detail;
use codemap_core::symbols::{self, SearchInput, SymbolKind};
use codemap_core::{Result, Store};
use codemap_js::Prop;
use codemap_js::template::pascal;
use serde_json::{Value, json};

use crate::extract::ModuleFacts;

/// Props (declared + `defineModel`) and a note when some could not be read.
pub fn props_of(f: &ModuleFacts) -> (Vec<Prop>, Option<String>) {
    let mut props: Vec<Prop> = f
        .script
        .options
        .iter()
        .flat_map(|o| o.props.clone())
        .collect();
    props.extend(f.script.setup.props.iter().cloned());
    for m in &f.script.setup.models {
        if !props.iter().any(|p| p.name == m.name) {
            props.push(m.clone());
        }
    }
    (props, f.script.setup.props_unresolved.clone())
}

/// `v-model` bindings: `defineModel`, Vue 3 `modelValue` + `update:modelValue`
/// pairs, and the Vue 2 `model` option / `value` + `input` default.
pub fn models_of(f: &ModuleFacts, vue_major: Option<u32>) -> Vec<String> {
    let mut out: Vec<String> = f
        .script
        .setup
        .models
        .iter()
        .map(|m| {
            if m.name == "modelValue" {
                "v-model".to_owned()
            } else {
                format!("v-model:{}", m.name)
            }
        })
        .collect();
    let (props, _) = props_of(f);
    let emits = f.emits();
    if let Some((prop, event)) = f.script.options.as_ref().and_then(|o| o.model.clone()) {
        out.push(format!("v-model (prop `{prop}`, event `{event}`)"));
    } else if vue_major == Some(2)
        && props.iter().any(|p| p.name == "value")
        && emits.iter().any(|e| e == "input")
    {
        out.push("v-model (prop `value`, event `input`)".into());
    }
    for p in &props {
        let event = format!("update:{}", p.name);
        if emits.contains(&event) && !f.script.setup.models.iter().any(|m| m.name == p.name) {
            out.push(if p.name == "modelValue" {
                "v-model".into()
            } else {
                format!("v-model:{}", p.name)
            });
        }
    }
    out.dedup();
    out
}

fn prop_line(p: &Prop) -> String {
    let mut s = p.name.clone();
    if let Some(t) = &p.r#type {
        s.push_str(&format!(": {t}"));
    }
    if p.required {
        s.push_str(" (required)");
    }
    if let Some(d) = &p.default {
        s.push_str(&format!(" = {d}"));
    }
    s
}

struct Row {
    path: String,
    kind: String,
    name: Option<String>,
    facts: ModuleFacts,
}

fn rows(store: &Store, sql: &str, arg: &str) -> Result<Vec<Row>> {
    let mut stmt = store.conn().prepare(sql)?;
    let rows = stmt.query_map([arg], |r| {
        Ok(Row {
            path: r.get(0)?,
            kind: r.get(1)?,
            name: r.get(2)?,
            facts: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default(),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Finds components by path, name, tag (`base-button`), or a registered alias.
fn find(store: &Store, query: &str) -> Result<Vec<Row>> {
    let q = query
        .trim()
        .trim_start_matches("./")
        .trim_start_matches('<')
        .trim_end_matches('>');
    const COLS: &str = "SELECT path, kind, name, facts_json FROM js_modules";
    let by_path = rows(store, &format!("{COLS} WHERE path = ?1"), q)?;
    if !by_path.is_empty() {
        return Ok(by_path);
    }
    let name = pascal(q.rsplit('/').next().unwrap_or(q).trim_end_matches(".vue"));
    let by_name = rows(
        store,
        &format!("{COLS} WHERE name = ?1 COLLATE NOCASE AND kind <> 'module'"),
        &name,
    )?;
    if !by_name.is_empty() {
        return Ok(by_name);
    }
    // Aliases: the tag resolves to a component through component_uses or registrations.
    let via_uses = rows(
        store,
        &format!(
            "{COLS} WHERE path IN (SELECT DISTINCT to_path FROM component_uses WHERE to_path IS NOT NULL AND tag = ?1 COLLATE NOCASE)"
        ),
        q,
    )?;
    if !via_uses.is_empty() {
        return Ok(via_uses);
    }
    // Path suffix: `orders/List.vue`.
    if q.contains('/') {
        return rows(store, &format!("{COLS} WHERE path LIKE '%/' || ?1"), q);
    }
    Ok(Vec::new())
}

pub fn get_component(
    store: &Store,
    query: &str,
    detail: Detail,
    vue_major: Option<u32>,
) -> Result<Value> {
    if !crate::stored_detection(store)?.is_some_and(|d| d.vue) {
        return Ok(json!({
            "found": false,
            "query": query,
            "hint": "No Vue / Nuxt app was detected in this project (no vue or nuxt in package.json). If the frontend is a separate repository, run codemap there.",
        }));
    }
    let found = find_with_kebab(store, query)?;
    if found.is_empty() {
        let s = symbols::search(
            store.conn(),
            &SearchInput {
                query: query.to_owned(),
                kind: Some(SymbolKind::Component),
                limit: Some(5),
                ..SearchInput::default()
            },
        )?;
        let suggestions = if s.data["found"] == true {
            s.data["matches"].clone()
        } else {
            s.data["suggestions"].clone()
        };
        let hint = suggestions
            .get(0)
            .and_then(|m| m["name"].as_str())
            .map_or_else(
                || format!("No component named {query}."),
                |n| format!("Did you mean {n}?"),
            );
        return Ok(
            json!({ "found": false, "query": query, "suggestions": suggestions, "hint": hint }),
        );
    }
    if found.len() > 1 {
        let list: Vec<Value> = found
            .iter()
            .map(|r| json!({ "name": r.name, "file": r.path, "kind": r.kind }))
            .collect();
        return Ok(json!({
            "found": true,
            "ambiguous": true,
            "candidates": list,
            "hint": "Several components match; call get_component with a file path.",
        }));
    }
    let r = &found[0];
    let f = &r.facts;
    let full = detail == Detail::Full;
    let api = if let Some(o) = &f.script.options {
        match (o.style.as_str(), f.has_script_setup) {
            (_, true) => "script setup + options".to_owned(),
            ("class", _) => "class component".to_owned(),
            ("defineComponent", _) if o.has_setup => "defineComponent with setup()".to_owned(),
            (s, _) => format!("options API ({s})"),
        }
    } else if f.has_script_setup {
        "script setup".to_owned()
    } else if f.lang == "vue" {
        "template only".to_owned()
    } else {
        "js module".to_owned()
    };
    let (props, props_note) = props_of(f);
    let mut v = json!({
        "found": true,
        "name": r.name,
        "file": r.path,
        "kind": r.kind,
        "api": api,
        "props": props.iter().map(|p| if full { json!({"name": p.name, "type": p.r#type, "required": p.required, "default": p.default, "line": p.line}) } else { json!(prop_line(p)) }).collect::<Vec<_>>(),
        "emits": f.emits(),
    });
    let o = v.as_object_mut().expect("object");
    if let Some(n) = props_note {
        o.insert("props_unresolved".into(), json!(n));
    }
    let models = models_of(f, vue_major);
    if !models.is_empty() {
        o.insert("v_model".into(), json!(models));
    }
    let slots: Vec<&str> = f
        .slots
        .iter()
        .map(|s| s.name.as_str())
        .chain(f.script.setup.slots.iter().map(|s| s.name.as_str()))
        .collect();
    if !slots.is_empty() {
        let mut unique: Vec<&str> = Vec::new();
        for s in slots {
            if !unique.contains(&s) {
                unique.push(s);
            }
        }
        o.insert("slots".into(), json!(unique));
    }

    // What it renders.
    let uses: Vec<Value> = {
        let mut stmt = store.conn().prepare(
            "SELECT tag, line, to_path, via, package FROM component_uses WHERE from_path = ?1 ORDER BY line",
        )?;
        let rows = stmt.query_map([&r.path], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?;
        rows.flatten()
            .map(|(tag, line, to, via, pkg)| {
                if full {
                    json!({ "tag": tag, "line": line, "component": to, "via": via, "package": pkg })
                } else {
                    let target = match (&to, via.as_str(), &pkg) {
                        (Some(p), _, _) => p.clone(),
                        (None, "external", Some(pkg)) => format!("external: {pkg}"),
                        (None, _, Some(note)) => format!("unresolved ({note})"),
                        _ => "unresolved".to_owned(),
                    };
                    json!(format!("<{tag}> :{line} → {target}"))
                }
            })
            .collect()
    };
    if !uses.is_empty() {
        o.insert("renders".into(), json!(uses));
    }
    let routes = crate::pages::routes_for_file(store, &r.path)?;
    if !routes.is_empty() {
        o.insert("routes".into(), json!(routes));
    }
    // Who renders it.
    let (used_by, total) = used_by(store, &r.path, if full { 200 } else { 25 })?;
    o.insert("used_by".into(), json!(used_by));
    if total > used_by.len() {
        o.insert("used_by_total".into(), json!(total));
    }

    // Stores and composables.
    let mut stores = Vec::new();
    let mut composables = Vec::new();
    for c in &f.script.composables {
        let target = symbols::all_named(store.conn(), &c.name)?
            .into_iter()
            .find(|s| {
                matches!(
                    s.kind,
                    SymbolKind::Store | SymbolKind::Composable | SymbolKind::Function
                )
            });
        match target {
            Some(s) if s.kind == SymbolKind::Store => {
                stores.push(json!(format!("{} → {}:{}", c.name, s.file, s.line)))
            }
            Some(s) => composables.push(json!(format!(
                "{} :{} → {}:{}",
                c.name, c.line, s.file, s.line
            ))),
            None => composables.push(json!(format!("{} :{}", c.name, c.line))),
        }
    }
    for u in &f.script.vuex_uses {
        stores.push(json!(format!(
            "vuex {} {}{} :{}",
            u.kind,
            u.namespace
                .as_ref()
                .map(|n| format!("{n}/"))
                .unwrap_or_default(),
            u.name,
            u.line
        )));
    }
    if !stores.is_empty() {
        o.insert("stores".into(), json!(stores));
    }
    if !composables.is_empty() {
        o.insert("composables".into(), json!(composables));
    }
    if let Some(opts) = &f.script.options {
        if !opts.mixins.is_empty() {
            let mixins: Vec<String> = opts
                .mixins
                .iter()
                .map(|m| match crate::registry::import_source(f, m) {
                    Some(src) => format!("{m} ({src})"),
                    None => m.clone(),
                })
                .collect();
            o.insert("mixins".into(), json!(mixins));
        }
        if opts.layout.is_some() || !opts.middleware.is_empty() {
            o.insert(
                "page".into(),
                json!({ "layout": opts.layout, "middleware": opts.middleware }),
            );
        }
        if full {
            o.insert(
                "options".into(),
                json!({
                    "data": opts.data, "computed": opts.computed, "methods": opts.methods,
                    "watch": opts.watch, "inject": opts.inject, "provide": opts.provide,
                }),
            );
        } else if !opts.methods.is_empty() {
            o.insert("methods".into(), json!(opts.methods));
        }
    }
    if !f.script.setup.page_meta.is_empty() {
        let meta: serde_json::Map<String, Value> = f
            .script
            .setup
            .page_meta
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();
        o.insert("page_meta".into(), Value::Object(meta));
    }
    if full {
        o.insert(
            "imports".into(),
            json!(
                f.script
                    .imports
                    .iter()
                    .filter(|i| !i.type_only)
                    .map(|i| format!("{} :{}", i.source, i.line))
                    .collect::<Vec<_>>()
            ),
        );
        if !f.script.setup.expose.is_empty() {
            o.insert("expose".into(), json!(f.script.setup.expose));
        }
        if !f.script.setup.provide.is_empty() || !f.script.setup.inject.is_empty() {
            o.insert(
                "provide_inject".into(),
                json!({ "provide": f.script.setup.provide, "inject": f.script.setup.inject }),
            );
        }
        if !f.custom_blocks.is_empty() {
            o.insert("custom_blocks".into(), json!(f.custom_blocks));
        }
        o.insert("lines".into(), json!(f.lines));
    }
    Ok(v)
}

fn find_with_kebab(store: &Store, query: &str) -> Result<Vec<Row>> {
    let found = find(store, query)?;
    if !found.is_empty() || !query.contains('-') {
        return Ok(found);
    }
    find(store, &pascal(query))
}

/// Files that render the component at `path`, with the first line.
pub fn used_by(store: &Store, path: &str, limit: usize) -> Result<(Vec<Value>, usize)> {
    let mut stmt = store.conn().prepare(
        "SELECT from_path, MIN(line) FROM component_uses WHERE to_path = ?1 AND from_path != ?1 GROUP BY from_path ORDER BY from_path",
    )?;
    let rows: Vec<(String, u32)> = stmt
        .query_map([path], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let total = rows.len();
    Ok((
        rows.into_iter()
            .take(limit)
            .map(|(f, l)| json!(format!("{f}:{l}")))
            .collect(),
        total,
    ))
}

/// Result of checking that a component exists (for `verify`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentCheck {
    Exists(String),
    External(String),
    NotFound(Vec<String>),
    NoFrontend,
}

/// Whether `<tag>` / `Name` is a project component, a registered alias, or a
/// library component; otherwise the closest names.
pub fn check(store: &Store, root: &std::path::Path, query: &str) -> Result<ComponentCheck> {
    let Some(d) = crate::stored_detection(store)? else {
        return Ok(ComponentCheck::NoFrontend);
    };
    if !d.vue {
        return Ok(ComponentCheck::NoFrontend);
    }
    let found = find_with_kebab(store, query)?;
    if let Some(r) = found.first() {
        return Ok(ComponentCheck::Exists(r.path.clone()));
    }
    let key = pascal(
        query
            .trim()
            .trim_start_matches('<')
            .trim_end_matches(['>', '/'])
            .trim(),
    );
    // Library components already seen in templates, or known by prefix.
    let seen: Option<String> = store
        .conn()
        .query_row(
            "SELECT package FROM component_uses WHERE via = 'external' AND package IS NOT NULL
             AND (tag = ?1 OR tag = ?2) LIMIT 1",
            [&key, &codemap_js::template::kebab(&key)],
            |r| r.get(0),
        )
        .ok();
    if let Some(pkg) = seen
        .or_else(|| crate::registry::library_tag(&d, &key, &crate::registry::ui_prefixes(&d)))
        .or_else(|| {
            crate::naming::read_generated(root, ".nuxt")?
                .into_iter()
                .find(|g| g.name == key && g.package.is_some())
                .and_then(|g| g.package)
        })
    {
        return Ok(ComponentCheck::External(pkg));
    }
    let mut stmt = store.conn().prepare(
        "SELECT DISTINCT name FROM js_modules WHERE kind = 'component' AND name IS NOT NULL",
    )?;
    let names: Vec<String> = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut scored: Vec<(f64, String)> = names
        .into_iter()
        .map(|n| {
            (
                strsim::jaro_winkler(&key.to_lowercase(), &n.to_lowercase()),
                n,
            )
        })
        .filter(|(s, _)| *s >= 0.8)
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    Ok(ComponentCheck::NotFound(
        scored.into_iter().take(5).map(|(_, n)| n).collect(),
    ))
}
