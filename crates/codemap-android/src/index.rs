//! Android refresh: per-file extraction of new or changed files, then the
//! project pass (screens and translations), which needs every file.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::time::Instant;

use codemap_core::symbols;
use codemap_core::{Result, ScanReport, Store};
use rayon::prelude::*;
use rusqlite::{Connection, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::extract::{self, AndroidFacts, Composable, Extracted, NamedLine, TypeFacts};

/// Extractor name in `extractor_state`.
pub const EXTRACTOR: &str = "android";

/// Bump when per-file output changes; every Android file is then re-parsed.
pub const ANDROID_EXTRACTOR_VERSION: i64 = 3;

/// `pages.source` and `i18n_*.source` for Android apps.
pub const SOURCE: &str = "android";

const LANGS: &str = "'kotlin', 'java', 'androidxml'";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AndroidReport {
    pub detected: bool,
    pub parsed: usize,
    pub failed: usize,
    pub symbols: usize,
    pub warnings: usize,
    pub composables: usize,
    pub screens: usize,
    pub duration_ms: u64,
}

fn is_android_path(p: &str) -> bool {
    p.ends_with(".kt") || p.ends_with(".java") || p.ends_with(".xml")
}

pub fn refresh(
    store: &mut Store,
    root: &Path,
    scan: &ScanReport,
    full: bool,
) -> Result<AndroidReport> {
    let started = Instant::now();
    let has_files: bool = store.conn().query_row(
        &format!("SELECT EXISTS (SELECT 1 FROM files WHERE lang IN ({LANGS}))"),
        [],
        |r| r.get(0),
    )?;
    if !has_files {
        let conn = store.conn();
        conn.execute("DELETE FROM pages WHERE source = ?1", [SOURCE])?;
        return Ok(AndroidReport::default());
    }
    let mut report = index_pending(store, root)?;
    report.detected = true;
    let removed = scan.removed_paths.iter().any(|p| is_android_path(p));
    let never_ran: bool = store.conn().query_row(
        "SELECT NOT EXISTS (SELECT 1 FROM meta WHERE key = 'android_screens')",
        [],
        |r| r.get(0),
    )?;
    if full || report.parsed > 0 || removed || never_ran {
        let (composables, screens) = rebuild(store.conn_mut())?;
        report.composables = composables;
        report.screens = screens;
    }
    report.duration_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

type FileRef = (i64, String);

pub fn index_pending(store: &mut Store, root: &Path) -> Result<AndroidReport> {
    let started = Instant::now();
    let tx = store
        .conn_mut()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let pending: Vec<FileRef> = {
        let mut stmt = tx.prepare(&format!(
            "SELECT f.id, f.path FROM files f
             WHERE f.lang IN ({LANGS}) AND NOT EXISTS (
                 SELECT 1 FROM extractor_state e
                 WHERE e.file_id = f.id AND e.extractor = ?1 AND e.version = ?2)
             ORDER BY f.path"
        ))?;
        let rows = stmt.query_map(params![EXTRACTOR, ANDROID_EXTRACTOR_VERSION], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let parsed: Vec<(i64, String, std::io::Result<Extracted>)> = pending
        .into_par_iter()
        .map(|(id, path)| {
            let result = std::fs::read(root.join(&path)).map(|bytes| {
                let src = String::from_utf8_lossy(&bytes);
                // A bug in one file's analysis must not stop the whole index.
                std::panic::catch_unwind(|| extract::extract_file(&path, &src)).unwrap_or_else(
                    |_| Extracted {
                        facts: AndroidFacts {
                            errors: vec![NamedLine {
                                name: "codemap could not analyse this file (internal error); please report it".into(),
                                line: 1,
                            }],
                            ..AndroidFacts::default()
                        },
                        ..Extracted::default()
                    },
                )
            });
            (id, path, result)
        })
        .collect();
    let total: usize = parsed
        .iter()
        .filter_map(|(_, _, r)| r.as_ref().ok())
        .map(|x| x.symbols.len())
        .sum();
    let bulk = total >= symbols::BULK_FTS_THRESHOLD;
    if bulk {
        symbols::bulk_insert_begin(&tx)?;
    }
    let mut report = AndroidReport::default();
    {
        let mut insert_file = tx.prepare(
            "INSERT OR REPLACE INTO android_files (file_id, path, facts_json) VALUES (?1, ?2, ?3)",
        )?;
        let mut insert_ref =
            tx.prepare("INSERT INTO refs (file_id, kind, target, line) VALUES (?1, ?2, ?3, ?4)")?;
        for (file_id, path, result) in &parsed {
            let x = match result {
                Ok(x) => x,
                Err(e) => {
                    report.failed += 1;
                    tracing::warn!(path, error = %e, "could not read Android file");
                    continue;
                }
            };
            tx.execute("DELETE FROM symbols WHERE file_id = ?1", [file_id])?;
            tx.execute("DELETE FROM refs WHERE file_id = ?1", [file_id])?;
            insert_file.execute(params![file_id, path, serde_json::to_string(&x.facts)?])?;
            for s in &x.symbols {
                symbols::insert(&tx, *file_id, s)?;
            }
            for r in &x.refs {
                insert_ref.execute(params![file_id, r.kind, r.target, r.line])?;
            }
            // The Kotlin grammar misses some valid syntax, so parse errors are
            // kept in the facts only (the compiler reports real ones).
            report.warnings += x.facts.errors.len();
            tx.execute(
                "INSERT OR REPLACE INTO extractor_state (file_id, extractor, version) VALUES (?1, ?2, ?3)",
                params![file_id, EXTRACTOR, ANDROID_EXTRACTOR_VERSION],
            )?;
            report.parsed += 1;
            report.symbols += x.symbols.len();
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
            "android extraction finished"
        );
    }
    Ok(report)
}

/// One Android file's facts, as stored.
pub struct AndroidFile {
    pub file_id: i64,
    pub path: String,
    pub facts: AndroidFacts,
}

pub fn load_files(conn: &Connection) -> Result<Vec<AndroidFile>> {
    let mut stmt =
        conn.prepare_cached("SELECT file_id, path, facts_json FROM android_files ORDER BY path")?;
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
        let Ok(facts) = serde_json::from_str(&json) else {
            continue;
        };
        out.push(AndroidFile {
            file_id,
            path,
            facts,
        });
    }
    Ok(out)
}

/// The project's composables by name → (composable, file). The first wins
/// when two files declare the same name.
pub fn composables(files: &[AndroidFile]) -> HashMap<&str, (&Composable, &str)> {
    let mut out = HashMap::new();
    for f in files {
        for c in &f.facts.composables {
            out.entry(c.name.as_str()).or_insert((c, f.path.as_str()));
        }
    }
    out
}

/// The class a manifest name points at: `.MainActivity`, `.ui.Main`, or a
/// full name.
pub fn manifest_class<'a>(
    files: &'a [AndroidFile],
    name: &str,
) -> Option<(&'a TypeFacts, &'a str)> {
    let all = || {
        files
            .iter()
            .flat_map(|f| f.facts.types.iter().map(move |t| (t, f.path.as_str())))
    };
    if let Some(hit) = all().find(|(t, _)| t.fqn == name) {
        return Some(hit);
    }
    let tail = if name.starts_with('.') {
        name.to_owned()
    } else {
        format!(".{name}")
    };
    all()
        .filter(|(t, _)| t.fqn.ends_with(&tail))
        .min_by_key(|(t, _)| t.fqn.len())
}

/// The screen a list of calls shows: the first project composable that is not
/// a theme wrapper (`AppTheme { Root() }` → `Root`).
fn pick<'a>(
    candidates: &[String],
    comps: &HashMap<&str, (&'a Composable, &'a str)>,
) -> Option<String> {
    let project: Vec<&String> = candidates
        .iter()
        .filter(|c| comps.contains_key(c.as_str()))
        .collect();
    project
        .iter()
        .find(|c| !c.ends_with("Theme"))
        .or(project.first())
        .map(|c| (*c).clone())
}

/// Rebuilds the `android` pages and translations from every file's facts.
fn rebuild(conn: &mut Connection) -> Result<(usize, usize)> {
    let files = load_files(conn)?;
    let comps = composables(&files);
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute("DELETE FROM pages WHERE source = ?1", [SOURCE])?;
    let mut n = 0;
    {
        let mut ins = tx.prepare(
            "INSERT INTO pages (route_path, name, file, source, defined_in, parent, meta)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        // What each activity shows through `setContent { … }`.
        let mut content: HashMap<&str, String> = HashMap::new();
        for f in &files {
            for s in f.facts.screens.iter().filter(|s| s.kind == "content") {
                if let (Some(owner), Some(view)) = (&s.owner, pick(&s.candidates, &comps)) {
                    content.insert(owner.as_str(), view);
                }
            }
        }
        let mut listed: BTreeSet<String> = BTreeSet::new();
        for f in &files {
            for c in &f.facts.manifest {
                let class = manifest_class(&files, &c.name);
                let short = c.name.rsplit('.').next().unwrap_or(&c.name).to_owned();
                let shows = class.and_then(|(t, _)| content.get(t.name.as_str()).cloned());
                if let Some((t, _)) = class {
                    listed.insert(t.name.clone());
                }
                let meta = serde_json::json!({
                    "via": "AndroidManifest.xml",
                    "launcher": c.launcher,
                    "exported": c.exported,
                    "shows": shows,
                    "class": class.map(|(t, _)| t.fqn.clone()),
                })
                .to_string();
                ins.execute(params![
                    c.kind,
                    short,
                    class.map(|(_, p)| p.to_owned()),
                    SOURCE,
                    format!("{}:{}", f.path, c.line),
                    Option::<String>::None,
                    meta,
                ])?;
                n += 1;
            }
        }
        // `when` branches: a screen switch when two or more show composables.
        let mut groups: BTreeMap<(&str, u32), Vec<(&extract::Screen, String)>> = BTreeMap::new();
        for f in &files {
            for s in &f.facts.screens {
                let Some(view) = pick(&s.candidates, &comps) else {
                    continue;
                };
                match s.kind.as_str() {
                    "switch" => groups
                        .entry((f.path.as_str(), s.group.unwrap_or(s.line)))
                        .or_default()
                        .push((s, view)),
                    "content" => {
                        if s.owner.as_ref().is_none_or(|o| !listed.contains(o)) {
                            insert(&mut ins, f, s, &view, &comps)?;
                            n += 1;
                        }
                    }
                    _ => {
                        insert(&mut ins, f, s, &view, &comps)?;
                        n += 1;
                    }
                }
            }
        }
        let by_path: HashMap<&str, &AndroidFile> =
            files.iter().map(|f| (f.path.as_str(), f)).collect();
        for ((path, _), entries) in groups {
            // A `when` that shows the same composable in each branch
            // (`Message(…)` with other text) switches content, not screens.
            let distinct: BTreeSet<&str> = entries.iter().map(|(_, v)| v.as_str()).collect();
            if distinct.len() < 2 {
                continue;
            }
            for (s, view) in entries {
                insert(&mut ins, by_path[path], s, &view, &comps)?;
                n += 1;
            }
        }
        // Fragments (View-based apps).
        for f in &files {
            for t in &f.facts.types {
                if t.kind == "class"
                    && t.supertypes.iter().any(|s| s.ends_with("Fragment"))
                    && !listed.contains(&t.name)
                {
                    ins.execute(params![
                        "fragment",
                        t.name,
                        f.path,
                        SOURCE,
                        format!("{}:{}", f.path, t.line),
                        Option::<String>::None,
                        serde_json::json!({"via": t.supertypes.first()}).to_string(),
                    ])?;
                    n += 1;
                }
            }
        }
    }
    // Translations: `translatable="false"` strings count in every language.
    tx.execute("DELETE FROM i18n_keys WHERE source = ?1", [SOURCE])?;
    tx.execute("DELETE FROM i18n_uses WHERE source = ?1", [SOURCE])?;
    {
        let locales: BTreeSet<&str> = files
            .iter()
            .flat_map(|f| f.facts.locales.iter().map(|l| l.locale.as_str()))
            .collect();
        let mut key = tx.prepare(
            "INSERT INTO i18n_keys (file_id, source, locale, key) VALUES (?1, 'android', ?2, ?3)",
        )?;
        let mut used = tx.prepare(
            "INSERT INTO i18n_uses (file_id, source, key, line, dynamic, guarded) VALUES (?1, 'android', ?2, ?3, 0, 0)",
        )?;
        for f in &files {
            for l in &f.facts.locales {
                for k in &l.keys {
                    key.execute(params![f.file_id, l.locale, k])?;
                }
            }
            for k in &f.facts.untranslatable {
                for l in locales
                    .iter()
                    .filter(|l| !f.facts.locales.iter().any(|x| x.locale == **l))
                {
                    key.execute(params![f.file_id, l, k])?;
                }
            }
            for u in &f.facts.i18n_uses {
                used.execute(params![f.file_id, u.key, u.line])?;
            }
        }
    }
    // The shared view model.
    {
        let mut vs = Vec::new();
        let mut uses = Vec::new();
        for f in &files {
            for c in &f.facts.composables {
                vs.push(codemap_core::views::View {
                    file_id: f.file_id,
                    name: c.name.clone(),
                    path: f.path.clone(),
                    line: c.line,
                    kind: "composable".into(),
                });
                for call in c.calls.iter().filter(|x| x.name != c.name) {
                    if let Some((_, to_path)) = comps.get(call.name.as_str()) {
                        uses.push(codemap_core::views::Use {
                            from_path: f.path.clone(),
                            from_name: Some(c.name.clone()),
                            to_name: call.name.clone(),
                            to_path: Some((*to_path).to_owned()),
                            line: call.line,
                        });
                    }
                }
            }
            for s in f.facts.screens.iter().filter(|s| s.kind == "content") {
                for name in &s.candidates {
                    if let Some((_, to_path)) = comps.get(name.as_str()) {
                        uses.push(codemap_core::views::Use {
                            from_path: f.path.clone(),
                            from_name: s.owner.clone(),
                            to_name: name.clone(),
                            to_path: Some((*to_path).to_owned()),
                            line: s.line,
                        });
                    }
                }
            }
        }
        codemap_core::views::replace(&tx, "compose", &vs, &uses)?;
    }
    tx.execute(
        "INSERT INTO meta (key, value) VALUES ('android_screens', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [n.to_string()],
    )?;
    tx.commit()?;
    Ok((comps.len(), n))
}

fn insert(
    ins: &mut rusqlite::Statement,
    f: &AndroidFile,
    s: &extract::Screen,
    view: &str,
    comps: &HashMap<&str, (&Composable, &str)>,
) -> Result<()> {
    let meta = serde_json::json!({
        "via": s.via,
        "title": s.title,
    })
    .to_string();
    ins.execute(params![
        s.kind,
        view,
        comps.get(view).map(|(_, p)| *p),
        SOURCE,
        format!("{}:{}", f.path, s.line),
        s.owner,
        meta,
    ])?;
    Ok(())
}
