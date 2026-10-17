//! One SwiftUI view for `get_component`: what it takes, its own state, what it
//! reads from the environment, which views it shows, and where it is shown.

use codemap_core::{Result, Store};
use rusqlite::params;
use serde_json::{Value, json};

use crate::extract::{Prop, TypeFacts};
use crate::index::{PAGES_SOURCE, SwiftFile, load_files, views};

/// Wrappers whose value the caller passes in.
const PASSED: &[&str] = &["Binding", "ObservedObject", "Bindable"];
/// Wrappers that read from the environment.
const ENVIRONMENT: &[&str] = &["Environment", "EnvironmentObject"];

/// `None` when no Swift view matches `query` (a name, `Outer.Inner`, or a file).
pub fn get_view(store: &Store, query: &str) -> Result<Option<Value>> {
    let has: bool =
        store
            .conn()
            .query_row("SELECT EXISTS (SELECT 1 FROM swift_files)", [], |r| {
                r.get(0)
            })?;
    if !has {
        return Ok(None);
    }
    let files = load_files(store.conn())?;
    let views = views(&files);
    let q = query.trim();
    let name = if views.contains_key(q) {
        q.to_owned()
    } else if let Some((n, _)) = views
        .iter()
        .filter(|(_, (file, _))| file == q)
        .min_by_key(|(_, (_, line))| *line)
    {
        n.clone()
    } else if let Some(n) = views
        .keys()
        .find(|n| n.rsplit('.').next() == Some(q) || n.eq_ignore_ascii_case(q))
    {
        n.clone()
    } else {
        return Ok(None);
    };
    let (file, line) = views[&name].clone();
    let Some(t) = type_facts(&files, &name, &file) else {
        return Ok(None);
    };
    // Members declared in extensions count too.
    let mut props: Vec<&Prop> = t.props.iter().collect();
    for f in &files {
        for e in f
            .facts
            .types
            .iter()
            .filter(|x| x.kind == "extension" && x.name == name)
        {
            props.extend(e.props.iter());
        }
    }
    fn stored(p: &&&Prop) -> bool {
        !p.computed && !p.is_static
    }
    let inputs: Vec<Value> = props
        .iter()
        .filter(stored)
        .filter(|p| match &p.wrapper {
            None => true,
            Some(w) => PASSED.contains(&w.as_str()),
        })
        // `let x = 1` can't be passed in.
        .filter(|p| !(p.is_let && p.default.is_some()))
        .map(|p| {
            json!({
                "name": p.name,
                "type": p.r#type,
                "wrapper": p.wrapper.as_ref().map(|w| format!("@{w}")),
                "required": p.default.is_none() && !p.r#type.as_deref().is_some_and(|t| t.ends_with('?')),
                "default": p.default,
                "line": p.line,
            })
        })
        .collect();
    let state: Vec<Value> = props
        .iter()
        .filter(stored)
        .filter_map(|p| {
            let w = p.wrapper.as_deref()?;
            (!PASSED.contains(&w) && !ENVIRONMENT.contains(&w)).then(|| {
                json!({
                    "name": p.name,
                    "wrapper": format!("@{w}"),
                    "type": p.r#type,
                    "initial": p.default,
                    "key": p.wrapper_arg,
                    "line": p.line,
                })
            })
        })
        .collect();
    let environment: Vec<Value> = props
        .iter()
        .filter_map(|p| {
            let w = p.wrapper.as_deref()?;
            ENVIRONMENT.contains(&w).then(|| {
                json!({
                    "name": p.name,
                    "wrapper": format!("@{w}"),
                    "type": p.r#type,
                    "key": p.wrapper_arg,
                    "line": p.line,
                })
            })
        })
        .collect();
    let short = |n: &str| n.rsplit('.').next().unwrap_or(n).to_owned();
    let resolve = |call: &str| -> Option<String> {
        if views.contains_key(call) {
            return Some(call.to_owned());
        }
        let nested = format!("{name}.{call}");
        if views.contains_key(&nested) {
            return Some(nested);
        }
        views.keys().find(|v| short(v) == call).cloned()
    };
    let renders: Vec<Value> = t
        .calls
        .iter()
        .filter_map(|c| {
            let v = resolve(&c.name)?;
            (v != name).then(|| json!({"view": v, "file": views[&v].0, "line": c.line}))
        })
        .collect();
    let mut used_in = Vec::new();
    for f in &files {
        for other in f.facts.types.iter().filter(|x| x.name != name) {
            if let Some(c) = other
                .calls
                .iter()
                .find(|c| resolve(&c.name).as_deref() == Some(name.as_str()))
            {
                used_in.push(json!({"in": other.name, "at": format!("{}:{}", f.path, c.line)}));
            }
        }
    }
    let mut opened_as = Vec::new();
    {
        let mut stmt = store.conn().prepare_cached(
            "SELECT route_path, defined_in, parent, meta FROM pages WHERE source = ?1 AND name = ?2 ORDER BY defined_in",
        )?;
        let rows = stmt.query_map(params![PAGES_SOURCE, name], |r| {
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
                "title": meta["title"],
                "from": from,
                "at": at,
            }));
        }
    }
    let mut v = json!({
        "found": true,
        "name": name,
        "kind": "swiftui_view",
        "file": file,
        "line": line,
        "end_line": t.end_line,
        "conforms": t.conforms,
        "inputs": inputs,
        "state": state,
        "environment": environment,
        "renders": renders,
        "used_in": used_in,
        "opened_as": opened_as,
    });
    if !t.attributes.is_empty() {
        v["attributes"] = json!(t.attributes);
    }
    let obj = v.as_object_mut().expect("object");
    obj.retain(|_, x| !(x.is_array() && x.as_array().is_some_and(|a| a.is_empty())));
    // Leave out unknown fields of each entry (`"type": null`).
    for list in obj.values_mut().filter_map(Value::as_array_mut) {
        for entry in list.iter_mut().filter_map(Value::as_object_mut) {
            entry.retain(|_, x| !x.is_null());
        }
    }
    if obj.get("used_in").is_none() && obj.get("opened_as").is_none() {
        obj.insert(
            "hint".into(),
            json!("No other view shows it and no screen opens it: likely unused, or shown from code codemap does not follow (UIKit, a string name)."),
        );
    }
    Ok(Some(v))
}

fn type_facts<'a>(files: &'a [SwiftFile], name: &str, file: &str) -> Option<&'a TypeFacts> {
    files
        .iter()
        .find(|f| f.path == file)?
        .facts
        .types
        .iter()
        .find(|t| t.name == name && t.kind != "extension")
}

/// For `project_overview`: what kind of Swift project, its app entry points,
/// and counts. Null when there is no Swift code.
pub fn overview(store: &Store, root: &std::path::Path) -> Result<Value> {
    let files = load_files(store.conn())?;
    if !files.iter().any(|f| f.path.ends_with(".swift")) {
        return Ok(Value::Null);
    }
    let views = views(&files);
    let mut apps = Vec::new();
    let mut by_kind: std::collections::BTreeMap<&str, usize> = Default::default();
    let mut frameworks: std::collections::BTreeMap<&str, usize> = Default::default();
    for f in &files {
        for t in &f.facts.types {
            if t.kind != "extension" {
                *by_kind.entry(t.kind.as_str()).or_default() += 1;
            }
            let app =
                t.attributes.iter().any(|a| a == "main") || t.conforms.iter().any(|c| c == "App");
            if app && t.kind != "extension" {
                apps.push(json!({"name": t.name, "at": format!("{}:{}", f.path, t.line), "conforms": t.conforms}));
            }
        }
        for i in &f.facts.imports {
            *frameworks.entry(i.as_str()).or_default() += 1;
        }
    }
    let mut frameworks: Vec<(&str, usize)> = frameworks.into_iter().collect();
    frameworks.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let screens: i64 = store.conn().query_row(
        "SELECT COUNT(*) FROM pages WHERE source = ?1",
        [PAGES_SOURCE],
        |r| r.get(0),
    )?;
    let projects: Vec<String> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            n == "Package.swift" || n.ends_with(".xcodeproj") || n.ends_with(".xcworkspace")
        })
        .collect();
    let mut dirs: std::collections::BTreeMap<String, usize> = Default::default();
    for f in files.iter().filter(|f| f.path.ends_with(".swift")) {
        let mut parts = f.path.split('/');
        let first = parts.next().unwrap_or("");
        let dir = match (first, parts.next()) {
            // `Sources/App/…` → `Sources/App`
            ("Sources" | "Tests", Some(second)) if f.path.matches('/').count() > 1 => {
                format!("{first}/{second}")
            }
            (_, Some(_)) => first.to_owned(),
            _ => ".".to_owned(),
        };
        *dirs.entry(dir).or_default() += 1;
    }
    Ok(json!({
        "project": projects,
        "apps": apps,
        "swift_files": files.iter().filter(|f| f.path.ends_with(".swift")).count(),
        "types": by_kind,
        "views": views.len(),
        "screens": screens,
        "imports": frameworks.iter().take(12).map(|(n, c)| format!("{n} ({c})")).collect::<Vec<_>>(),
        "dirs": dirs,
    }))
}
