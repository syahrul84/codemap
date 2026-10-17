//! Pre-release check: one call that returns only problems (`verify` with
//! `release_check: true`). Each section reuses the tool that knows it:
//! API calls (api_usage), translations, unused code added on the branch,
//! migrations not run on the configured databases, config / env keys, and
//! settings read without a default.

use codemap_core::Store;
use codemap_core::envelope::Detail;
use serde_json::{Value, json};

use crate::tools::{Project, ToolError};
use crate::usages::is_test_path;

type R<T> = Result<T, ToolError>;

/// Folders whose classes Laravel (or Nuxt) finds by itself: "nothing uses it"
/// says nothing there.
const DISCOVERED: &[&str] = &[
    "app/Providers/",
    "app/Console/",
    "app/Policies/",
    "app/Listeners/",
    "app/Observers/",
    "app/Filament/",
    "app/Http/Middleware/",
    "app/Livewire/",
    "app/View/Components/",
    "database/",
    "routes/",
    "config/",
    "bootstrap/",
    "lang/",
    "resources/views/",
    "pages/",
    "layouts/",
    "plugins/",
    "middleware/",
    "server/",
    "app/pages/",
    "app/layouts/",
    "app/plugins/",
    "app/middleware/",
    "src/views/",
    "src/router/",
];

fn discovered(path: &str) -> bool {
    DISCOVERED.iter().any(|d| path.starts_with(d))
        || path.ends_with("app.vue")
        || path.ends_with("App.vue")
        || path.ends_with("error.vue")
        || path.ends_with("main.ts")
        || path.ends_with("main.js")
        || path.ends_with(".config.ts")
        || path.ends_with(".config.js")
        || path.ends_with(".d.ts")
}

/// Code in `files` that nothing uses: components no template renders and no
/// file imports, exported functions never imported or called, PHP classes
/// never referenced.
fn unused_in(store: &Store, root: &std::path::Path, files: &[String]) -> R<Vec<String>> {
    let conn = store.conn();
    let count = |sql: &str, p: &[&dyn rusqlite::ToSql]| -> i64 {
        conn.query_row(sql, p, |r| r.get(0)).unwrap_or(1)
    };
    let mut out = Vec::new();
    let wanted: std::collections::HashSet<&str> = files.iter().map(String::as_str).collect();
    let mut by_file: std::collections::HashMap<String, Vec<codemap_core::symbols::SymbolRecord>> =
        std::collections::HashMap::new();
    for s in codemap_core::symbols::all(conn)? {
        if wanted.contains(s.file.as_str()) {
            by_file.entry(s.file.clone()).or_default().push(s);
        }
    }
    for f in files {
        if is_test_path(f) || discovered(f) {
            continue;
        }
        let prefix = format!("{f}#");
        let prefix_end = crate::usages::range_end(&prefix);
        if f.ends_with(".vue") {
            let rendered = count(
                "SELECT COUNT(*) FROM component_uses WHERE to_path = ?1",
                &[f],
            );
            let imported = count(
                "SELECT COUNT(*) FROM refs WHERE kind = 'import'
                 AND target >= ?1 COLLATE NOCASE AND target < ?2 COLLATE NOCASE",
                &[&prefix, &prefix_end],
            );
            let page: i64 = count("SELECT COUNT(*) FROM pages WHERE file = ?1", &[f]);
            if rendered + imported + page == 0 {
                out.push(format!("{f}: component nothing renders or imports"));
            }
            continue;
        }
        let syms = by_file.remove(f.as_str()).unwrap_or_default();
        let text = std::fs::read_to_string(root.join(f)).unwrap_or_default();
        for s in syms {
            if f.ends_with(".php") {
                if !s.kind.is_class_like() {
                    continue;
                }
                let members = format!("{}::", s.fqn);
                let members_end = crate::usages::range_end(&members);
                // Two indexed counts, not one `OR` (that scans every reference).
                let used = count(
                    "SELECT COUNT(*) FROM refs r JOIN files fl ON fl.id = r.file_id
                     WHERE fl.path != ?2 AND r.kind = 'class' AND r.target = ?1 COLLATE NOCASE",
                    &[&s.fqn, f],
                ) + count(
                    "SELECT COUNT(*) FROM refs r JOIN files fl ON fl.id = r.file_id
                     WHERE fl.path != ?3 AND r.kind = 'call'
                       AND r.target >= ?1 COLLATE NOCASE AND r.target < ?2 COLLATE NOCASE",
                    &[&members, &members_end, f],
                );
                let routed = count(
                    "SELECT COUNT(*) FROM routes WHERE controller_fqn = ?1 COLLATE NOCASE",
                    &[&s.fqn],
                );
                if used + routed == 0 {
                    out.push(format!("{f}: class {} is not used anywhere", s.fqn));
                }
            } else if matches!(s.kind.as_str(), "function" | "composable" | "store") {
                let used = count(
                    "SELECT COUNT(*) FROM refs WHERE (kind = 'import' AND target = ?1)
                       OR (kind = 'js_call' AND target = ?2)",
                    &[&format!("{f}#{}", s.name), &s.name],
                );
                // Mentioned again in its own file (called, or passed by name).
                let local = word_count(&text, &s.name) > 1;
                if used == 0 && !local {
                    out.push(format!("{f}: {} {} is not used anywhere", s.kind, s.name));
                }
            }
        }
    }
    Ok(out)
}

/// Laravel's attribute lookup: an exact key, or a pattern with `*` segments.
fn attribute_named(names: &std::collections::HashSet<String>, field: &str) -> bool {
    if names.contains(field) {
        return true;
    }
    let f: Vec<&str> = field.split('.').collect();
    names.iter().any(|n| {
        let p: Vec<&str> = n.split('.').collect();
        p.len() == f.len()
            && p.iter()
                .zip(&f)
                .all(|(a, b)| *a == "*" || *b == "*" || a == b)
    })
}

/// Form Request fields without a name in a shipped language's
/// `validation.attributes` (when that list is not empty), as `(broken, unnamed)`:
/// broken = a nested field whose last word *is* named (`translations.*.description`
/// while `description` is): the name is clearly meant, but Laravel looks up the
/// full key, so errors show `translations.ms.description`; unnamed = the rest.
/// `None`: nothing to check.
#[allow(clippy::type_complexity)]
pub(crate) fn field_names_missing(
    store: &Store,
    shipped: &[String],
) -> R<Option<(Vec<String>, Vec<String>)>> {
    let conn = store.conn();
    let mut by_locale: std::collections::BTreeMap<String, std::collections::HashSet<String>> =
        std::collections::BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT locale, key FROM i18n_keys WHERE source = 'laravel' AND key LIKE 'validation.attributes.%'",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (l, k) = row?;
            let name = k.trim_start_matches("validation.attributes.").to_owned();
            by_locale.entry(l).or_default().insert(name);
        }
    }
    // Every language the app has, even one naming no fields (Laravel's own
    // `en` has `'attributes' => []`, or no validation file at all).
    {
        let mut stmt =
            conn.prepare("SELECT DISTINCT locale FROM i18n_keys WHERE source = 'laravel'")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        for l in rows {
            by_locale.entry(l?).or_default();
        }
    }
    let by_locale: Vec<(String, std::collections::HashSet<String>)> = by_locale
        .into_iter()
        .filter(|(l, _)| shipped.is_empty() || shipped.iter().any(|s| s == l))
        .collect();
    if by_locale.iter().all(|(_, names)| names.is_empty()) {
        return Ok(None);
    }
    let mut out = Vec::new();
    let mut unnamed: Vec<String> = Vec::new();
    for req in codemap_laravel::requests::load(conn, None)? {
        // A request naming its own fields (`attributes()`) is covered.
        let own = codemap_core::symbols::by_fqn(conn, &format!("{}::attributes", req.fqn))?;
        if !own.is_empty() {
            continue;
        }
        for f in &req.rules {
            if f.field.starts_with('_') || f.field == "*" {
                continue;
            }
            for (locale, names) in &by_locale {
                if !attribute_named(names, &f.field) {
                    let last = f.field.rsplit('.').next().unwrap_or(&f.field);
                    let nested = f.field.contains('.') && last != "*";
                    // The last part is named here, or (for a language naming
                    // nothing) in another shipped language: the team names this
                    // field, but the full key is missing.
                    let named_elsewhere: Vec<&str> = by_locale
                        .iter()
                        .filter(|(l, n)| {
                            l != locale && (n.contains(last) || attribute_named(n, &f.field))
                        })
                        .map(|(l, _)| l.as_str())
                        .collect();
                    let has = if names.contains(last) {
                        format!("only `{last}` has")
                    } else if !named_elsewhere.is_empty() {
                        format!("{} names `{last}`", named_elsewhere.join(", "))
                    } else {
                        String::new()
                    };
                    if !nested || has.is_empty() {
                        if !names.is_empty() {
                            let label = format!("{locale} {}", f.field);
                            if !unnamed.contains(&label) {
                                unnamed.push(label);
                            }
                        }
                        continue;
                    }
                    // `*` is whatever key the item has: a number for a list, a language
                    // code or id for a keyed map.
                    let shown = f.field.replace('*', "<key>");
                    out.push(format!(
                        "field `{}` has no name in {locale} validation.attributes ({has}): its errors show the raw path \"{}\" ({} {}:{}); add '{}' => '…' to lang/{locale}/validation.php",
                        f.field,
                        shown.replace('_', " "),
                        req.fqn.rsplit('\\').next().unwrap_or(&req.fqn),
                        req.file,
                        req.line,
                        f.field
                    ));
                }
            }
        }
    }
    Ok(Some((out, unnamed)))
}

/// The `file:line` places a problem names, when the file exists.
fn places(root: &std::path::Path, text: &str) -> Vec<(String, usize)> {
    text.split(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == ',')
        .filter_map(|tok| {
            let (file, line) = tok.rsplit_once(':')?;
            let line: usize = line.parse().ok()?;
            (file.contains('.') && root.join(file).is_file()).then(|| (file.to_owned(), line))
        })
        .collect()
}

/// A `codemap-ignore` comment on the line a problem points at, or the line above.
fn marked_in_source(root: &std::path::Path, text: &str) -> bool {
    places(root, text).into_iter().any(|(file, line)| {
        let Ok(src) = std::fs::read_to_string(root.join(&file)) else {
            return false;
        };
        let lines: Vec<&str> = src.lines().collect();
        [line.checked_sub(1), line.checked_sub(2)]
            .into_iter()
            .flatten()
            .filter_map(|i| lines.get(i))
            .any(|l| {
                // `pidx-ignore`: the name before codemap, still honoured.
                [
                    "codemap-ignore",
                    "codemap: ignore",
                    "pidx-ignore",
                    "pidx: ignore",
                ]
                .iter()
                .any(|m| l.contains(m))
            })
    })
}

/// Whole-word occurrences of an identifier.
fn word_count(text: &str, name: &str) -> usize {
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    text.match_indices(name)
        .filter(|(i, _)| {
            let before = text[..*i].chars().next_back();
            let after = text[i + name.len()..].chars().next();
            !before.is_some_and(ident) && !after.is_some_and(ident)
        })
        .count()
}

pub fn release_check(store: &Store, project: &Project, base: Option<&str>) -> R<Value> {
    let mut problems: Vec<Value> = Vec::new();
    let mut checked: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    // Low-priority findings: listed, never counted as problems.
    let mut low: Vec<String> = Vec::new();
    let push = |problems: &mut Vec<Value>, area: &str, item: String| {
        problems.push(json!(format!("{area}: {item}")));
    };

    // Frontend ↔ backend.
    let api = crate::api::api_usage(
        store,
        project,
        crate::api::ApiUsageParams {
            limit: Some(60),
            ..Default::default()
        },
    )?;
    if api.data["calls"].is_array() {
        checked.push("API calls".into());
        let mut unclear = 0;
        for c in api.data["calls"].as_array().into_iter().flatten() {
            let s = c.as_str().unwrap_or_default();
            // Only notes (a type listing a field nothing reads): low priority.
            let findings: Vec<&str> = s.split("  → ").skip(1).collect();
            if s.contains("→ unclear") {
                unclear += 1;
            } else if !findings.is_empty() && findings.iter().all(|f| f.starts_with("note:")) {
                low.push(format!("api: {s}"));
            } else {
                push(&mut problems, "api", s.to_owned());
            }
        }
        if unclear > 0 {
            notes.push(format!(
                "{unclear} API calls could not be tied to one route (api_usage status: unresolved)"
            ));
        }
    }

    // Translations: shipped locales only.
    let cats = crate::i18n::catalogs(store.conn(), &project.config.i18n.locales)?;
    if !cats.is_empty() {
        checked.push("translations".into());
    }
    for c in &cats {
        let h = c.health(20);
        for (locale, keys) in &h.missing_samples {
            for k in keys {
                push(
                    &mut problems,
                    "translation",
                    format!("{k} missing in {locale} ({})", h.source),
                );
            }
        }
        for u in &h.undefined {
            push(
                &mut problems,
                "translation",
                format!("{u} used but defined in no locale ({})", h.source),
            );
        }
        for u in &h.used_missing {
            push(&mut problems, "translation", format!("{u} ({})", h.source));
        }
    }

    // Code added on this branch that nothing uses.
    let mut branch: Option<Vec<String>> = None;
    match codemap_core::git::branch_files(&project.root, base) {
        Some((used_base, files)) => {
            branch = Some(files.clone());
            let files: Vec<String> = files
                .into_iter()
                .filter(|f| project.root.join(f).is_file())
                .collect();
            checked.push(format!(
                "unused code in {} changed files (vs {used_base})",
                files.len()
            ));
            for u in unused_in(store, &project.root, &files)? {
                push(&mut problems, "unused", u);
            }
        }
        None => notes.push("not a git repository: unused new code not checked".into()),
    }

    // What the release needs on the server, the project's file rules, and
    // mentions left behind after deletions.
    let mut deploy_steps: Vec<Value> = Vec::new();
    let mut rules: Vec<String> = Vec::new();
    let mut leftover_job: Option<std::thread::JoinHandle<Vec<crate::leftovers::Mention>>> = None;
    if let Some((_, from, changes)) = codemap_core::git::branch_changes(&project.root, base) {
        deploy_steps = crate::deploy::steps(store, project, &from, &changes);
        for (rule, files) in
            crate::deploy::rules_hit(&project.root, &project.config.rules, &changes)
        {
            let line = format!("{} ({})", rule.note.trim(), files.join(", "));
            if rule.level.as_deref() == Some("problem") {
                push(&mut problems, "rule", line);
            } else {
                rules.push(line);
            }
        }
        let gone = crate::leftovers::gone(&project.root, &from, &changes);
        if !gone.is_empty() {
            checked.push(format!("mentions of {} deleted names", gone.len()));
        }
        // Reading every text file takes longest: it runs while the other
        // checks do.
        let (root, gone) = (project.root.clone(), gone.clone());
        leftover_job = Some(std::thread::spawn(move || {
            crate::leftovers::mentions(&root, &gone)
        }));
    }

    // Tests that only pass with the right data already in the database.
    let all_files: Vec<String> = store.list_files()?.into_iter().map(|f| f.path).collect();
    let live = crate::livedata::find(&project.root, &all_files);
    if !live.is_empty() {
        checked.push("tests that depend on live data".into());
        let examples: Vec<String> = live
            .iter()
            .take(8)
            .map(|(f, why)| format!("{f} ({why})"))
            .collect();
        low.push(format!(
            "tests: {} tests check data they never create, so they pass only while the database holds the right records (e.g. {})",
            live.len(),
            examples.join("; ")
        ));
    }

    // Laravel: migrations, config, settings.
    let files: Vec<String> = store.list_files()?.into_iter().map(|f| f.path).collect();
    let laravel = files.iter().any(|f| f.starts_with("database/migrations/"))
        || project.root.join("artisan").is_file();
    if laravel {
        let m = codemap_laravel::migration_status::status(
            &project.root,
            &files,
            &project.config.databases,
            false,
        );
        if m["configured"] == true {
            checked.push("migrations".into());
            for d in m["databases"].as_array().into_iter().flatten() {
                let name = d["name"].as_str().unwrap_or("?");
                match d["status"].as_str().unwrap_or("") {
                    "pending" => {
                        let list: Vec<&str> = d["pending"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|x| x.as_str())
                            .take(10)
                            .collect();
                        push(
                            &mut problems,
                            "migrations",
                            format!(
                                "{} not run on {name}: {}",
                                d["pending_total"],
                                list.join(", ")
                            ),
                        );
                    }
                    "up_to_date" => {}
                    other => notes.push(format!(
                        "database {name}: {other} ({})",
                        d["detail"].as_str().unwrap_or("")
                    )),
                }
            }
        } else if m["migration_files"].as_u64().unwrap_or(0) > 0 {
            notes.push(
                "migrations NOT checked against a database (no connection given): add `\"-e\", \"CODEMAP_DB_LOCAL\"` (and `CODEMAP_DB_STAGING`) to the docker args of this MCP server with the URL in its `env`, plus `--network <compose network>` when the database runs in Docker; codemap only reads the migrations table, read-only"
                    .into(),
            );
        }
        // Validation messages name each field from `validation.attributes`;
        // a field missing from a shipped language's list shows its raw key.
        if let Some((broken, unnamed)) = field_names_missing(store, &project.config.i18n.locales)? {
            checked.push("validation field names".into());
            for p in broken {
                push(&mut problems, "translation", p);
            }
            if !unnamed.is_empty() {
                let n = unnamed.len();
                let examples: Vec<String> = unnamed.into_iter().take(8).collect();
                low.push(format!(
                    "translation: {n} form fields have no name in a shipped language's validation.attributes, so their errors show the field key (e.g. {})",
                    examples.join(", ")
                ));
            }
        }
        checked.push("config and env keys".into());
        for (what, item, at) in codemap_laravel::settings::config_problems(
            &project.root,
            &files,
            store.conn(),
            branch.as_deref(),
        )? {
            push(&mut problems, "config", format!("{what}: {item} {at}"));
        }
        let s = codemap_laravel::settings::describe(store, &project.root, None, Detail::Full)?;
        if s["settings_total"].as_u64().unwrap_or(0) > 0 {
            checked.push("settings".into());
        }
        for e in s["settings"].as_array().into_iter().flatten() {
            let no_default = e["defaults"].as_array().is_some_and(|d| d.is_empty());
            let read = e["read_at"].as_array().is_some_and(|r| !r.is_empty());
            if no_default && read {
                let key = e["key"].as_str().unwrap_or("?");
                let at = e["read_at"][0].as_str().unwrap_or("");
                // Saved by the app or an admin page: empty until then, by design.
                let saved = e["written_at"]
                    .as_array()
                    .and_then(|w| w.first())
                    .and_then(|w| w.as_str())
                    .map(str::to_owned)
                    .or_else(|| {
                        e["admin_pages"]
                            .as_array()
                            .and_then(|p| p.first())
                            .and_then(|p| p.as_str())
                            .map(|p| format!("admin page {p}"))
                    });
                match saved {
                    Some(w) => low.push(format!(
                        "setting: {key} has no default, so it is empty until saved ({w}); make sure {at} copes with no value"
                    )),
                    None => push(
                        &mut problems,
                        "setting",
                        format!("{key} is read without a default and nothing in the app saves it ({at})"),
                    ),
                }
            }
        }
    }

    // House copy rules (banned words per locale).
    let copy = crate::copy_rules::check(store, project)?;
    if let Some(found) = copy {
        checked.push("copy rules".into());
        for f in found {
            push(&mut problems, "copy", f);
        }
    }

    // Mentions of deleted names, searched in the background.
    {
        for (name, why, places) in leftover_job
            .take()
            .and_then(|j| j.join().ok())
            .unwrap_or_default()
        {
            let (hard, soft): (Vec<_>, Vec<_>) = places
                .into_iter()
                .partition(|(kind, _)| matches!(*kind, "code" | "test"));
            let list = |v: &[(&str, String)]| {
                v.iter()
                    .map(|(k, at)| format!("{at} ({k})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            if !hard.is_empty() {
                push(
                    &mut problems,
                    "leftover",
                    format!(
                        "`{name}` is still named after it was removed ({why}): {}",
                        list(&hard)
                    ),
                );
            }
            if !soft.is_empty() {
                low.push(format!(
                    "leftover: `{name}` is still named in comments or docs ({why}): {}",
                    list(&soft)
                ));
            }
        }
    }
    // Marked as intended: `codemap-ignore` on (or above) the line, or `[release] ignore`.
    let mut ignored: Vec<String> = Vec::new();
    problems.retain(|p| {
        let text = p.as_str().unwrap_or_default();
        let by_config = project
            .config
            .release
            .ignore
            .iter()
            .any(|i| !i.is_empty() && text.contains(i.as_str()));
        if by_config || marked_in_source(&project.root, text) {
            ignored.push(text.to_owned());
            false
        } else {
            true
        }
    });
    let mut v = json!({
        "clean": problems.is_empty(),
        "problems_total": problems.len(),
        "problems": problems.into_iter().take(100).collect::<Vec<_>>(),
        "checked": checked,
    });
    if !v["clean"].as_bool().unwrap_or(true) {
        v["to_mark_intended"] = json!(
            "a problem that is on purpose: add a `codemap-ignore` comment (e.g. `// codemap-ignore: why`) on the line it points at or the line above, or add text from the problem to `[release] ignore = [\"…\"]` in .codemap.toml; it then moves to `ignored`"
        );
    }
    if !deploy_steps.is_empty() {
        v["release_steps"] = json!(deploy_steps);
    }
    if !rules.is_empty() {
        v["rules"] = json!(rules);
    }
    if !notes.is_empty() {
        v["notes"] = json!(notes);
    }
    if !ignored.is_empty() {
        v["ignored_total"] = json!(ignored.len());
        v["ignored"] = json!(ignored.into_iter().take(50).collect::<Vec<_>>());
    }
    if !low.is_empty() {
        v["low_priority"] = json!(low.into_iter().take(50).collect::<Vec<_>>());
    }
    Ok(v)
}
