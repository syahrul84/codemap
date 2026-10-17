//! One Compose function for `get_component`: its parameters, the state it
//! keeps, the composables it shows, where it is shown and how it is opened;
//! and the Android section of `project_overview`.

use codemap_core::{Result, Store};
use rusqlite::params;
use serde_json::{Value, json};

use crate::index::{SOURCE, composables, load_files, manifest_class};

/// `None` when no composable matches `query` (a name, an FQN, or a file).
pub fn get_composable(store: &Store, query: &str) -> Result<Option<Value>> {
    let has: bool =
        store
            .conn()
            .query_row("SELECT EXISTS (SELECT 1 FROM android_files)", [], |r| {
                r.get(0)
            })?;
    if !has {
        return Ok(None);
    }
    let files = load_files(store.conn())?;
    let comps = composables(&files);
    let q = query.trim();
    let hit = comps
        .get(q)
        .copied()
        .or_else(|| comps.values().find(|(c, _)| c.fqn == q).copied())
        .or_else(|| {
            comps
                .values()
                .filter(|(_, p)| *p == q)
                .min_by_key(|(c, _)| c.line)
                .copied()
        })
        .or_else(|| {
            comps
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(q))
                .map(|(_, v)| *v)
        });
    let Some((c, file)) = hit else {
        return Ok(None);
    };
    let name = c.name.as_str();
    let params: Vec<Value> = c
        .params
        .iter()
        .map(|p| {
            let callback = p.r#type.as_deref().is_some_and(|t| t.contains("->"));
            json!({
                "name": p.name,
                "type": p.r#type,
                "required": p.default.is_none() && !p.r#type.as_deref().is_some_and(|t| t.ends_with('?')),
                "default": p.default,
                "callback": callback.then_some(true),
            })
        })
        .collect();
    let state: Vec<Value> = c
        .state
        .iter()
        .map(|s| json!({"name": s.name, "via": s.via, "initial": s.initial, "line": s.line}))
        .collect();
    let renders: Vec<Value> = c
        .calls
        .iter()
        .filter(|x| x.name != name)
        .filter_map(|x| {
            let (_, f) = comps.get(x.name.as_str())?;
            Some(json!({"composable": x.name, "file": f, "line": x.line}))
        })
        .collect();
    let mut used_in = Vec::new();
    for f in &files {
        for other in f.facts.composables.iter().filter(|o| o.name != name) {
            if let Some(x) = other.calls.iter().find(|x| x.name == name) {
                used_in.push(json!({"in": other.name, "at": format!("{}:{}", f.path, x.line)}));
            }
        }
        for s in f.facts.screens.iter().filter(|s| s.kind == "content") {
            if s.candidates.iter().any(|x| x == name) {
                used_in.push(json!({
                    "in": s.owner,
                    "at": format!("{}:{}", f.path, s.line),
                    "via": "setContent",
                }));
            }
        }
    }
    let mut opened_as = Vec::new();
    {
        let mut stmt = store.conn().prepare_cached(
            "SELECT route_path, defined_in, parent, meta FROM pages WHERE source = ?1 AND (name = ?2 OR meta LIKE ?3) ORDER BY defined_in",
        )?;
        let shows = format!("%\"shows\":\"{name}\"%");
        let rows = stmt.query_map(params![SOURCE, name, shows], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        for row in rows {
            let (kind, at, from, meta) = row?;
            let meta: Value = meta
                .and_then(|m| serde_json::from_str(&m).ok())
                .unwrap_or(Value::Null);
            opened_as.push(json!({
                "as": kind,
                "via": meta["via"],
                "route": meta["title"],
                "from": from.or_else(|| meta["class"].as_str().map(str::to_owned)),
                "at": at,
            }));
        }
    }
    let mut v = json!({
        "found": true,
        "name": name,
        "fqn": c.fqn,
        "kind": "composable",
        "file": file,
        "line": c.line,
        "end_line": c.end_line,
        "visibility": c.private.then_some("private"),
        "params": params,
        "state": state,
        "renders": renders,
        "used_in": used_in,
        "opened_as": opened_as,
    });
    let obj = v.as_object_mut().expect("object");
    obj.retain(|_, x| !x.is_null() && !x.as_array().is_some_and(|a| a.is_empty()));
    for list in obj.values_mut().filter_map(Value::as_array_mut) {
        for entry in list.iter_mut().filter_map(Value::as_object_mut) {
            entry.retain(|_, x| !x.is_null());
        }
    }
    if obj.get("used_in").is_none() && obj.get("opened_as").is_none() {
        obj.insert(
            "hint".into(),
            json!("No other composable shows it and no screen opens it: likely unused, or shown from code codemap does not follow (a ComposeView, reflection)."),
        );
    }
    Ok(Some(v))
}

/// For `project_overview`: modules, the launcher activity, counts. Null when
/// there is no Kotlin or Java code.
pub fn overview(store: &Store) -> Result<Value> {
    let files = load_files(store.conn())?;
    let code: Vec<_> = files
        .iter()
        .filter(|f| f.path.ends_with(".kt") || f.path.ends_with(".java"))
        .collect();
    if code.is_empty() {
        return Ok(Value::Null);
    }
    let kotlin = code.iter().filter(|f| f.path.ends_with(".kt")).count();
    let mut by_kind: std::collections::BTreeMap<&str, usize> = Default::default();
    let mut packages: std::collections::BTreeMap<&str, usize> = Default::default();
    for f in &code {
        for t in &f.facts.types {
            *by_kind.entry(t.kind.as_str()).or_default() += 1;
        }
        if let Some(p) = &f.facts.package {
            *packages.entry(p.as_str()).or_default() += 1;
        }
    }
    let mut components = Vec::new();
    for f in &files {
        for c in &f.facts.manifest {
            let class = manifest_class(&files, &c.name);
            components.push(json!({
                "kind": c.kind,
                "name": class.map(|(t, _)| t.fqn.clone()).unwrap_or_else(|| c.name.clone()),
                "launcher": c.launcher.then_some(true),
                "file": class.map(|(_, p)| p.to_owned()),
            }));
        }
    }
    for c in components.iter_mut().filter_map(Value::as_object_mut) {
        c.retain(|_, x| !x.is_null());
    }
    // Modules: the folder above `src/`.
    let mut modules: std::collections::BTreeMap<String, usize> = Default::default();
    for f in &code {
        let module = f
            .path
            .split("/src/")
            .next()
            .filter(|m| *m != f.path)
            .unwrap_or(".")
            .to_owned();
        *modules.entry(module).or_default() += 1;
    }
    let screens: i64 = store.conn().query_row(
        "SELECT COUNT(*) FROM pages WHERE source = ?1",
        [SOURCE],
        |r| r.get(0),
    )?;
    let tests = code
        .iter()
        .filter(|f| f.path.contains("/src/test/") || f.path.contains("/src/androidTest/"))
        .count();
    Ok(json!({
        "kotlin_files": kotlin,
        "java_files": code.len() - kotlin,
        "test_files": tests,
        "modules": modules,
        "packages": packages,
        "types": by_kind,
        "composables": composables(&files).len(),
        "screens": screens,
        "manifest": components,
    }))
}
