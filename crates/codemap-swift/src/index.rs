//! Swift refresh: per-file extraction of new or changed files, then the
//! screens pass, which needs every file's views.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use codemap_core::symbols;
use codemap_core::{Result, ScanReport, Store};
use rayon::prelude::*;
use rusqlite::{Connection, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::extract::{self, Extracted, NamedLine, SwiftFacts};

/// Extractor name in `extractor_state` and `warnings.adapter`.
pub const EXTRACTOR: &str = "swift";

/// Bump when per-file output changes; every Swift file is then re-parsed.
pub const SWIFT_EXTRACTOR_VERSION: i64 = 2;

/// `pages.source` and `i18n_*.source` for Swift apps.
pub const SOURCE: &str = "swift";

/// `pages.source` for SwiftUI screens.
pub const PAGES_SOURCE: &str = "swiftui";

const LANGS: &str = "'swift', 'strings', 'xcstrings'";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwiftReport {
    pub detected: bool,
    pub parsed: usize,
    pub failed: usize,
    pub symbols: usize,
    pub warnings: usize,
    pub views: usize,
    pub screens: usize,
    pub duration_ms: u64,
}

fn is_swift_path(p: &str) -> bool {
    p.ends_with(".swift") || p.ends_with(".strings") || p.ends_with(".xcstrings")
}

pub fn refresh(
    store: &mut Store,
    root: &Path,
    scan: &ScanReport,
    full: bool,
) -> Result<SwiftReport> {
    let started = Instant::now();
    let has_files: bool = store.conn().query_row(
        &format!("SELECT EXISTS (SELECT 1 FROM files WHERE lang IN ({LANGS}))"),
        [],
        |r| r.get(0),
    )?;
    if !has_files {
        // Files removed since the last refresh take their rows with them
        // (ON DELETE CASCADE); only the screens are left to clear.
        store
            .conn()
            .execute("DELETE FROM pages WHERE source = ?1", [PAGES_SOURCE])?;
        return Ok(SwiftReport::default());
    }
    let mut report = index_pending(store, root)?;
    report.detected = true;
    let removed = scan.removed_paths.iter().any(|p| is_swift_path(p));
    let never_ran: bool = store.conn().query_row(
        "SELECT NOT EXISTS (SELECT 1 FROM meta WHERE key = 'swift_screens')",
        [],
        |r| r.get(0),
    )?;
    if full || report.parsed > 0 || removed || never_ran {
        let (views, screens) = rebuild_screens(store.conn_mut())?;
        report.views = views;
        report.screens = screens;
    }
    report.duration_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

type FileRef = (i64, String);

pub fn index_pending(store: &mut Store, root: &Path) -> Result<SwiftReport> {
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
        let rows = stmt.query_map(params![EXTRACTOR, SWIFT_EXTRACTOR_VERSION], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let parsed: Vec<(i64, String, std::io::Result<Extracted>)> = pending
        .into_par_iter()
        .map(|(id, path)| {
            let result = std::fs::read(root.join(&path)).map(|bytes| {
                let src = decode(&bytes);
                // A bug in one file's analysis must not stop the whole index.
                std::panic::catch_unwind(|| extract::extract_file(&path, &src)).unwrap_or_else(
                    |_| Extracted {
                        facts: SwiftFacts {
                            errors: vec![NamedLine {
                                name: "codemap could not analyse this file (internal error); please report it".into(),
                                line: 1,
                            }],
                            ..SwiftFacts::default()
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
    let mut report = SwiftReport::default();
    {
        let mut insert_file = tx.prepare(
            "INSERT OR REPLACE INTO swift_files (file_id, path, facts_json) VALUES (?1, ?2, ?3)",
        )?;
        let mut insert_ref =
            tx.prepare("INSERT INTO refs (file_id, kind, target, line) VALUES (?1, ?2, ?3, ?4)")?;
        let mut insert_key = tx.prepare(
            "INSERT INTO i18n_keys (file_id, source, locale, key) VALUES (?1, 'swift', ?2, ?3)",
        )?;
        let mut insert_use = tx.prepare(
            "INSERT INTO i18n_uses (file_id, source, key, line, dynamic, guarded) VALUES (?1, 'swift', ?2, ?3, 0, ?4)",
        )?;
        for (file_id, path, result) in &parsed {
            let x = match result {
                Ok(x) => x,
                Err(e) => {
                    report.failed += 1;
                    tracing::warn!(path, error = %e, "could not read Swift file");
                    continue;
                }
            };
            for sql in [
                "DELETE FROM symbols WHERE file_id = ?1",
                "DELETE FROM refs WHERE file_id = ?1",
                "DELETE FROM i18n_keys WHERE file_id = ?1 AND source = 'swift'",
                "DELETE FROM i18n_uses WHERE file_id = ?1 AND source = 'swift'",
            ] {
                tx.execute(sql, [file_id])?;
            }
            insert_file.execute(params![file_id, path, serde_json::to_string(&x.facts)?])?;
            for s in &x.symbols {
                symbols::insert(&tx, *file_id, s)?;
            }
            for r in &x.refs {
                insert_ref.execute(params![file_id, r.kind, r.target, r.line])?;
            }
            for l in &x.facts.locales {
                for k in &l.keys {
                    insert_key.execute(params![file_id, l.locale, k])?;
                }
            }
            for u in &x.facts.i18n_uses {
                insert_use.execute(params![file_id, u.key, u.line, u.guarded])?;
            }
            // The Swift grammar misses some valid syntax (`@_silgen_name`,
            // regex literals), so parse errors are kept in the facts only.
            report.warnings += x.facts.errors.len();
            tx.execute(
                "INSERT OR REPLACE INTO extractor_state (file_id, extractor, version) VALUES (?1, ?2, ?3)",
                params![file_id, EXTRACTOR, SWIFT_EXTRACTOR_VERSION],
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
            "swift extraction finished"
        );
    }
    Ok(report)
}

/// `.strings` files are often UTF-16 (older Xcode projects).
fn decode(bytes: &[u8]) -> String {
    let utf16 = |le: bool| {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| {
                if le {
                    u16::from_le_bytes([c[0], c[1]])
                } else {
                    u16::from_be_bytes([c[0], c[1]])
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    match bytes {
        [0xFF, 0xFE, ..] => utf16(true),
        [0xFE, 0xFF, ..] => utf16(false),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// One Swift file's facts, as stored.
pub struct SwiftFile {
    pub file_id: i64,
    pub path: String,
    pub facts: SwiftFacts,
}

pub fn load_files(conn: &Connection) -> Result<Vec<SwiftFile>> {
    let mut stmt =
        conn.prepare_cached("SELECT file_id, path, facts_json FROM swift_files ORDER BY path")?;
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
        out.push(SwiftFile {
            file_id,
            path,
            facts,
        });
    }
    Ok(out)
}

/// SwiftUI views, and UIKit / AppKit views wrapped for SwiftUI.
const VIEW_PROTOCOLS: &[&str] = &[
    "View",
    "UIViewRepresentable",
    "UIViewControllerRepresentable",
    "NSViewRepresentable",
    "NSViewControllerRepresentable",
];

/// SwiftUI views: types that conform to `View` in their declaration or an
/// extension, by name → (file, line).
pub fn views(files: &[SwiftFile]) -> HashMap<String, (String, u32)> {
    let mut declared: HashMap<&str, (&str, u32)> = HashMap::new();
    let mut is_view: HashSet<&str> = HashSet::new();
    for f in files {
        for t in &f.facts.types {
            if t.kind != "extension" {
                declared.insert(&t.name, (&f.path, t.line));
            }
            if t.conforms
                .iter()
                .any(|c| VIEW_PROTOCOLS.contains(&c.as_str()))
            {
                is_view.insert(&t.name);
            }
        }
    }
    is_view
        .into_iter()
        .filter_map(|n| {
            declared
                .get(n)
                .map(|(p, l)| (n.to_owned(), ((*p).to_owned(), *l)))
        })
        .collect()
}

/// Short name of a possibly nested type: `ContentView.Row` → `Row`.
fn short(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// Rebuilds the `swiftui` pages from every file's screens.
fn rebuild_screens(conn: &mut Connection) -> Result<(usize, usize)> {
    let files = load_files(conn)?;
    let views = views(&files);
    // Nested views are called by their short name inside their parent.
    let mut by_short: HashMap<&str, &str> = HashMap::new();
    for name in views.keys() {
        by_short.entry(short(name)).or_insert(name);
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute("DELETE FROM pages WHERE source = ?1", [PAGES_SOURCE])?;
    let mut n = 0;
    {
        let mut ins = tx.prepare(
            "INSERT INTO pages (route_path, name, file, source, defined_in, parent, meta)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for f in &files {
            for s in &f.facts.screens {
                let view = s
                    .candidates
                    .iter()
                    .find_map(|c| views.get_key_value(c.as_str()).map(|(k, _)| k.as_str()))
                    .or_else(|| {
                        s.candidates
                            .iter()
                            .find_map(|c| by_short.get(c.as_str()).copied())
                    });
                // A popover of a bare `Text` is not a screen.
                let Some(v) = view else { continue };
                let (name, file) = (v.to_owned(), views.get(v).map(|(p, _)| p.clone()));
                let meta = serde_json::json!({
                    "via": s.via,
                    "title": s.title,
                })
                .to_string();
                ins.execute(params![
                    s.kind,
                    name,
                    file,
                    PAGES_SOURCE,
                    format!("{}:{}", f.path, s.line),
                    s.owner,
                    meta,
                ])?;
                n += 1;
            }
        }
    }
    tx.execute(
        "INSERT INTO meta (key, value) VALUES ('swift_screens', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [n.to_string()],
    )?;
    tx.commit()?;
    Ok((views.len(), n))
}
