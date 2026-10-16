//! The code map: one self-contained HTML page with every screen, the API
//! calls it makes, the endpoints they reach, the controllers behind them, the
//! models those use and the tables they are stored in, plus the problems the
//! release check finds. Built from the index only; nothing is sent anywhere.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use codemap_core::store::Store;
use codemap_vue::api::CallRow;
use serde_json::{Value, json};

use crate::api::{Matcher, Outcome};
use crate::tools::{Project, ToolError};

type R<T> = Result<T, ToolError>;

const PAGE: &str = include_str!("map.html");

/// How far below a screen's file calls are looked for (components,
/// composables, stores).
const DEPTH: usize = 4;

#[derive(Default)]
struct Graph {
    nodes: BTreeMap<String, Value>,
    edges: Vec<Value>,
    /// `(from, to, kind)` → index in `edges`.
    seen: HashMap<(String, String, String), usize>,
}

impl Graph {
    /// Adds a node; a stub (an endpoint seen only from a frontend) is
    /// replaced when the backend itself is mapped.
    fn node(&mut self, id: &str, mut v: Value) {
        v["id"] = json!(id);
        match self.nodes.get(id) {
            Some(old) if old["stub"] != true || v["stub"] == true => {}
            _ => {
                self.nodes.insert(id.to_owned(), v);
            }
        }
    }

    fn info(&mut self, id: &str, key: &str, value: Value) {
        if let Some(n) = self.nodes.get_mut(id) {
            n["info"][key] = value;
        }
    }

    /// One link per pair and kind; further places are counted in `more`.
    fn edge(&mut self, from: &str, to: &str, kind: &str, label: &str) {
        let key = (from.to_owned(), to.to_owned(), kind.to_owned());
        if let Some(&i) = self.seen.get(&key) {
            let e = &mut self.edges[i];
            if e["label"] != label {
                e["more"] = json!(e["more"].as_u64().unwrap_or(0) + 1);
            }
            return;
        }
        self.seen.insert(key, self.edges.len());
        self.edges
            .push(json!({ "from": from, "to": to, "kind": kind, "label": label }));
    }
}

fn short(fqn: &str) -> &str {
    fqn.rsplit('\\').next().unwrap_or(fqn)
}

fn root_key(p: &std::path::Path) -> String {
    p.to_string_lossy().trim_end_matches('/').to_owned()
}

/// A file's name, with its folder when the name alone says little
/// (`rooms/[slug]/page.tsx`, `api/hit/route.ts`).
fn file_label(path: &str) -> String {
    let segs: Vec<&str> = path.split('/').collect();
    let name = segs.last().copied().unwrap_or(path);
    let stem = name.split('.').next().unwrap_or(name);
    let generic = [
        "page", "route", "layout", "index", "default", "loading", "error", "actions", "+page",
    ];
    if generic.contains(&stem) && segs.len() >= 2 {
        let take = if segs.len() >= 3 && segs[segs.len() - 2].starts_with(['[', '(']) {
            3
        } else {
            2
        };
        segs[segs.len() - take..].join("/")
    } else {
        name.to_owned()
    }
}

fn route_id(root: &str, methods: &str, uri: &str) -> String {
    format!("route:{root}|{methods} {uri}")
}

/// Every app's part of the map, as data for the page.
pub fn data(apps: &[(&Store, &Project)], title: &str) -> R<Value> {
    let mut g = Graph::default();
    let mut app_list = Vec::new();
    let mut problems = serde_json::Map::new();
    for (store, project) in apps {
        let app = project.app.clone().unwrap_or_else(|| project.name(store));
        let root = root_key(&project.root);
        let kinds = backend(&mut g, store, &app, &root)?;
        let screens = frontend(&mut g, store, project, &app, &root)?;
        app_list.push(json!({ "name": app, "backend": kinds, "screens": screens }));
        match crate::release::release_check(store, project, None) {
            Ok(r) => {
                problems.insert(
                    app.clone(),
                    json!({
                        "problems": r["problems"],
                        "low_priority": r.get("low_priority").cloned().unwrap_or(json!([])),
                        "ignored": r.get("ignored").cloned().unwrap_or(json!([])),
                    }),
                );
            }
            Err(e) => {
                problems.insert(app.clone(), json!({ "error": e.to_string() }));
            }
        }
    }
    // Backends linked to a frontend (`--link`, `[[links]]`) but not mapped
    // as apps of their own: their endpoints, controllers, models and tables.
    let mut mapped: HashSet<String> = apps.iter().map(|(_, p)| root_key(&p.root)).collect();
    for (store, project) in apps {
        let (linked, _) = crate::api::backends(store, project)?;
        for b in &linked {
            let root = root_key(&b.root);
            let Some(linked_store) = &b.store else {
                continue;
            };
            if !mapped.insert(root.clone()) {
                continue;
            }
            let kinds = backend(&mut g, linked_store, &b.name, &root)?;
            app_list
                .push(json!({ "name": b.name, "backend": kinds, "screens": 0, "linked": true }));
        }
    }
    // Problems that name a file flag every node in that file.
    let mut flagged: HashMap<(String, String), Vec<String>> = HashMap::new();
    for (app, v) in &problems {
        for p in v["problems"].as_array().into_iter().flatten() {
            let Some(text) = p.as_str() else { continue };
            for tok in text.split(|c: char| c.is_whitespace() || "(),\"`".contains(c)) {
                let file = tok.split(':').next().unwrap_or(tok);
                if file.contains('/') && file.contains('.') {
                    flagged
                        .entry((app.clone(), file.to_owned()))
                        .or_default()
                        .push(text.to_owned());
                }
            }
        }
    }
    for n in g.nodes.values_mut() {
        let (Some(app), Some(file)) = (n["app"].as_str(), n["file"].as_str()) else {
            continue;
        };
        if let Some(list) = flagged.get(&(app.to_owned(), file.to_owned())) {
            let mut list = list.clone();
            list.dedup();
            n["problems"] = json!(list);
        }
    }
    // The AI's wrong guesses, and the real items they were closest to.
    let mistakes = crate::misses::report(apps, 300);
    for m in &mistakes {
        let Some(closest) = m["closest"].as_str() else {
            continue;
        };
        let wanted = closest.split("::").next().unwrap_or(closest);
        let column = closest.split_once('.').map(|(t, _)| t);
        for n in g.nodes.values_mut() {
            let hit = n["sub"] == wanted
                || n["label"] == wanted
                || (n["kind"] == "table" && column.is_some_and(|t| n["label"] == t));
            let same_app = m["app"].is_null() || m["app"] == n["app"];
            if hit && same_app {
                let line = format!(
                    "{}× asked for `{}`",
                    m["times"],
                    m["item"].as_str().unwrap_or_default()
                );
                match n["mistaken_for"].as_array_mut() {
                    Some(list) => list.push(json!(line)),
                    None => n["mistaken_for"] = json!([line]),
                }
            }
        }
    }
    Ok(json!({
        "mistakes": mistakes,
        "title": title,
        "generated_at": jiff::Timestamp::now().to_string(),
        "apps": app_list,
        "nodes": g.nodes.into_values().collect::<Vec<_>>(),
        "edges": g.edges,
        "problems": problems,
    }))
}

/// The page itself, with the data inlined.
pub fn html(data: &Value) -> String {
    // `</script>` inside the data must not end the script block.
    let json = data.to_string().replace("</", "<\\/");
    PAGE.replace("/*__CODEMAP_DATA__*/null", &json)
}

/// Routes, controllers, models and tables. Returns what the app has.
fn backend(g: &mut Graph, store: &Store, app: &str, root: &str) -> R<Vec<&'static str>> {
    let conn = store.conn();
    let mut has = Vec::new();
    let routes = codemap_laravel::routes::all(conn)?;
    if !routes.is_empty() {
        has.push("routes");
    }
    let class_at = |fqn: &str| -> Option<(String, u32)> {
        codemap_core::symbols::by_fqn(conn, fqn)
            .ok()?
            .into_iter()
            .next()
            .map(|s| (s.file, s.line))
    };
    let methods_of = |fqn: &str| -> Vec<String> {
        let Ok(mut stmt) = conn.prepare(
            "SELECT name, start_line FROM symbols WHERE parent_fqn = ?1 AND kind = 'method' ORDER BY start_line",
        ) else {
            return Vec::new();
        };
        stmt.query_map([fqn], |r| {
            Ok(format!(
                "{}() :{}",
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?
            ))
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
    };
    let mut controllers: HashMap<String, String> = HashMap::new();
    for r in &routes {
        let methods = r.methods_str();
        let id = route_id(root, &methods, &r.uri);
        g.node(
            &id,
            json!({
                "kind": "endpoint",
                "label": format!("{} /{}", methods.replace("|HEAD", ""), r.uri.trim_start_matches('/')),
                "sub": r.name,
                "app": app,
                "file": r.file,
                "line": r.line,
                "info": {
                    "middleware": r.middleware,
                    "action": r.action_raw,
                },
            }),
        );
        if let (Some(c), Some(m)) = (&r.controller_fqn, &r.controller_method) {
            let cid = format!("class:{root}|{c}");
            if !controllers.contains_key(c) {
                let at = class_at(c);
                g.node(
                    &cid,
                    json!({
                        "kind": "controller",
                        "label": short(c),
                        "sub": c,
                        "app": app,
                        "file": at.as_ref().map(|a| a.0.clone()),
                        "line": at.as_ref().map(|a| a.1),
                        "info": { "methods": methods_of(c) },
                    }),
                );
                controllers.insert(c.clone(), at.map(|a| a.0).unwrap_or_default());
            }
            g.edge(&id, &cid, "handled_by", &format!("{m}()"));
        }
    }

    // Models and their tables.
    let mut models: Vec<(String, String, String, u32)> = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT fqn, table_name, file, line FROM models")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, i64>(3)? as u32))
        })?;
        for row in rows {
            models.push(row?);
        }
    }
    if !models.is_empty() {
        has.push("models");
    }
    let model_set: HashSet<&str> = models.iter().map(|m| m.0.as_str()).collect();
    for (fqn, table, file, line) in &models {
        let id = format!("model:{root}|{fqn}");
        g.node(
            &id,
            json!({
                "kind": "model",
                "label": short(fqn),
                "sub": fqn,
                "app": app,
                "file": file,
                "line": line,
                "info": { "table": table, "methods": methods_of(fqn) },
            }),
        );
        g.edge(&id, &format!("table:{root}|{table}"), "stored_in", "");
    }
    {
        let mut stmt = conn.prepare(
            "SELECT model_fqn, method, type, related_fqn FROM model_relations WHERE related_fqn IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (from, method, ty, to) = row?;
            if model_set.contains(to.as_str()) {
                g.edge(
                    &format!("model:{root}|{from}"),
                    &format!("model:{root}|{to}"),
                    "relation",
                    &format!("{ty} {method}()"),
                );
            }
        }
    }

    // Tables with their columns and foreign keys.
    let mut columns: HashMap<String, Vec<String>> = HashMap::new();
    let mut fks: Vec<(String, String, String)> = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT table_name, name, type, nullable, fk_table FROM db_columns ORDER BY table_name, position",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, bool>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?;
        for row in rows {
            let (t, c, ty, nullable, fk) = row?;
            columns.entry(t.clone()).or_default().push(format!(
                "{c} {ty}{}{}",
                if nullable { " null" } else { "" },
                fk.as_ref().map(|f| format!(" → {f}")).unwrap_or_default()
            ));
            if let Some(f) = fk {
                fks.push((t, f, c));
            }
        }
    }
    {
        let mut stmt =
            conn.prepare("SELECT name, created_in FROM db_tables WHERE dropped = 0 ORDER BY name")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        for row in rows {
            let (t, created) = row?;
            if !has.contains(&"tables") {
                has.push("tables");
            }
            let (file, line) = created
                .as_deref()
                .and_then(|c| c.rsplit_once(':'))
                .map(|(f, l)| (Some(f.to_owned()), l.parse::<u32>().ok()))
                .unwrap_or((None, None));
            g.node(
                &format!("table:{root}|{t}"),
                json!({
                    "kind": "table",
                    "label": t,
                    "app": app,
                    "file": file,
                    "line": line,
                    "info": { "columns": columns.remove(&t).unwrap_or_default() },
                }),
            );
        }
    }
    for (t, f, c) in fks {
        g.edge(
            &format!("table:{root}|{t}"),
            &format!("table:{root}|{f}"),
            "foreign_key",
            &c,
        );
    }
    // Drop edges to tables the schema does not know (inferred model tables).
    let known: HashSet<String> = g.nodes.keys().cloned().collect();
    g.edges.retain(|e| {
        e["kind"] != "stored_in" || known.contains(e["to"].as_str().unwrap_or_default())
    });

    // Which models each controller uses.
    if !controllers.is_empty() && !models.is_empty() {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT f.path, r.target FROM refs r JOIN files f ON f.id = r.file_id WHERE r.kind IN ('class', 'call')",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let by_file: HashMap<&str, &str> = controllers
            .iter()
            .map(|(c, f)| (f.as_str(), c.as_str()))
            .collect();
        for row in rows {
            let (file, target) = row?;
            let Some(c) = by_file.get(file.as_str()) else {
                continue;
            };
            let class = target.split("::").next().unwrap_or(&target);
            if model_set.contains(class) {
                g.edge(
                    &format!("class:{root}|{c}"),
                    &format!("model:{root}|{class}"),
                    "uses",
                    "",
                );
            }
        }
    }
    Ok(has)
}

/// Screens, the frontend files making API calls under them, and the
/// endpoints those calls reach. Returns how many screens there are.
fn frontend(g: &mut Graph, store: &Store, project: &Project, app: &str, root: &str) -> R<usize> {
    let conn = store.conn();
    let pages: Vec<(String, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT route_path, file, source FROM pages WHERE file IS NOT NULL ORDER BY route_path",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let calls: Vec<CallRow> = codemap_vue::api::load(conn)?;
    // `rooms:select` (a table and what is done to it); `rpc:fn` calls are left out.
    let db_refs: Vec<(String, String, u32)> = {
        let mut stmt = conn.prepare(
            "SELECT f.path, r.target, r.line FROM refs r JOIN files f ON f.id = r.file_id WHERE r.kind = 'db_table'",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u32>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (f, target, l) = row?;
            let table = target.split(':').next().unwrap_or(&target);
            if target.starts_with("rpc:") || table.is_empty() {
                continue;
            }
            out.push((f, table.to_owned(), l));
        }
        out
    };
    if pages.is_empty() && calls.is_empty() && db_refs.is_empty() {
        return Ok(0);
    }

    // What each file renders (whole components), imports and calls by name.
    let mut renders: HashMap<String, Vec<String>> = HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT from_path, to_path FROM component_uses WHERE to_path IS NOT NULL")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (from, to) = row?;
            renders.entry(from).or_default().push(to);
        }
    }
    let mut files_by_name: HashMap<String, Vec<String>> = HashMap::new();
    let mut spans: HashMap<String, Vec<(String, u32, u32)>> = HashMap::new();
    for s in codemap_core::symbols::all(conn)? {
        if !s.file.ends_with(".php") {
            files_by_name
                .entry(s.name.clone())
                .or_default()
                .push(s.file.clone());
            spans
                .entry(s.file)
                .or_default()
                .push((s.name, s.line, s.end_line));
        }
    }
    let mut imports: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let mut named_calls: HashMap<String, Vec<(String, u32)>> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT f.path, r.kind, r.target, r.line FROM refs r JOIN files f ON f.id = r.file_id
             WHERE r.kind IN ('import', 'js_call')",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, u32>(3)?,
            ))
        })?;
        for row in rows {
            let (from, kind, target, line) = row?;
            if kind == "import" {
                let (file, name) = target.split_once('#').unwrap_or((&target, ""));
                imports
                    .entry(from)
                    .or_default()
                    .push((file.to_owned(), name.to_owned()));
            } else {
                named_calls.entry(from).or_default().push((target, line));
            }
        }
    }
    let span_of = |file: &str, name: &str| -> Option<(u32, u32)> {
        spans
            .get(file)?
            .iter()
            .find(|(n, _, _)| n == name)
            .map(|(_, a, b)| (*a, *b))
    };

    // Frontend files that talk to a backend (API calls or database tables).
    let mut by_file: BTreeMap<&str, Vec<&CallRow>> = BTreeMap::new();
    for c in &calls {
        by_file.entry(c.file.as_str()).or_default().push(c);
    }
    let mut db_by_file: BTreeMap<&str, Vec<(&str, u32)>> = BTreeMap::new();
    for (f, t, l) in &db_refs {
        db_by_file
            .entry(f.as_str())
            .or_default()
            .push((t.as_str(), *l));
    }

    let (backends, _) = crate::api::backends(store, project)?;
    let matcher = Matcher::new(&backends, &calls);
    for (file, list) in &by_file {
        let fid = format!("file:{root}|{file}");
        let mut lines = Vec::new();
        let mut missing = Vec::new();
        for c in list {
            let label = crate::api::call_label(c);
            match matcher.outcome(c) {
                Outcome::Matched(hits) => {
                    if let Some((bi, ri)) = hits.first() {
                        let b = &backends[*bi];
                        let r = &b.routes[*ri];
                        let rid = route_id(&root_key(&b.root), &r.methods_str(), &r.uri);
                        // A linked backend outside this map still shows its endpoint.
                        g.node(
                            &rid,
                            json!({
                                "kind": "endpoint",
                                "stub": true,
                                "label": format!("{} /{}", r.methods_str().replace("|HEAD", ""), r.uri.trim_start_matches('/')),
                                "sub": r.name,
                                "app": b.name,
                                "file": r.file,
                                "line": r.line,
                                "info": { "action": r.action_raw },
                            }),
                        );
                        g.edge(&fid, &rid, "calls", &format!(":{}", c.line));
                    }
                    lines.push(format!("{label} :{}", c.line));
                }
                Outcome::External => lines.push(format!("{label} :{} (external)", c.line)),
                Outcome::MethodMismatch(_) => {
                    missing.push(format!("{label} :{} wrong method", c.line));
                }
                Outcome::NotFound(_) => {
                    missing.push(format!("{label} :{} no such endpoint", c.line));
                }
                Outcome::Unresolved(_) => {
                    lines.push(format!("{label} :{} (URL not readable)", c.line));
                }
            }
        }
        g.node(
            &fid,
            json!({
                "kind": "frontend",
                "label": file_label(file),
                "sub": file,
                "app": app,
                "file": file,
                "info": { "calls": lines, "calls_to_missing_endpoints": missing },
            }),
        );
    }
    for (file, list) in &db_by_file {
        let fid = format!("file:{root}|{file}");
        g.node(
            &fid,
            json!({
                "kind": "frontend",
                "label": file_label(file),
                "sub": file,
                "app": app,
                "file": file,
                "info": {},
            }),
        );
        let mut lines = Vec::new();
        for (t, l) in list {
            g.edge(
                &fid,
                &format!("table:{root}|{t}"),
                "reads_table",
                &format!(":{l}"),
            );
            lines.push(format!("{t} :{l}"));
        }
        g.info(&fid, "tables", json!(lines));
    }
    // A route handler file (Next.js `route.ts`) reading tables: the endpoint
    // reads them.
    for r in codemap_laravel::routes::all(conn)? {
        let Some(f) = r.file.as_deref() else { continue };
        for (t, l) in db_by_file.get(f).into_iter().flatten() {
            g.edge(
                &route_id(root, &r.methods_str(), &r.uri),
                &format!("table:{root}|{t}"),
                "reads_table",
                &format!(":{l}"),
            );
        }
    }

    // Each call's endpoint, to link screens straight to what they call.
    let mut endpoint_of: HashMap<(&str, u32), String> = HashMap::new();
    for c in &calls {
        if let Outcome::Matched(hits) = matcher.outcome(c)
            && let Some((bi, ri)) = hits.first()
        {
            let b = &backends[*bi];
            let r = &b.routes[*ri];
            endpoint_of.insert(
                (c.file.as_str(), c.line),
                route_id(&root_key(&b.root), &r.methods_str(), &r.uri),
            );
        }
    }

    // Screens: walk what they render, import and call, following a function
    // into its own body only (a shared composable file counts only for the
    // functions this screen uses).
    let mut screens = 0;
    for (path, file, source) in &pages {
        let sid = format!("screen:{root}|{path}");
        screens += 1;
        g.node(
            &sid,
            json!({
                "kind": "screen",
                "label": path,
                "sub": source,
                "app": app,
                "file": file,
                "info": {},
            }),
        );
        // `(file, function)`; an empty function name is the whole file.
        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut queue: VecDeque<(String, String, usize)> =
            VecDeque::from([(file.clone(), String::new(), 0)]);
        let mut reached: BTreeMap<String, Vec<u32>> = BTreeMap::new();
        while let Some((f, func, d)) = queue.pop_front() {
            if !seen.insert((f.clone(), func.clone())) {
                continue;
            }
            let span = (!func.is_empty()).then(|| span_of(&f, &func)).flatten();
            let inside = |line: u32| span.is_none_or(|(a, b)| line >= a && line <= b);
            for c in by_file.get(f.as_str()).into_iter().flatten() {
                if inside(c.line) {
                    reached.entry(f.clone()).or_default().push(c.line);
                }
            }
            for (t, l) in db_by_file.get(f.as_str()).into_iter().flatten() {
                if inside(*l) {
                    reached.entry(f.clone()).or_default().push(*l);
                    g.edge(
                        &sid,
                        &format!("table:{root}|{t}"),
                        "screen_reads",
                        &format!("{f}:{l}"),
                    );
                }
            }
            if d >= DEPTH {
                continue;
            }
            for (name, line) in named_calls.get(&f).into_iter().flatten() {
                if !inside(*line) {
                    continue;
                }
                for target in files_by_name.get(name).into_iter().flatten() {
                    queue.push_back((target.clone(), name.clone(), d + 1));
                }
            }
            if span.is_none() {
                for to in renders.get(&f).into_iter().flatten() {
                    queue.push_back((to.clone(), String::new(), d + 1));
                }
                for (to, name) in imports.get(&f).into_iter().flatten() {
                    let whole = name.is_empty() || name == "default" || name == "*";
                    let name = if whole || span_of(to, name).is_none() {
                        String::new()
                    } else {
                        name.clone()
                    };
                    queue.push_back((to.clone(), name, d + 1));
                }
            }
        }
        let mut lines = Vec::new();
        for (f, at) in &reached {
            g.edge(
                &sid,
                &format!("file:{root}|{f}"),
                "uses",
                if f == file { "" } else { "via components" },
            );
            for l in at {
                if let Some(rid) = endpoint_of.get(&(f.as_str(), *l)) {
                    g.edge(&sid, rid, "screen_calls", &format!("{f}:{l}"));
                }
            }
            lines.push(format!("{f} ({} calls)", at.len()));
        }
        g.info(&sid, "talks_through", json!(lines));
    }
    Ok(screens)
}

/// Drops edges whose ends are not in the map (a linked backend not mapped).
pub fn prune(data: &mut Value) {
    let ids: HashSet<String> = data["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| n["id"].as_str().map(str::to_owned))
        .collect();
    if let Some(edges) = data["edges"].as_array_mut() {
        edges.retain(|e| {
            ids.contains(e["from"].as_str().unwrap_or_default())
                && ids.contains(e["to"].as_str().unwrap_or_default())
        });
    }
}
