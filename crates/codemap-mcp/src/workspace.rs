//! Workspaces: one folder holding several apps (a Laravel API next to Vue /
//! Nuxt frontends). Each app keeps its own index; frontends are linked to the
//! workspace's Laravel apps; tool calls run on every app (or the one named in
//! `app`) and the answers are merged.

use std::path::{Path, PathBuf};

use codemap_core::config::LinkConfig;
use codemap_core::envelope::Envelope;
use codemap_core::{Config, Store, paths};
use serde_json::{Map, Value, json};

use crate::tools::{self, Freshness, Project, ToolError};

const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "vendor",
    "dist",
    "storage",
    "public",
    "build",
];

fn is_laravel(dir: &Path) -> bool {
    dir.join("artisan").is_file() && dir.join("composer.json").is_file()
}

fn is_frontend(dir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join("package.json")) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    ["dependencies", "devDependencies"].iter().any(|k| {
        ["vue", "nuxt", "next", "react"]
            .iter()
            .any(|f| v[*k].get(*f).is_some())
    })
}

/// A Laravel or Vue / Nuxt app lives here.
pub fn is_project(dir: &Path) -> bool {
    is_laravel(dir) || is_frontend(dir)
}

/// The apps of a workspace root, or `None` when the root is a single project.
pub fn discover(root: &Path, config: &Config) -> Option<Vec<(String, PathBuf)>> {
    if !config.workspace.apps.is_empty() {
        let apps: Vec<(String, PathBuf)> = config
            .workspace
            .apps
            .iter()
            .map(|a| a.trim_matches('/'))
            .filter(|a| !a.is_empty())
            .map(|a| (a.replace('/', "-"), root.join(a)))
            .filter(|(_, p)| p.is_dir())
            .collect();
        return (!apps.is_empty()).then_some(apps);
    }
    if is_project(root) {
        return None;
    }
    let mut apps: Vec<(String, PathBuf)> = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (!name.starts_with('.') && !SKIP_DIRS.contains(&name.as_str()) && is_project(&e.path()))
                .then(|| (name, e.path()))
        })
        .collect();
    apps.sort();
    (!apps.is_empty()).then_some(apps)
}

/// The projects to index: the root itself, or one per workspace app.
/// `base_id` names the index (per app: `<base_id>-<app>`).
pub fn projects(
    root: &Path,
    config: Config,
    data_dir: &Path,
    base_id: &str,
) -> codemap_core::Result<Vec<Project>> {
    let Some(apps) = discover(root, &config) else {
        return Ok(vec![Project {
            root: root.to_path_buf(),
            index_path: paths::index_path(data_dir, base_id),
            id: base_id.to_owned(),
            config,
            app: None,
        }]);
    };
    let workspace_name = config
        .project
        .name
        .clone()
        .or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "workspace".into());
    let mut out: Vec<Project> = Vec::new();
    for (name, app_root) in &apps {
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let id = format!("{base_id}-{safe}");
        paths::validate_project_id(&id)?;
        let mut app_config = Config::load(app_root)?;
        if app_config.project.name.is_none() {
            app_config.project.name = Some(format!("{workspace_name}/{name}"));
        }
        // Shipped locales and links given for the workspace apply to every app.
        if app_config.i18n.locales.is_empty() {
            app_config.i18n.locales = config.i18n.locales.clone();
        }
        for i in &config.release.ignore {
            if !app_config.release.ignore.contains(i) {
                app_config.release.ignore.push(i.clone());
            }
        }
        if app_config.copy.banned.is_empty() {
            app_config.copy = config.copy.clone();
        }
        if app_config.tests.is_empty()
            && let Some(t) = config.workspace.tests.get(name.as_str())
        {
            app_config.tests = t.clone();
        }
        for d in &config.databases {
            if d.app.as_deref().is_none_or(|a| a == name.as_str())
                && !app_config.databases.iter().any(|x| x.name == d.name)
            {
                app_config.databases.push(d.clone());
            }
        }
        for l in &config.links {
            if !app_config.links.iter().any(|x| x.name == l.name) {
                app_config.links.push(l.clone());
            }
        }
        out.push(Project {
            root: app_root.clone(),
            index_path: paths::index_path(data_dir, &id),
            id,
            config: app_config,
            app: Some(name.clone()),
        });
    }
    // Laravel apps first: frontends read their routes when matching API calls.
    out.sort_by_key(|p| !is_laravel(&p.root));
    // Frontends link to the workspace's Laravel apps, sharing their index.
    let backends: Vec<(String, PathBuf, PathBuf)> = out
        .iter()
        .filter(|p| is_laravel(&p.root))
        .map(|p| {
            (
                p.app.clone().unwrap_or_default(),
                p.root.clone(),
                p.index_path.clone(),
            )
        })
        .collect();
    for p in &mut out {
        if is_laravel(&p.root) || !is_frontend(&p.root) {
            continue;
        }
        let app_root = p.root.clone();
        for (name, root, index) in &backends {
            // An existing link to this app (e.g. `path = "../backend"`) shares its index.
            let mut linked = false;
            for l in &mut p.config.links {
                let target = if l.path.is_absolute() {
                    l.path.clone()
                } else {
                    app_root.join(&l.path)
                };
                if target.canonicalize().ok().as_ref() == root.canonicalize().ok().as_ref() {
                    l.index_path = Some(index.clone());
                    linked = true;
                }
            }
            if !linked && !p.config.links.iter().any(|l| l.name == *name) {
                p.config.links.push(LinkConfig {
                    name: name.clone(),
                    path: root.clone(),
                    kind: Some("laravel".into()),
                    api_base: None,
                    index_path: Some(index.clone()),
                });
            }
        }
    }
    Ok(out)
}

fn meaningful(tool: &str, data: &Value) -> bool {
    match tool {
        "api_usage" => {
            data["calls"].as_array().is_some_and(|a| !a.is_empty())
                || data["endpoints"].as_array().is_some_and(|a| !a.is_empty())
                || data["summary"].as_object().is_some_and(|o| !o.is_empty())
        }
        "list_pages" | "list_routes" => data["total"].as_u64().is_some_and(|n| n > 0),
        "get_schema" => {
            data["found"] != false && data["tables"].as_array().is_none_or(|t| !t.is_empty())
        }
        _ => data["found"] != false,
    }
}

const STATUS_RANK: &[&str] = &["exists", "external", "unresolved", "not_found"];

/// `verify` across apps: each item keeps its best status (exists > external >
/// unresolved > not_found), and says which app it was found in. An app that
/// has nothing of the kind indexed (no migrations for a column, no locale
/// files) cannot tell, so its "unresolved" ranks below another app's
/// definite "not_found".
fn merge_verify(results: Vec<(String, Value)>) -> Value {
    let mut items: Vec<(String, String, Value)> = Vec::new(); // (kind, item, result)
    for (app, data) in results {
        for r in data["results"].as_array().cloned().unwrap_or_default() {
            let key = (
                r["kind"].as_str().unwrap_or("").to_owned(),
                r["item"].as_str().unwrap_or("").to_owned(),
            );
            let mut r = r;
            if r["status"] != "not_found" {
                r["app"] = json!(app);
            }
            let rank = |v: &Value| {
                let blind = v["status"] == "unresolved"
                    && v["detail"].as_str().is_some_and(|d| d.contains(" indexed"));
                if blind {
                    return STATUS_RANK.len();
                }
                STATUS_RANK
                    .iter()
                    .position(|s| v["status"] == *s)
                    .unwrap_or(9)
            };
            match items
                .iter_mut()
                .find(|(k, i, _)| *k == key.0 && *i == key.1)
            {
                Some((_, _, existing)) if rank(&r) < rank(existing) => *existing = r,
                Some(_) => {}
                None => items.push((key.0, key.1, r)),
            }
        }
    }
    let count = |s: &str| items.iter().filter(|(_, _, r)| r["status"] == s).count();
    let (ok, missing, external, unresolved) = (
        count("exists"),
        count("not_found"),
        count("external"),
        count("unresolved"),
    );
    let mut summary = format!("{ok} ok, {missing} not_found");
    if external > 0 {
        summary.push_str(&format!(", {external} external"));
    }
    if unresolved > 0 {
        summary.push_str(&format!(", {unresolved} unresolved"));
    }
    json!({
        "summary": summary,
        "all_exist": missing == 0 && unresolved == 0,
        "results": items.into_iter().map(|(_, _, r)| r).collect::<Vec<_>>(),
    })
}

/// Runs a tool on every app (or the one in `args.app`) and merges the answers.
pub fn call(
    apps: &[(&Store, &Project)],
    tool: &str,
    mut args: Value,
    fresh: Freshness,
) -> Result<Envelope<Value>, ToolError> {
    let wanted = args
        .as_object_mut()
        .and_then(|o| o.remove("app"))
        .and_then(|v| v.as_str().map(str::to_owned));
    if apps.len() == 1 && apps[0].1.app.is_none() {
        let (s, p) = apps[0];
        return tools::call(s, p, tool, args, fresh);
    }
    let names: Vec<String> = apps
        .iter()
        .map(|(_, p)| p.app.clone().unwrap_or_default())
        .collect();
    let selected: Vec<&(&Store, &Project)> = match &wanted {
        Some(w) => {
            let hit: Vec<_> = apps
                .iter()
                .filter(|(_, p)| p.app.as_deref() == Some(w.as_str()))
                .collect();
            if hit.is_empty() {
                return Err(ToolError::InvalidParams(format!(
                    "unknown app `{w}`; apps: {}",
                    names.join(", ")
                )));
            }
            hit
        }
        None => apps.iter().collect(),
    };
    // `changed_files` are relative to the workspace: route each to its app.
    let changed: Option<Vec<String>> = args["changed_files"].as_array().map(|a| {
        a.iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect()
    });

    let mut results: Vec<(String, Envelope<Value>)> = Vec::new();
    for (store, project) in selected {
        let app = project.app.clone().unwrap_or_default();
        let mut a = args.clone();
        if let Some(files) = &changed {
            let prefix = format!("{app}/");
            let mine: Vec<String> = files
                .iter()
                .filter_map(|f| f.strip_prefix(&prefix).map(str::to_owned))
                .collect();
            if mine.is_empty() && wanted.is_none() {
                continue;
            }
            a["changed_files"] = json!(mine);
        }
        results.push((app, tools::call(store, project, tool, a, fresh.clone())?));
    }
    if results.is_empty() {
        return Err(ToolError::InvalidParams(format!(
            "changed_files must start with an app folder: {}",
            names
                .iter()
                .map(|n| format!("{n}/…"))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    // Change impact: frontend calls (in any app) to backend routes the change touches.
    let impact =
        tool == "find_usages" && (changed.is_some() || args["git_changes"].as_bool() == Some(true));
    let mut screens = Map::new();
    let mut browser = Map::new();
    if impact {
        let routes: Vec<String> = results
            .iter()
            .flat_map(|(_, e)| e.data["changed"].as_array().cloned().unwrap_or_default())
            .flat_map(|f| {
                let mut v = f["routes"].as_array().cloned().unwrap_or_default();
                v.extend(
                    f["routes_via_callers"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default(),
                );
                v
            })
            .filter_map(|r| {
                r.as_str().map(|s| {
                    let s = s.split(" → ").next().unwrap_or(s);
                    s.split(" (via ").next().unwrap_or(s).to_owned()
                })
            })
            .collect();
        if !routes.is_empty() {
            for (store, project) in apps {
                let callers = crate::api::callers_of_routes(store, project, &routes)?;
                // Browser tests that open the screens making those calls.
                let files: Vec<String> = callers
                    .iter()
                    .filter_map(|c| c.split(':').next().map(str::to_owned))
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                let b = crate::browser::for_files(store, project, &files)?;
                if !b.tests.is_empty() {
                    let specs: Vec<String> = b.tests.iter().map(|(t, _)| t.clone()).collect();
                    browser.insert(
                        project.app.clone().unwrap_or_default(),
                        json!({
                            "tests": b.tests.iter().take(40).map(|(t, why)| format!("{t}  ({why})")).collect::<Vec<_>>(),
                            "run": crate::usages::test_commands(project, &specs, &[]),
                        }),
                    );
                }
                if !callers.is_empty() {
                    let total = callers.len();
                    let mut shown: Vec<Value> =
                        callers.into_iter().take(30).map(Value::from).collect();
                    if total > 30 {
                        shown.push(json!(format!("… {} more", total - 30)));
                    }
                    screens.insert(project.app.clone().unwrap_or_default(), json!(shown));
                }
            }
        }
    }

    let mut meta = results[0].1.meta.clone();
    meta.reindexed_files = results.iter().map(|(_, e)| e.meta.reindexed_files).sum();
    meta.stale = results.iter().any(|(_, e)| e.meta.stale);
    meta.truncated = results.iter().any(|(_, e)| e.meta.truncated);
    meta.next_cursor = None;
    if let Some(ws) = meta.project.split('/').next().map(str::to_owned) {
        meta.project = ws;
    }

    let data = if tool == "verify" && args["release_check"] == true {
        // Per app, plus one verdict for the whole workspace.
        let clean = results.iter().all(|(_, e)| e.data["clean"] != false);
        let total: u64 = results
            .iter()
            .map(|(_, e)| e.data["problems_total"].as_u64().unwrap_or(0))
            .sum();
        let mut by_app = Map::new();
        for (app, e) in results.drain(..) {
            by_app.insert(app, e.data);
        }
        json!({ "clean": clean, "problems_total": total, "apps": by_app })
    } else if tool == "verify" {
        merge_verify(
            results
                .iter()
                .map(|(a, e)| (a.clone(), e.data.clone()))
                .collect(),
        )
    } else if tool == "project_overview" || results.len() == 1 {
        if results.len() == 1 {
            let (app, e) = results.remove(0);
            meta = e.meta;
            let mut d = e.data;
            if let Some(o) = d.as_object_mut() {
                o.insert("app".into(), json!(app));
            }
            d
        } else {
            // Several apps: a short summary each; `app` gives the full overview.
            let full = args["detail"] == "full";
            let mut by_app = Map::new();
            for (app, e) in results {
                let mut d = e.data;
                if !full && let Some(o) = d.as_object_mut() {
                    for k in [
                        "modules",
                        "models",
                        "key_dirs",
                        "psr4",
                        "route_files",
                        "next",
                        "detection_reasons",
                        "name",
                    ] {
                        o.remove(k);
                    }
                }
                by_app.insert(app, d);
            }
            json!({
                "apps": by_app,
                "hint": "This is a workspace: pass `app` to any tool to look at one app (project_overview with `app` gives its models, modules and key directories).",
            })
        }
    } else {
        let hits: Vec<usize> = results
            .iter()
            .enumerate()
            .filter(|(_, (_, e))| meaningful(tool, &e.data))
            .map(|(i, _)| i)
            .collect();
        match hits.as_slice() {
            [one] => {
                let (app, e) = results.swap_remove(*one);
                meta.next_cursor = e.meta.next_cursor.clone();
                meta.truncated = e.meta.truncated;
                let mut d = e.data;
                if let Some(o) = d.as_object_mut() {
                    o.insert("app".into(), json!(app));
                }
                d
            }
            [] => {
                // Nothing anywhere: the first answer (with its suggestions).
                let (_, e) = results.remove(0);
                let mut d = e.data;
                d["searched_apps"] = json!(names);
                d
            }
            many => {
                let mut by_app = Map::new();
                for i in many.iter().rev() {
                    let (app, e) = results.swap_remove(*i);
                    by_app.insert(app, e.data);
                }
                json!({ "apps": by_app })
            }
        }
    };
    let mut data = data;
    if !screens.is_empty() {
        data["frontend_calls_to_changed_routes"] = Value::Object(screens);
    }
    if !browser.is_empty() {
        data["browser_tests_for_changed_routes"] = Value::Object(browser);
    }
    Ok(Envelope::new(data, meta))
}
