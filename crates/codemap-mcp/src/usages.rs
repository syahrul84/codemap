//! `find_usages`: every place a class, method, function, route, component,
//! composable / store, translation key, endpoint or file is used, with the
//! tests that touch it listed separately.

use std::collections::BTreeSet;

use codemap_core::Store;
use codemap_core::envelope::Detail;
use codemap_core::symbols::{self, SymbolKind, SymbolRecord};
use codemap_laravel::routes::{self, RouteDef};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::api::{ApiOutput, fit, route_segs};
use crate::tools::{DetailParam, Project, ToolError};

type R<T> = Result<T, ToolError>;

#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct FindUsagesParams {
    /// Workspace app to look at (`backend`, `consumer`, …). Omit to search every app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// What to look for: `App\Models\Order`, `Order`, `Order::save`, a function,
    /// a route name (`orders.show`), a component (`OrderCard`), a composable or
    /// store (`useCart`), a translation key (`orders.title`), an endpoint
    /// (`GET api/v1/orders`), or a file path.
    #[serde(default)]
    pub target: String,
    /// Change impact instead of one target: files you changed (paths from the
    /// project root). Returns what each affects and the tests to run, with commands.
    #[serde(default)]
    pub changed_files: Option<Vec<String>>,
    /// Change impact for the files git shows as changed (uncommitted, incl. untracked).
    #[serde(default)]
    pub git_changes: bool,
    /// Force an interpretation: class, method, function, route, component,
    /// composable, store, i18n, endpoint, file.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub detail: DetailParam,
}

/// Tests by convention: `tests/`, `__tests__`, `*.spec.*`, `*.test.*`, `*Test.php`.
pub fn is_test_path(p: &str) -> bool {
    p.starts_with("tests/")
        || p.starts_with("test/")
        || p.contains("/tests/")
        || p.contains("/__tests__/")
        || p.contains(".spec.")
        || p.contains(".test.")
        || p.ends_with("Test.php")
        || p.contains("/e2e/")
}

#[derive(Default)]
struct Found {
    resolved_as: String,
    /// A JS symbol: its file, name and line, to count uses in its own file.
    own_file: Option<(String, String, u32)>,
    /// `(file, line, label)`.
    hits: BTreeSet<(String, u32, String)>,
    possible: BTreeSet<(String, u32, String)>,
    routes: Vec<String>,
    notes: Vec<String>,
}

impl Found {
    fn add(&mut self, file: String, line: u32, label: impl Into<String>) {
        self.hits.insert((file, line, label.into()));
    }
}

fn refs(
    store: &Store,
    sql_where: &str,
    params: &[&dyn rusqlite::ToSql],
) -> R<Vec<(String, u32, String, String)>> {
    let mut stmt = store.conn().prepare(&format!(
        "SELECT f.path, r.line, r.kind, r.target FROM refs r JOIN files f ON f.id = r.file_id WHERE {sql_where}
         ORDER BY f.path, r.line"
    ))?;
    let rows = stmt.query_map(params, |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// PHP tests calling `$this->getJson('/api/...')` that hit `route`.
fn tests_hitting(store: &Store, route: &RouteDef, f: &mut Found) -> R<()> {
    let segs = route_segs(&route.uri);
    for (file, line, _, target) in refs(store, "r.kind = 'http'", &[])? {
        let Some((verb, url)) = target.split_once(' ') else {
            continue;
        };
        if !route.methods.iter().any(|m| m == verb) {
            continue;
        }
        let url = url.split('?').next().unwrap_or(url);
        let call: Vec<&str> = url
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|s| if s.contains("{}") { "{}" } else { s })
            .collect();
        if fit(&call, &segs).is_some() {
            f.add(file, line, format!("{verb} {url}"));
        }
    }
    for (file, line, _, _) in refs(store, "r.kind = 'route' AND r.target = ?1", &[&route.name])? {
        if route.name.is_some() {
            f.add(
                file,
                line,
                format!("route('{}')", route.name.as_deref().unwrap_or("")),
            );
        }
    }
    Ok(())
}

fn class_like(store: &Store, name: &str) -> R<Vec<SymbolRecord>> {
    let name = name.trim().trim_start_matches('\\');
    let mut found: Vec<SymbolRecord> = symbols::by_fqn(store.conn(), name)?
        .into_iter()
        .filter(|s| s.kind.is_class_like())
        .collect();
    if found.is_empty() && !name.contains('\\') {
        found = symbols::all_named(store.conn(), name)?
            .into_iter()
            .filter(|s| s.kind.is_class_like() && s.file.ends_with(".php"))
            .collect();
    }
    Ok(found)
}

fn php_class(store: &Store, fqn: &str, f: &mut Found) -> R<()> {
    f.resolved_as = format!("class {fqn}");
    let member_prefix = format!("{fqn}::%");
    for (file, line, kind, target) in refs(
        store,
        "(r.target = ?1 COLLATE NOCASE AND r.kind = 'class') OR (r.kind = 'call' AND r.target LIKE ?2)",
        &[&fqn, &member_prefix],
    )? {
        let label = if kind == "call" {
            format!("{}()", target.rsplit("::").next().unwrap_or(&target))
        } else {
            kind
        };
        f.add(file, line, label);
    }
    for r in routes::all(store.conn())?.iter().filter(|r| {
        r.controller_fqn
            .as_deref()
            .is_some_and(|c| c.eq_ignore_ascii_case(fqn))
    }) {
        f.routes.push(format!(
            "{} {} → {}",
            r.methods_str(),
            r.uri,
            r.action_label()
        ));
        tests_hitting(store, r, f)?;
    }
    Ok(())
}

/// Classes that use the classes of `file`, and the classes using those (up to
/// `max_depth` steps), with the chain of short names that reached each:
/// `Popularity` → `BrandDirectory` → `BrandController`.
fn php_caller_chain(store: &Store, file: &str, max_depth: usize) -> R<Vec<(String, Vec<String>)>> {
    let mut classes_by_file: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for s in symbols::all(store.conn())? {
        if s.kind.is_class_like() && s.file.ends_with(".php") {
            classes_by_file.entry(s.file).or_default().push(s.fqn);
        }
    }
    let short = |fqn: &str| fqn.rsplit('\\').next().unwrap_or(fqn).to_owned();
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut frontier: Vec<(String, Vec<String>)> = classes_by_file
        .get(file)
        .into_iter()
        .flatten()
        .map(|c| (c.clone(), vec![short(c)]))
        .collect();
    for c in &frontier {
        seen.insert(c.0.clone());
    }
    for _ in 0..max_depth {
        let mut next = Vec::new();
        for (class, path) in &frontier {
            let members = format!("{class}::%");
            for (from, _, _, _) in refs(
                store,
                "(r.target = ?1 COLLATE NOCASE AND r.kind = 'class') OR (r.kind = 'call' AND r.target LIKE ?2)",
                &[class, &members],
            )? {
                if is_test_path(&from) {
                    continue;
                }
                for c in classes_by_file.get(&from).into_iter().flatten() {
                    if seen.insert(c.clone()) {
                        let mut p = path.clone();
                        p.push(short(c));
                        out.push((c.clone(), p.clone()));
                        next.push((c.clone(), p));
                    }
                }
            }
        }
        if next.is_empty() || out.len() > 400 {
            break;
        }
        frontier = next;
    }
    Ok(out)
}

fn php_method(store: &Store, class: &str, method: &str, f: &mut Found) -> R<()> {
    let mut classes = vec![class.to_owned()];
    classes.extend(
        symbols::descendants(store.conn(), &[class])?
            .into_iter()
            .map(|c| c.fqn),
    );
    f.resolved_as = format!("method {class}::{method}");
    for c in &classes {
        let target = format!("{c}::{method}");
        for (file, line, _, t) in refs(
            store,
            "r.kind = 'call' AND r.target = ?1 COLLATE NOCASE",
            &[&target],
        )? {
            f.add(
                file,
                line,
                if *c == class {
                    "call".to_owned()
                } else {
                    format!("call via {}", t.split("::").next().unwrap_or(""))
                },
            );
        }
    }
    for (file, line, _, _) in refs(
        store,
        "r.kind = 'call_unknown' AND r.target = ?1",
        &[&method],
    )? {
        f.possible.insert((
            file,
            line,
            format!("->{method}() on an object of unknown type"),
        ));
    }
    for r in routes::all(store.conn())?.iter().filter(|r| {
        r.controller_fqn
            .as_deref()
            .is_some_and(|c| c.eq_ignore_ascii_case(class))
            && r.controller_method.as_deref() == Some(method)
    }) {
        f.routes.push(format!(
            "{} {}{}",
            r.methods_str(),
            r.uri,
            r.name
                .as_ref()
                .map(|n| format!(" [{n}]"))
                .unwrap_or_default()
        ));
        tests_hitting(store, r, f)?;
    }
    Ok(())
}

fn js_symbol(store: &Store, sym: &SymbolRecord, f: &mut Found) -> R<()> {
    f.resolved_as = format!("{} {} ({})", sym.kind, sym.name, sym.file);
    if sym.kind == SymbolKind::Component {
        let mut stmt = store.conn().prepare(
            "SELECT from_path, line, tag FROM component_uses WHERE to_path = ?1 ORDER BY from_path, line",
        )?;
        let rows = stmt.query_map([&sym.file], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, u32>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (file, line, tag) = row?;
            f.add(file, line, format!("<{tag}>"));
        }
        let pattern = format!("{}#%", sym.file);
        for (file, line, _, _) in
            refs(store, "r.kind = 'import' AND r.target LIKE ?1", &[&pattern])?
        {
            f.add(file, line, "import");
        }
        f.routes = codemap_vue::pages::routes_for_file(store, &sym.file)?;
        return Ok(());
    }
    f.own_file = Some((sym.file.clone(), sym.name.clone(), sym.line));
    let exact = format!("{}#{}", sym.file, sym.name);
    let default = format!("{}#default", sym.file);
    let star = format!("{}#*", sym.file);
    let stem = sym
        .file
        .rsplit('/')
        .next()
        .unwrap_or("")
        .split('.')
        .next()
        .unwrap_or("");
    let is_default = stem == sym.name;
    for (file, line, _, target) in refs(
        store,
        "r.kind = 'import' AND (r.target = ?1 OR r.target = ?2 OR (r.target = ?3 AND ?4))",
        &[&exact, &star, &default, &is_default],
    )? {
        f.add(
            file,
            line,
            if target.ends_with("#*") {
                "import * as"
            } else {
                "import"
            },
        );
    }
    // Auto-imported (Nuxt `composables/`, `utils/`, stores) or global: called by name.
    if matches!(
        sym.kind,
        SymbolKind::Composable | SymbolKind::Store | SymbolKind::Function
    ) {
        for (file, line, _, _) in refs(store, "r.kind = 'js_call' AND r.target = ?1", &[&sym.name])?
        {
            if file != sym.file {
                f.add(file, line, format!("{}()", sym.name));
            }
        }
    }
    Ok(())
}

pub fn find_usages(store: &Store, project: &Project, p: FindUsagesParams) -> R<ApiOutput> {
    let detail: Detail = p.detail.into();
    if p.git_changes || p.changed_files.is_some() {
        let mut files = p.changed_files.clone().unwrap_or_default();
        if p.git_changes {
            match codemap_core::git::changed_files(&project.root) {
                Some(g) => files.extend(g),
                None => {
                    return Err(ToolError::InvalidParams(
                        "git_changes: this folder is not a git repository (or git is unavailable); pass changed_files".into(),
                    ));
                }
            }
        }
        files.sort();
        files.dedup();
        return impact(store, project, &files, detail);
    }
    if p.target.trim().is_empty() {
        return Err(ToolError::InvalidParams(
            "pass `target`, `changed_files`, or `git_changes: true`".into(),
        ));
    }
    let target = p.target.trim().to_owned();
    let kind = p.kind.as_deref().unwrap_or("").to_ascii_lowercase();
    let mut f = Found::default();
    let all_routes = routes::all(store.conn())?;

    let verb = target
        .split_once(' ')
        .filter(|(v, _)| v.chars().all(|c| c.is_ascii_uppercase()));
    if kind == "endpoint" || verb.is_some() {
        // Frontend callers come from api_usage; PHP tests from their HTTP calls.
        let out = crate::api::api_usage(
            store,
            project,
            crate::api::ApiUsageParams {
                endpoint: Some(target.clone()),
                limit: Some(10),
                ..Default::default()
            },
        )?;
        f.resolved_as = format!("endpoint {target}");
        for e in out.data["endpoints"].as_array().into_iter().flatten() {
            f.routes.push(e["route"].as_str().unwrap_or("").to_owned());
            for c in e["callers"].as_array().into_iter().flatten() {
                let at = c["at"].as_str().unwrap_or("");
                let (file, line) = at.rsplit_once(':').unwrap_or((at, "0"));
                f.add(
                    file.to_owned(),
                    line.parse().unwrap_or(0),
                    c["call"].as_str().unwrap_or("call").to_owned(),
                );
            }
        }
        if let Some((v, uri)) = verb {
            let segs = route_segs(uri.trim_matches('/'));
            for r in all_routes
                .iter()
                .filter(|r| r.methods.iter().any(|m| m == v))
            {
                if route_segs(&r.uri) == segs {
                    tests_hitting(store, r, &mut f)?;
                }
            }
        }
    } else if (kind.is_empty() || kind == "route")
        && let Some(r) = all_routes
            .iter()
            .find(|r| r.name.as_deref() == Some(target.as_str()))
    {
        f.resolved_as = format!("route {target} ({} {})", r.methods_str(), r.uri);
        for (file, line, _, _) in refs(store, "r.kind = 'route' AND r.target = ?1", &[&target])? {
            f.add(file, line, format!("route('{target}')"));
        }
        tests_hitting(store, r, &mut f)?;
        f.routes.push(format!(
            "{} {} → {}",
            r.methods_str(),
            r.uri,
            r.action_label()
        ));
    } else if let Some((class, method)) = target
        .split_once("::")
        .filter(|_| kind.is_empty() || kind == "method")
    {
        let classes = class_like(store, class)?;
        match classes.as_slice() {
            [] => f.notes.push(format!("no class named {class}")),
            [c] => php_method(
                store,
                &c.fqn,
                method.trim_start_matches('$').trim_end_matches("()"),
                &mut f,
            )?,
            many => {
                f.notes.push(format!(
                    "{class} is ambiguous: {}; pass the FQN",
                    many.iter()
                        .map(|c| c.fqn.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
    } else if kind == "table" || (kind.is_empty() && is_db_table(store, &target)?) {
        // A table used through a query builder (`supabase.from('rooms')`) or
        // known from the schema.
        f.resolved_as = format!("table {target}");
        let rows = refs(
            store,
            "r.kind = 'db_table' AND (r.target LIKE ?1 OR r.target = ?2)",
            &[&format!("{target}:%"), &format!("rpc:{target}")],
        )?;
        for (file, line, _, t) in rows {
            let op = t.rsplit(':').next().unwrap_or("").to_owned();
            f.add(file, line, op);
        }
    } else if kind == "i18n" || (kind.is_empty() && is_i18n_target(store, &target)?) {
        f.resolved_as = format!("translation key {target}");
        let mut stmt = store.conn().prepare(
            "SELECT f.path, u.line, u.key, u.dynamic FROM i18n_uses u JOIN files f ON f.id = u.file_id ORDER BY f.path, u.line",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, u32>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, bool>(3)?,
            ))
        })?;
        for row in rows {
            let (file, line, key, dynamic) = row?;
            let hit = key == target
                || key.split_once('.').is_some_and(|(_, rest)| rest == target)
                || target.split_once('.').is_some_and(|(_, rest)| rest == key)
                || (dynamic && target.contains(key.as_str()));
            if hit {
                f.add(
                    file,
                    line,
                    if dynamic {
                        format!("{key}… (dynamic)")
                    } else {
                        key
                    },
                );
            }
        }
    } else {
        // Symbols: PHP class / function, JS component / composable / store / function.
        let is_path = target.contains('/') && store.list_files()?.iter().any(|x| x.path == target);
        if is_path || kind == "file" {
            f.resolved_as = format!("file {target}");
            let pattern = format!("{target}#%");
            for (file, line, _, _) in
                refs(store, "r.kind = 'import' AND r.target LIKE ?1", &[&pattern])?
            {
                f.add(file, line, "import");
            }
            let classes: Vec<SymbolRecord> = symbols::all(store.conn())?
                .into_iter()
                .filter(|s| s.file == target && s.kind.is_class_like())
                .collect();
            for c in classes {
                php_class(store, &c.fqn, &mut f)?;
            }
            if let Ok(Some(_)) =
                store
                    .conn()
                    .query_row("SELECT 1 FROM js_modules WHERE path = ?1", [&target], |r| {
                        r.get::<_, Option<i64>>(0)
                    })
            {
                let (used_by, _) = codemap_vue::component::used_by(store, &target, 500)?;
                for u in used_by {
                    let s = u.as_str().unwrap_or("");
                    if let Some((file, line)) = s.rsplit_once(':') {
                        f.add(file.to_owned(), line.parse().unwrap_or(0), "renders it");
                    }
                }
            }
            f.resolved_as = format!("file {target}");
        } else {
            let classes = class_like(store, &target)?;
            let js: Vec<SymbolRecord> = symbols::all_named(store.conn(), &target)?
                .into_iter()
                .filter(|s| {
                    !s.file.ends_with(".php")
                        && matches!(
                            s.kind,
                            SymbolKind::Component
                                | SymbolKind::Composable
                                | SymbolKind::Store
                                | SymbolKind::Function
                                | SymbolKind::Const
                                | SymbolKind::Class
                        )
                })
                .collect();
            let pascal = codemap_js::template::pascal(&target);
            let js = if js.is_empty() && pascal != target {
                symbols::all_named(store.conn(), &pascal)?
                    .into_iter()
                    .filter(|s| s.kind == SymbolKind::Component)
                    .collect()
            } else {
                js
            };
            match (classes.as_slice(), js.as_slice()) {
                ([c], []) => php_class(store, &c.fqn, &mut f)?,
                ([], [s]) => js_symbol(store, s, &mut f)?,
                ([], []) => {
                    let funcs = refs(store, "r.kind = 'function' AND r.target = ?1", &[&target])?;
                    if funcs.is_empty() {
                        f.notes.push(format!(
                            "nothing named `{target}` was found (classes, methods as Class::method, functions, routes, components, composables, stores, translation keys, endpoints, files)"
                        ));
                    } else {
                        f.resolved_as = format!("function {target}");
                        for (file, line, _, _) in funcs {
                            f.add(file, line, format!("{target}()"));
                        }
                    }
                }
                (cs, js) => {
                    let mut names: Vec<String> = cs.iter().map(|c| c.fqn.clone()).collect();
                    names.extend(js.iter().map(|s| s.fqn.clone()));
                    f.notes.push(format!(
                        "`{target}` is ambiguous: {}; pass the FQN, path, or `kind`",
                        names.join(", ")
                    ));
                }
            }
        }
    }

    // Uses in the symbol's own file (`PIN_TOLERANCE_M[match]` inside another
    // exported function): they count, so it is not called unused.
    if let Some((file, name, decl_line)) = f.own_file.clone()
        && let Ok(text) = std::fs::read_to_string(project.root.join(&file))
    {
        let mut seen_decl = false;
        for (i, line) in text.lines().enumerate() {
            let n = i as u32 + 1;
            if n == decl_line || !line.contains(name.as_str()) {
                continue;
            }
            let t = line.trim_start();
            if t.starts_with("//") || t.starts_with('*') {
                continue;
            }
            let b = line.as_bytes();
            let word = line.match_indices(name.as_str()).any(|(at, _)| {
                let before = at == 0
                    || !(b[at - 1].is_ascii_alphanumeric()
                        || b[at - 1] == b'_'
                        || b[at - 1] == b'$');
                let j = at + name.len();
                let after =
                    j >= b.len() || !(b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b'$');
                before && after
            });
            // The declaration itself when the symbol's line is off by a decorator/comment.
            let declares = !seen_decl
                && [
                    "const ",
                    "function ",
                    "let ",
                    "class ",
                    "type ",
                    "interface ",
                ]
                .iter()
                .any(|k| t.contains(&format!("{k}{name}")));
            if declares {
                seen_decl = true;
                continue;
            }
            if word {
                f.add(file.clone(), n, "same file");
            }
        }
    }

    // `Order::query()` records the class and the call on one line: keep the call.
    let specific: BTreeSet<(String, u32)> = f
        .hits
        .iter()
        .filter(|(_, _, label)| label != "class")
        .map(|(file, line, _)| (file.clone(), *line))
        .collect();
    f.hits.retain(|(file, line, label)| {
        label != "class" || !specific.contains(&(file.clone(), *line))
    });

    // Split tests from the rest and paginate.
    let (tests, usages): (Vec<_>, Vec<_>) = f
        .hits
        .into_iter()
        .partition(|(file, _, _)| is_test_path(file));
    let limit = p.limit.unwrap_or(30).clamp(1, 200);
    let offset = p
        .cursor
        .as_deref()
        .and_then(|c| c.strip_prefix('o'))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0usize);
    let fmt = |(file, line, label): &(String, u32, String)| -> Value {
        if detail == Detail::Full {
            json!({ "file": file, "line": line, "what": label })
        } else {
            json!(format!("{file}:{line}  {label}"))
        }
    };
    let total = usages.len();
    let shown: Vec<Value> = usages.iter().skip(offset).take(limit).map(fmt).collect();
    let mut test_files: Vec<String> = tests.iter().map(|(file, _, _)| file.clone()).collect();
    test_files.dedup();
    // Found the definition but no use: it exists, nothing uses it (dead code,
    // or used in ways codemap can't see such as dynamic names).
    let unused = total + tests.len() == 0 && !f.resolved_as.is_empty();
    let mut data = json!({
        "target": target,
        "resolved_as": f.resolved_as,
        "found": total + tests.len() > 0 || unused,
        "total": total,
        "usages": shown,
    });
    if unused {
        data["unused"] = json!(true);
        data["hint"] = json!(format!(
            "`{target}` exists ({}) but nothing in this project uses it: likely dead code. Check for dynamic uses (string names, other repositories) before removing it.",
            f.resolved_as
        ));
    }
    if !tests.is_empty() {
        data["tests"] = json!(tests.iter().take(100).map(fmt).collect::<Vec<_>>());
        data["test_files"] = json!(test_files);
    }
    if !f.routes.is_empty() {
        data["routes"] = json!(f.routes);
    }
    if !f.possible.is_empty() {
        data["possible"] = json!(f.possible.iter().take(30).map(fmt).collect::<Vec<_>>());
        data["possible_total"] = json!(f.possible.len());
        data["possible_note"] = json!(
            "Calls of a method with this name on objects whose class is not known statically."
        );
    }
    if !f.notes.is_empty() {
        data["notes"] = json!(f.notes);
    }
    let next = (offset + limit < total).then(|| format!("o{}", offset + limit));
    Ok(ApiOutput {
        data,
        truncated: next.is_some(),
        next_cursor: next,
    })
}

/// A database table this project reads or writes through a query builder
/// (or an `rpc` function), not otherwise a class or symbol.
fn is_db_table(store: &Store, target: &str) -> R<bool> {
    if target.contains(['/', '\\', ' ', ':', '.']) {
        return Ok(false);
    }
    let n: i64 = store.conn().query_row(
        "SELECT COUNT(*) FROM refs WHERE kind = 'db_table' AND (target LIKE ?1 || ':%' OR target = 'rpc:' || ?1)",
        [target],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

fn is_i18n_target(store: &Store, target: &str) -> R<bool> {
    if !target.contains('.') || target.contains(['/', '\\', ' ', ':']) {
        return Ok(false);
    }
    let n: i64 = store.conn().query_row(
        "SELECT (SELECT COUNT(*) FROM i18n_keys WHERE key = ?1 OR ?1 LIKE '%.' || key)
              + (SELECT COUNT(*) FROM i18n_uses WHERE key = ?1)",
        [target],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

// ---------------------------------------------------------------- change impact

/// What each changed file affects, the tests that cover it, and how to run them.
/// A PHP file PHPUnit can run: a concrete class with test methods
/// (`test…`, `#[Test]`, `@test`) or a Pest file.
fn is_php_test_file(root: &std::path::Path, file: &str) -> bool {
    let Ok(src) = std::fs::read_to_string(root.join(file)) else {
        return false;
    };
    if src.contains("abstract class ") {
        return false;
    }
    let pest = src.lines().any(|l| {
        let t = l.trim_start();
        t.starts_with("it(") || t.starts_with("test(") || t.starts_with("describe(")
    });
    pest || src.contains("function test") || src.contains("#[Test]") || src.contains("@test")
}

/// The test files to run: base test classes are replaced by the concrete
/// tests that extend them (directly or through other bases).
fn runnable_tests(
    store: &Store,
    root: &std::path::Path,
    tests: BTreeSet<String>,
) -> R<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    let mut bases: Vec<String> = Vec::new();
    for t in tests {
        if !t.ends_with(".php") || is_php_test_file(root, &t) {
            out.insert(t);
        } else {
            bases.push(t);
        }
    }
    if bases.is_empty() {
        return Ok(out);
    }
    let all = symbols::all(store.conn())?;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    while let Some(base_file) = bases.pop() {
        if !seen.insert(base_file.clone()) {
            continue;
        }
        let base_classes: Vec<&String> = all
            .iter()
            .filter(|s| s.file == base_file && s.kind.is_class_like())
            .map(|s| &s.fqn)
            .collect();
        for s in all.iter().filter(|s| {
            s.kind.is_class_like()
                && is_test_path(&s.file)
                && s.meta
                    .extends
                    .iter()
                    .any(|e| base_classes.iter().any(|b| b.eq_ignore_ascii_case(e)))
        }) {
            if is_php_test_file(root, &s.file) {
                out.insert(s.file.clone());
            } else {
                bases.push(s.file.clone());
            }
        }
    }
    Ok(out)
}

fn impact(store: &Store, project: &Project, files: &[String], detail: Detail) -> R<ApiOutput> {
    let indexed: std::collections::HashSet<String> =
        store.list_files()?.into_iter().map(|f| f.path).collect();
    let all_routes = routes::all(store.conn())?;
    let mut entries = Vec::new();
    let mut tests: BTreeSet<String> = BTreeSet::new();
    let mut affected: BTreeSet<String> = BTreeSet::new();
    // Per changed PHP file: routes reached through callers, and the tests hitting them.
    let mut route_chain: Vec<(String, Vec<String>, BTreeSet<String>)> = Vec::new();
    for file in files {
        let file = file.trim_start_matches("./").to_owned();
        if is_test_path(&file) {
            tests.insert(file.clone());
            entries.push(json!({ "file": file, "is": "a test" }));
            continue;
        }
        if !indexed.contains(&file) {
            entries.push(json!({ "file": file, "is": "not indexed (deleted, or not a code file codemap reads)" }));
            continue;
        }
        let mut f = Found::default();
        let mut what: Vec<String> = Vec::new();
        if file.ends_with(".php") {
            let classes: Vec<SymbolRecord> = symbols::all(store.conn())?
                .into_iter()
                .filter(|s| s.file == file && s.kind.is_class_like())
                .collect();
            for c in &classes {
                php_class(store, &c.fqn, &mut f)?;
                what.push(format!("{} {}", c.kind, c.fqn));
            }
            for r in all_routes
                .iter()
                .filter(|r| r.file.as_deref() == Some(file.as_str()))
            {
                f.routes.push(format!(
                    "{} {} → {}",
                    r.methods_str(),
                    r.uri,
                    r.action_label()
                ));
                tests_hitting(store, r, &mut f)?;
            }
            // Callers of callers up to the controllers: their routes and the
            // tests that hit them (`Popularity` → `BrandDirectory` → `BrandController`).
            let mut via = Found::default();
            let mut via_routes: Vec<String> = Vec::new();
            if !classes.is_empty() {
                let chain = php_caller_chain(store, &file, 4)?;
                for (class, path) in &chain {
                    for r in all_routes.iter().filter(|r| {
                        r.controller_fqn
                            .as_deref()
                            .is_some_and(|c| c.eq_ignore_ascii_case(class))
                    }) {
                        let label =
                            format!("{} {} (via {})", r.methods_str(), r.uri, path.join(" → "));
                        if via_routes.len() < 30
                            && !f
                                .routes
                                .iter()
                                .any(|x| x.starts_with(&format!("{} {} ", r.methods_str(), r.uri)))
                        {
                            via_routes.push(label);
                            tests_hitting(store, r, &mut via)?;
                        }
                    }
                }
            }
            // Console commands the change reaches (itself or through callers),
            // and the tests that run them (`$this->artisan('credit:fold')`).
            if !classes.is_empty() {
                let mut reach: Vec<(String, Vec<String>)> = classes
                    .iter()
                    .map(|c| (c.fqn.clone(), vec![c.name.clone()]))
                    .collect();
                reach.extend(php_caller_chain(store, &file, 4)?);
                let mut seen = BTreeSet::new();
                for (class, path) in reach {
                    let Some(sym) = symbols::by_fqn(store.conn(), &class)?.into_iter().next()
                    else {
                        continue;
                    };
                    let command = sym.file.starts_with("app/Console/")
                        || sym.meta.extends.iter().any(|e| e.ends_with("Command"));
                    let Some(sig) = command
                        .then(|| codemap_laravel::background::signature(&sym, &project.root))
                        .flatten()
                    else {
                        continue;
                    };
                    if !seen.insert(sig.clone()) {
                        continue;
                    }
                    let mut hit = false;
                    for (t, line, _, _) in
                        refs(store, "r.kind = 'command' AND r.target = ?1", &[&sig])?
                    {
                        if is_test_path(&t) {
                            via.add(t, line, format!("artisan {sig}"));
                            hit = true;
                        }
                    }
                    if hit && via_routes.len() < 30 {
                        via_routes.push(format!("artisan {sig} (via {})", path.join(" → ")));
                    }
                }
            }
            let via_tests: BTreeSet<String> = via
                .hits
                .iter()
                .map(|h| h.0.clone())
                .filter(|t| is_test_path(t))
                .collect();
            if !via_routes.is_empty() {
                route_chain.push((file.clone(), via_routes, via_tests));
            }
            if file.starts_with("database/migrations/") {
                what.push("migration (check get_schema for the tables it changes)".into());
            }
            if codemap_laravel::lang::lang_file(&file).is_some() {
                what.push("translations".into());
            }
            if f.routes.len() > 20 {
                let n = f.routes.len();
                f.routes.truncate(20);
                f.routes.push(format!("… {} more", n - 20));
            }
        } else {
            let syms: Vec<SymbolRecord> = symbols::all(store.conn())?
                .into_iter()
                .filter(|s| {
                    s.file == file && !matches!(s.kind, SymbolKind::Method | SymbolKind::Property)
                })
                .collect();
            for s in &syms {
                js_symbol(store, s, &mut f)?;
                what.push(format!("{} {}", s.kind, s.name));
            }
            let pattern = format!("{file}#%");
            for (from, line, _, _) in
                refs(store, "r.kind = 'import' AND r.target LIKE ?1", &[&pattern])?
            {
                f.add(from, line, "import");
            }
            if codemap_vue::i18n::locale_of(&file).is_some() {
                what.push("translations".into());
            }
        }
        let users: BTreeSet<String> = f.hits.iter().map(|h| h.0.clone()).collect();
        let (file_tests, file_users): (Vec<String>, Vec<String>) = users
            .into_iter()
            .filter(|u| *u != file)
            .partition(|u| is_test_path(u));
        // Frontend: tests that import a file that uses the changed one.
        let mut indirect = BTreeSet::new();
        if !file.ends_with(".php") {
            for u in &file_users {
                let pattern = format!("{u}#%");
                for (from, _, _, _) in
                    refs(store, "r.kind = 'import' AND r.target LIKE ?1", &[&pattern])?
                {
                    if is_test_path(&from) {
                        indirect.insert(from);
                    }
                }
            }
        }
        tests.extend(file_tests.iter().cloned());
        tests.extend(indirect.iter().cloned());
        affected.extend(file_users.iter().cloned());
        let mut e = json!({
            "file": file,
            "is": if what.is_empty() { "code".to_owned() } else { what.join("; ") },
            "used_in_files": file_users.len(),
            "tests": file_tests.len() + indirect.len(),
        });
        if !f.routes.is_empty() {
            e["routes"] = json!(f.routes);
        }
        if let Some((_, via_routes, via_tests)) = route_chain.iter().find(|(f2, _, _)| *f2 == file)
        {
            let new: Vec<&String> = via_tests
                .iter()
                .filter(|t| !file_tests.contains(t) && !indirect.contains(*t))
                .collect();
            e["routes_via_callers"] = json!(via_routes);
            e["tests_direct"] = json!(file_tests.len() + indirect.len());
            e["tests_via_route"] = json!(new.len());
            if detail == Detail::Full || new.len() <= 20 {
                e["tests_via_route_files"] = json!(new);
            }
            tests.extend(new.into_iter().cloned());
        }
        if detail == Detail::Full {
            e["used_in"] = json!(file_users);
            e["test_files"] = json!(file_tests.iter().chain(&indirect).collect::<Vec<_>>());
        } else if !file_users.is_empty() {
            e["used_in"] = json!(file_users.iter().take(10).collect::<Vec<_>>());
        }
        entries.push(e);
    }
    // Browser tests that open the screens the change ends up in.
    let changed: Vec<String> = files
        .iter()
        .map(|f| f.trim_start_matches("./").to_owned())
        .collect();
    let browser = crate::browser::for_files(store, project, &changed)?;
    for (t, _) in &browser.tests {
        tests.insert(t.clone());
    }
    // Only files PHPUnit can run: a base test class (abstract, or without
    // test methods) is swapped for the tests that extend it.
    let tests: Vec<String> = runnable_tests(store, &project.root, tests)?
        .into_iter()
        .collect();
    let all_tests: Vec<String> = indexed
        .iter()
        .filter(|p| is_test_path(p) && (!p.ends_with(".php") || is_php_test_file(&project.root, p)))
        .cloned()
        .collect();
    let commands = test_commands(project, &tests, &all_tests);
    let mut data = json!({
        "found": !files.is_empty(),
        "changed": entries,
        "affected_files": affected.len(),
        "tests_total": tests.len(),
        "run": commands,
    });
    if let Some(n) = php_runner_note(project, &tests) {
        data["run_note"] = json!(n);
    }
    // The project's rules for the files changed (`[[rules]]`).
    let changes = crate::deploy::changes_of(&project.root, files);
    let rules: Vec<String> =
        crate::deploy::rules_hit(&project.root, &project.config.rules, &changes)
            .into_iter()
            .map(|(r, f)| format!("{} ({})", r.note.trim(), f.join(", ")))
            .collect();
    if !rules.is_empty() {
        data["rules"] = json!(rules);
    }
    // Tests to run that only pass with the right data in the database.
    let live = crate::livedata::find(&project.root, &tests);
    if !live.is_empty() {
        data["tests_on_live_data"] = json!(
            live.iter()
                .map(|(f, why)| format!("{f}: {why}"))
                .collect::<Vec<_>>()
        );
    }
    // Brief: the commands already name the tests.
    if detail == Detail::Full || tests.len() <= 15 {
        data["tests"] = json!(tests);
    } else {
        data["tests_note"] = json!(format!(
            "{} test files, named in `run`; pass detail: \"full\" for the list",
            tests.len()
        ));
    }
    if browser.tests.len() > 5 && !browser.top.is_empty() {
        data["browser_tests_top"] = json!(
            browser
                .top
                .iter()
                .map(|(t, why)| format!("{t}  ({why})"))
                .collect::<Vec<_>>()
        );
        let specs: Vec<String> = browser.top.iter().map(|(t, _)| t.clone()).collect();
        if let Some(cmd) = test_commands(project, &specs, &[]).into_iter().last() {
            data["run_top"] = json!(cmd);
        }
    }
    if !browser.tests.is_empty() {
        data["browser_tests"] = json!(
            browser
                .tests
                .iter()
                .take(40)
                .map(|(t, why)| format!("{t}  ({why})"))
                .collect::<Vec<_>>()
        );
    }
    if !browser.screens.is_empty() {
        let n = browser.screens.len();
        let mut shown: Vec<String> = browser.screens.iter().take(20).cloned().collect();
        if n > 20 {
            shown.push(format!("… {} more", n - 20));
        }
        data["screens"] = json!(shown);
    }
    if !browser.wider_screens.is_empty() {
        let n = browser.wider_screens.len();
        let mut shown: Vec<String> = browser.wider_screens.iter().take(20).cloned().collect();
        if n > 20 {
            shown.push(format!("… {} more", n - 20));
        }
        data["screens_wider"] = json!(shown);
    }
    if !browser.wider.is_empty() {
        let n = browser.wider.len();
        let shown = if detail == Detail::Full { n } else { 10 };
        let mut v: Vec<String> = browser
            .wider
            .iter()
            .take(shown)
            .map(|(t, why)| format!("{t}  ({why})"))
            .collect();
        if n > shown {
            v.push(format!("… {} more", n - shown));
        }
        data["browser_tests_wider"] = json!(v);
    }
    let mut notes: Vec<String> = Vec::new();
    if !browser.wider.is_empty() {
        notes.push(format!(
            "browser_tests (in `run`) open screens close to the change; the {} in browser_tests_wider reach it only through shared building blocks: run them before a release.",
            browser.wider.len()
        ));
    }
    if !browser.shells.is_empty() {
        notes.push(format!(
            "The change also reaches {}, which wraps every screen.",
            browser.shells.join(", ")
        ));
    }
    if !notes.is_empty() {
        data["browser_note"] = json!(notes.join(" "));
    }
    if !all_tests.is_empty() && tests.len() * 2 > all_tests.len() {
        data["note"] = json!(format!(
            "{} of the project's {} test files are affected: running the whole suite may be simpler.",
            tests.len(),
            all_tests.len()
        ));
    }
    if files.is_empty() {
        data["hint"] = json!("No changed files.");
    }
    if detail == Detail::Full {
        data["affected"] = json!(affected);
    }
    Ok(ApiOutput {
        data,
        next_cursor: None,
        truncated: false,
    })
}

/// Commands that run exactly `tests`, using the project's own runners.
/// `tests/Feature/Consumer/A.php … Z.php` → `tests/Feature/Consumer` when every
/// test in that folder is selected (repeated up the tree).
fn collapse(selected: &[&String], all: &[String]) -> Vec<String> {
    let mut paths: Vec<String> = selected.iter().map(|s| (*s).clone()).collect();
    loop {
        let mut dirs: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        for p in &paths {
            if let Some((d, _)) = p.rsplit_once('/') {
                *dirs.entry(d.to_owned()).or_insert(0) += 1;
            }
        }
        let mut changed = false;
        for (dir, n) in dirs {
            let prefix = format!("{dir}/");
            let in_dir = all.iter().filter(|a| a.starts_with(&prefix)).count();
            let selected_in_dir = paths
                .iter()
                .filter(|p| p.starts_with(&prefix))
                .map(|p| {
                    if all.contains(p) {
                        1
                    } else {
                        all.iter()
                            .filter(|a| a.starts_with(&format!("{p}/")))
                            .count()
                    }
                })
                .sum::<usize>();
            // Exact for small sets; for large ones a folder that is mostly
            // affected is run whole (a few extra tests, a much shorter command).
            let enough = if selected.len() > 40 {
                selected_in_dir * 4 >= in_dir * 3
            } else {
                selected_in_dir >= in_dir
            };
            if n >= 2 && in_dir > 0 && enough && dir.contains('/') {
                paths.retain(|p| !p.starts_with(&prefix));
                paths.push(dir);
                changed = true;
                break;
            }
        }
        if !changed {
            break;
        }
    }
    paths.sort();
    paths
}

/// How the project runs package binaries, from its lockfile (or the
/// workspace's, one folder up): `pnpm exec`, `yarn`, `bunx`, else `npx`.
pub fn js_exec(root: &std::path::Path) -> &'static str {
    for dir in [Some(root), root.parent()].into_iter().flatten() {
        if dir.join("pnpm-lock.yaml").is_file() {
            return "pnpm exec";
        }
        if dir.join("yarn.lock").is_file() {
            return "yarn";
        }
        if dir.join("bun.lock").is_file() || dir.join("bun.lockb").is_file() {
            return "bunx";
        }
        if dir.join("package-lock.json").is_file() {
            return "npx";
        }
    }
    "npx"
}

/// When PHP tests would run on the host only because no Compose file is
/// visible (the server sees just this folder): how to point them at Docker.
pub(crate) fn php_runner_note(project: &Project, tests: &[String]) -> Option<String> {
    (tests.iter().any(|t| t.ends_with(".php"))
        && project.config.tests.php.is_none()
        && crate::compose::service_for(&project.root, project.app.as_deref()).is_none())
    .then(|| {
        "PHP tests are shown running on this computer: no docker-compose file mounts this app in the folder codemap can see. If they run in a container, set `[tests] php = \"docker compose -f <compose file> exec -T <service> php artisan test\"` in this app's .codemap.toml, or start codemap on the parent folder that holds the compose file".to_owned()
    })
}

pub(crate) fn test_commands(
    project: &Project,
    tests: &[String],
    all_tests: &[String],
) -> Vec<String> {
    let root = &project.root;
    let cd = project
        .app
        .as_ref()
        .map(|a| format!("cd {a} && "))
        .unwrap_or_default();
    let mut out = Vec::new();
    let php: Vec<&String> = tests.iter().filter(|t| t.ends_with(".php")).collect();
    if !php.is_empty() {
        let detected = if root.join("artisan").is_file() {
            "php artisan test"
        } else if root.join("vendor/bin/pest").exists() {
            "vendor/bin/pest"
        } else {
            "vendor/bin/phpunit"
        };
        // Configured, else the Compose service that mounts the app (tests that
        // need the app's container and database), else on the host.
        let in_docker = project.config.tests.php.is_none().then(|| {
            crate::compose::service_for(root, project.app.as_deref()).map(|(svc, file)| {
                let f = if file == "docker-compose.yml" || file == "compose.yml" {
                    String::new()
                } else {
                    format!("-f {file} ")
                };
                format!("docker compose {f}exec -T {svc} {detected}")
            })
        });
        let runner = project
            .config
            .tests
            .php
            .clone()
            .or(in_docker.flatten())
            .unwrap_or_else(|| detected.to_owned());
        out.push(format!(
            "{cd}{runner} {}",
            collapse(&php, all_tests).join(" ")
        ));
    }
    let js: Vec<&String> = tests.iter().filter(|t| !t.ends_with(".php")).collect();
    if !js.is_empty() {
        let pkg: Value = std::fs::read_to_string(root.join("package.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
        let has = |name: &str| {
            ["dependencies", "devDependencies"]
                .iter()
                .any(|k| pkg[*k].get(name).is_some())
        };
        let exec = js_exec(root);
        let src = |t: &str| std::fs::read_to_string(root.join(t)).unwrap_or_default();
        let (browser, unit): (Vec<&String>, Vec<&String>) = js
            .into_iter()
            .partition(|t| crate::browser::is_browser_test(t, &src(t)));
        let (cypress, playwright): (Vec<&String>, Vec<&String>) = browser
            .into_iter()
            .partition(|t| crate::browser::is_cypress(t, &src(t)));
        if !unit.is_empty() {
            let tool = if has("vitest") {
                "vitest run"
            } else if has("jest") {
                "jest"
            } else {
                "vitest run"
            };
            let runner = match &project.config.tests.js {
                Some(c) => c.clone(),
                None => format!("{exec} {tool}"),
            };
            out.push(format!(
                "{cd}{runner} {}",
                collapse(&unit, all_tests).join(" ")
            ));
        }
        if !playwright.is_empty() {
            let runner = match &project.config.tests.browser {
                Some(c) => c.clone(),
                None => format!("{exec} playwright test"),
            };
            out.push(format!(
                "{cd}{runner} {}",
                playwright
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
        }
        if !cypress.is_empty() {
            out.push(format!(
                "{cd}{exec} cypress run --spec {}",
                cypress
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
    }
    out
}

#[cfg(test)]
mod runnable_tests {
    #[test]
    fn only_runnable_php_test_files() {
        let dir = tempfile::tempdir().unwrap();
        let w = |p: &str, t: &str| {
            let path = dir.path().join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, t).unwrap();
        };
        w(
            "tests/Feature/IntegrationTestCase.php",
            "<?php abstract class IntegrationTestCase extends TestCase { protected function order() {} }",
        );
        w(
            "tests/Feature/HelperCase.php",
            "<?php class HelperCase extends TestCase { protected function makeOrder() {} }",
        );
        w(
            "tests/Feature/FlowTest.php",
            "<?php class FlowTest extends IntegrationTestCase { public function test_it() {} }",
        );
        w(
            "tests/Feature/AttrTest.php",
            "<?php class AttrTest extends TestCase { #[Test] public function it_works() {} }",
        );
        w(
            "tests/Feature/PestTest.php",
            "<?php\nit('works', function () {});",
        );
        let ok = |f: &str| super::is_php_test_file(dir.path(), f);
        assert!(!ok("tests/Feature/IntegrationTestCase.php"));
        assert!(!ok("tests/Feature/HelperCase.php"));
        assert!(ok("tests/Feature/FlowTest.php"));
        assert!(ok("tests/Feature/AttrTest.php"));
        assert!(ok("tests/Feature/PestTest.php"));
    }
}
