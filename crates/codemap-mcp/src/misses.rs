//! The AI mistakes log: every time the AI (through the MCP server) asks for
//! something that does not exist — a class, method, column, route, component,
//! translation key or endpoint — it is written to `misses.jsonl` next to the
//! app's index. Ranked, it shows the names the AI keeps getting wrong, which
//! usually point at code that is confusingly named, or at assumptions about
//! features that are not built.
//!
//! A lookup that comes back empty is often fine (checking that nothing exists
//! before creating it). So when the log is read, every entry is asked again:
//! anything that exists now is left out.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;

use codemap_core::store::Store;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::tools::{Freshness, Project};

const FILE: &str = "misses.jsonl";

/// One wrong guess.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Miss {
    pub at: String,
    pub tool: String,
    /// `class`, `column`, `symbol`, `component`, `table`, `endpoint`, …
    pub kind: String,
    pub item: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// The call that asks again whether it exists.
    pub recheck: Value,
}

pub fn log_path(project: &Project) -> PathBuf {
    project.index_path.with_file_name(FILE)
}

/// A suggestion as text: `"App\\Models\\Mall"` or `{ fqn | name }`.
fn suggestion(v: &Value) -> Option<String> {
    let first = v.as_array()?.first()?;
    first
        .as_str()
        .map(str::to_owned)
        .or_else(|| first["fqn"].as_str().map(str::to_owned))
        .or_else(|| first["name"].as_str().map(str::to_owned))
        .or_else(|| first["table"].as_str().map(str::to_owned))
        .or_else(|| first["key"].as_str().map(str::to_owned))
        .or_else(|| first["route"].as_str().map(str::to_owned))
}

/// The `verify` claim list an item kind belongs to.
fn claim_field(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "class" => "classes",
        "method" => "methods",
        "function" => "functions",
        "route" => "routes",
        "table" => "tables",
        "column" => "columns",
        "relation" => "relations",
        "component" => "components",
        "endpoint" => "endpoints",
        "i18n_key" => "i18n_keys",
        _ => return None,
    })
}

/// What the call asked for, by tool.
fn asked(tool: &str, args: &Value) -> Option<(&'static str, String)> {
    let s = |k: &str| args[k].as_str().map(str::to_owned);
    Some(match tool {
        "search_symbol" => ("symbol", s("query")?),
        "get_symbol" => ("symbol", s("fqn").or_else(|| s("name"))?),
        "get_component" => ("component", s("name").or_else(|| s("file"))?),
        "get_schema" => ("table", s("table")?),
        "find_usages" => ("name", s("target")?),
        "api_usage" => ("endpoint", s("endpoint")?),
        _ => return None,
    })
}

/// The wrong guesses in one answer.
pub fn found_in(tool: &str, args: &Value, data: &Value) -> Vec<Miss> {
    let now = jiff::Timestamp::now().to_string();
    let app_arg = args["app"].as_str().map(str::to_owned);
    let mut out = Vec::new();
    if tool == "verify" {
        if args["release_check"] == true {
            return out;
        }
        let snippet = args["php_snippet"].as_str().is_some_and(|s| !s.is_empty());
        for r in data["results"].as_array().into_iter().flatten() {
            if r["status"] != "not_found" {
                continue;
            }
            let (Some(kind), Some(item)) = (r["kind"].as_str(), r["item"].as_str()) else {
                continue;
            };
            let Some(field) = claim_field(kind) else {
                continue;
            };
            // Only names the AI wrote: listed in its claims, or in its snippet
            // (`i18n_keys: ['*']` lists the project's own gaps).
            let claimed = args["claims"][field]
                .as_array()
                .is_some_and(|l| l.iter().any(|x| x == item));
            if !claimed && !(snippet && kind != "i18n_key") {
                continue;
            }
            let app = r["app"].as_str().map(str::to_owned).or(app_arg.clone());
            let mut recheck = json!({ "claims": { field: [item] } });
            if let Some(a) = &app {
                recheck["app"] = json!(a);
            }
            out.push(Miss {
                at: now.clone(),
                tool: tool.into(),
                kind: kind.into(),
                item: item.into(),
                closest: suggestion(&r["suggestions"]),
                app,
                recheck: json!({ "tool": "verify", "args": recheck }),
            });
        }
        return out;
    }
    if data["found"] != false {
        return out;
    }
    let Some((kind, item)) = asked(tool, args) else {
        return out;
    };
    let app = data["app"].as_str().map(str::to_owned).or(app_arg);
    out.push(Miss {
        at: now,
        tool: tool.into(),
        kind: kind.into(),
        item,
        closest: suggestion(&data["suggestions"]),
        app,
        recheck: json!({ "tool": tool, "args": args }),
    });
    out
}

/// Appends the wrong guesses in an answer to the log of the app they
/// belong to (the first app when not known). Failures only warn: the log
/// must never break a tool call.
pub fn record(projects: &[&Project], tool: &str, args: &Value, data: &Value) {
    let found = found_in(tool, args, data);
    if found.is_empty() || projects.is_empty() {
        return;
    }
    for m in found {
        let project = m
            .app
            .as_deref()
            .and_then(|a| projects.iter().find(|p| p.app.as_deref() == Some(a)))
            .unwrap_or(&projects[0]);
        let path = log_path(project);
        let line = match serde_json::to_string(&m) {
            Ok(l) => l,
            Err(_) => continue,
        };
        let res = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| writeln!(f, "{line}"));
        if let Err(e) = res {
            tracing::warn!(path = %path.display(), "cannot write the AI mistakes log: {e}");
        }
    }
}

pub fn load(project: &Project) -> Vec<Miss> {
    let Ok(text) = std::fs::read_to_string(log_path(project)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

pub fn clear(project: &Project) -> std::io::Result<()> {
    match std::fs::remove_file(log_path(project)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// What to do about a name the AI keeps getting wrong.
fn idea(kind: &str, item: &str, closest: Option<&str>, times: usize) -> String {
    let short = |s: &str| {
        s.rsplit(['\\', '.', '/', ':'])
            .next()
            .unwrap_or(s)
            .to_owned()
    };
    match closest {
        Some(c) if c.eq_ignore_ascii_case(item) => {
            format!("Only the letter case differs: the real name is `{c}`.")
        }
        Some(c) if times >= 3 => format!(
            "The AI expects `{item}`, but the code has `{c}`. A name the AI keeps guessing wrong is often unclear to people too: consider renaming `{}` to what it does, or add a line to CLAUDE.md.",
            short(c)
        ),
        Some(c) => format!("The AI expected `{item}`; the closest real {kind} is `{c}`."),
        None => format!(
            "Nothing like `{item}` exists. The AI may assume a {kind} that was never built, or one that is named very differently."
        ),
    }
}

/// Wrong guesses grouped by name, most frequent first. `still_missing`
/// re-asks each one (with its own call), so names created since drop out.
pub fn ranked(misses: &[Miss], still_missing: &dyn Fn(&Miss) -> bool, limit: usize) -> Vec<Value> {
    struct Group<'a> {
        first: &'a Miss,
        times: usize,
        last: &'a str,
        closest: BTreeMap<&'a str, usize>,
        tools: Vec<&'a str>,
    }
    let mut groups: BTreeMap<(String, String, String), Group> = BTreeMap::new();
    for m in misses {
        let key = (
            m.app.clone().unwrap_or_default(),
            m.kind.clone(),
            m.item.to_lowercase(),
        );
        let g = groups.entry(key).or_insert(Group {
            first: m,
            times: 0,
            last: &m.at,
            closest: BTreeMap::new(),
            tools: Vec::new(),
        });
        g.times += 1;
        if m.at.as_str() > g.last {
            g.last = &m.at;
        }
        if let Some(c) = &m.closest {
            *g.closest.entry(c).or_default() += 1;
        }
        if !g.tools.contains(&m.tool.as_str()) {
            g.tools.push(&m.tool);
        }
    }
    let mut list: Vec<Group> = groups.into_values().collect();
    list.sort_by(|a, b| b.times.cmp(&a.times).then_with(|| b.last.cmp(a.last)));
    let mut out = Vec::new();
    for g in list {
        if out.len() >= limit {
            break;
        }
        if !still_missing(g.first) {
            continue;
        }
        let closest = g
            .closest
            .iter()
            .max_by_key(|(_, n)| **n)
            .map(|(c, _)| (*c).to_owned());
        out.push(json!({
            "kind": g.first.kind,
            "item": g.first.item,
            "app": g.first.app,
            "times": g.times,
            "first_seen": g.first.at,
            "last_seen": g.last,
            "closest": closest,
            "asked_with": g.tools,
            "idea": idea(&g.first.kind, &g.first.item, closest.as_deref(), g.times),
        }));
    }
    out
}

/// Re-asks a logged call against the current index.
pub fn still_missing(apps: &[(&Store, &Project)], m: &Miss) -> bool {
    let (Some(tool), args) = (m.recheck["tool"].as_str(), m.recheck["args"].clone()) else {
        return true;
    };
    match crate::workspace::call(apps, tool, args.clone(), Freshness::default()) {
        Ok(env) => found_in(tool, &args, &env.data)
            .iter()
            .any(|again| again.item.eq_ignore_ascii_case(&m.item)),
        Err(_) => true,
    }
}

/// Every app's log, ranked, for the code map and `codemap misses`.
pub fn report(apps: &[(&Store, &Project)], limit: usize) -> Vec<Value> {
    let mut all = Vec::new();
    for (_, p) in apps {
        all.extend(load(p));
    }
    ranked(&all, &|m| still_missing(apps, m), limit)
}

/// For `project_overview`: names the AI got wrong more than once, so it
/// can avoid them this time.
pub fn often_mistaken(store: &Store, project: &Project) -> Vec<String> {
    let misses = load(project);
    if misses.is_empty() {
        return Vec::new();
    }
    let apps = [(store, project)];
    ranked(&misses, &|m| still_missing(&apps, m), 10)
        .into_iter()
        .filter(|g| g["times"].as_u64().unwrap_or(0) >= 2)
        .map(|g| {
            let item = g["item"].as_str().unwrap_or_default();
            let kind = g["kind"].as_str().unwrap_or_default();
            match g["closest"].as_str() {
                Some(c) => format!("{kind} `{item}` does not exist; it is `{c}`"),
                None => format!("{kind} `{item}` does not exist"),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_names_the_ai_wrote_are_recorded() {
        let args = json!({ "claims": { "columns": ["orders.order_status"], "i18n_keys": ["*"] } });
        let data = json!({ "results": [
            { "kind": "column", "item": "orders.order_status", "status": "not_found", "suggestions": ["orders.status"] },
            { "kind": "i18n_key", "item": "orders.empty", "status": "not_found" },
            { "kind": "table", "item": "orders", "status": "exists" },
        ]});
        let m = found_in("verify", &args, &data);
        assert_eq!(m.len(), 1);
        assert_eq!(
            (
                m[0].kind.as_str(),
                m[0].item.as_str(),
                m[0].closest.as_deref()
            ),
            ("column", "orders.order_status", Some("orders.status"))
        );
        assert_eq!(
            m[0].recheck["args"]["claims"]["columns"][0],
            "orders.order_status"
        );
        // A release check lists the project's problems, not the AI's guesses.
        assert!(found_in("verify", &json!({ "release_check": true }), &data).is_empty());

        let m = found_in(
            "search_symbol",
            &json!({ "query": "MallControler" }),
            &json!({ "found": false, "suggestions": [{ "fqn": "App\\Http\\Controllers\\MallController" }] }),
        );
        assert_eq!(
            m[0].closest.as_deref(),
            Some("App\\Http\\Controllers\\MallController")
        );
        assert!(
            found_in(
                "search_symbol",
                &json!({ "query": "Mall" }),
                &json!({ "found": true })
            )
            .is_empty()
        );
    }

    #[test]
    fn ranked_by_times_and_rechecked() {
        let miss = |item: &str, at: &str| Miss {
            at: at.into(),
            tool: "verify".into(),
            kind: "column".into(),
            item: item.into(),
            closest: Some("orders.status".into()),
            app: None,
            recheck: Value::Null,
        };
        let log = [
            miss("orders.order_status", "2026-10-01T00:00:00Z"),
            miss("orders.order_status", "2026-10-02T00:00:00Z"),
            miss("Orders.Order_Status", "2026-10-03T00:00:00Z"),
            miss("orders.made_later", "2026-10-04T00:00:00Z"),
        ];
        let r = ranked(&log, &|m| m.item != "orders.made_later", 10);
        assert_eq!(r.len(), 1, "created since: left out");
        assert_eq!(r[0]["times"], 3);
        assert_eq!(r[0]["last_seen"], "2026-10-03T00:00:00Z");
        assert!(
            r[0]["idea"]
                .as_str()
                .unwrap()
                .contains("consider renaming `status`")
        );
    }
}
