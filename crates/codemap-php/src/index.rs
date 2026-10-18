//! Writes extracted PHP symbols into the index for files that are pending:
//! new or modified files, or files parsed by an older extractor version.

use std::path::Path;
use std::time::Instant;

use codemap_core::scan::scan;
use codemap_core::store::Store;
use codemap_core::{Config, Result, ScanOptions, ScanReport, symbols};
use rayon::prelude::*;
use rusqlite::{TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::extract::{ParsedFile, extract};

/// Extractor name in `extractor_state` and `warnings.adapter`.
pub const EXTRACTOR: &str = "php";

/// Bump when extraction output changes; every PHP file is then re-parsed.
pub const PHP_EXTRACTOR_VERSION: i64 = 20;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhpReport {
    pub parsed: usize,
    pub symbols: usize,
    pub warnings: usize,
    /// Files that could not be read (e.g. deleted since the scan); retried next time.
    pub failed: usize,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshReport {
    pub scan: ScanReport,
    pub php: PhpReport,
}

impl RefreshReport {
    /// Files whose facts changed in this refresh.
    pub fn reindexed_files(&self) -> usize {
        self.scan.reparsed + self.scan.removed
    }
}

/// Scans for changed files, then extracts symbols from pending PHP files.
pub fn refresh(
    store: &mut Store,
    root: &Path,
    config: &Config,
    opts: ScanOptions,
) -> Result<RefreshReport> {
    let scan = scan(store, root, config, opts)?;
    let php = index_pending(store, root)?;
    Ok(RefreshReport { scan, php })
}

pub fn index_pending(store: &mut Store, root: &Path) -> Result<PhpReport> {
    let started = Instant::now();
    // Hold the write lock from reading the pending list to the commit, so two
    // processes do not extract the same files concurrently.
    let tx = store
        .conn_mut()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let pending: Vec<(i64, String)> = {
        let mut stmt = tx.prepare(
            "SELECT f.id, f.path FROM files f
             WHERE f.lang = 'php' AND NOT EXISTS (
                 SELECT 1 FROM extractor_state e
                 WHERE e.file_id = f.id AND e.extractor = ?1 AND e.version = ?2)
             ORDER BY f.path",
        )?;
        let rows = stmt.query_map(params![EXTRACTOR, PHP_EXTRACTOR_VERSION], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };

    let parsed: Vec<(i64, String, std::io::Result<ParsedFile>)> = pending
        .into_par_iter()
        .map(|(id, path)| {
            // Blade templates are not PHP classes: only their `route()` references.
            let result = if path.ends_with(".blade.php") {
                std::fs::read_to_string(root.join(&path)).map(|src| ParsedFile {
                    refs: crate::refs::blade_refs(&src),
                    ..ParsedFile::default()
                })
            } else {
                std::fs::read(root.join(&path)).map(|src| extract(&src))
            };
            (id, path, result)
        })
        .collect();

    let parsed_at = Instant::now();
    let total_symbols: usize = parsed
        .iter()
        .filter_map(|(_, _, r)| r.as_ref().ok())
        .map(|p| p.symbols.len())
        .sum();
    let bulk = total_symbols >= symbols::BULK_FTS_THRESHOLD;
    if bulk {
        symbols::bulk_insert_begin(&tx)?;
    }
    // A first index: the reference lookups are built once at the end, not
    // updated on each of the (hundreds of thousands of) inserts.
    let fresh_refs = bulk
        && tx.query_row("SELECT NOT EXISTS (SELECT 1 FROM refs)", [], |r| {
            r.get::<_, bool>(0)
        })?;
    if fresh_refs {
        tx.execute_batch(
            "DROP INDEX IF EXISTS refs_target; DROP INDEX IF EXISTS refs_file;
             DROP INDEX IF EXISTS refs_kind_target;",
        )?;
    }
    tracing::debug!(
        files = parsed.len(),
        ms = started.elapsed().as_millis() as u64,
        "php files read and parsed"
    );
    let mut report = PhpReport::default();
    for (file_id, path, result) in &parsed {
        let parsed = match result {
            Ok(parsed) => parsed,
            Err(e) => {
                report.failed += 1;
                tracing::warn!(path, error = %e, "could not read PHP file");
                continue;
            }
        };
        tx.prepare_cached("DELETE FROM symbols WHERE file_id = ?1")?
            .execute([file_id])?;
        // Nothing to delete when the table started empty (and no lookup to do it fast).
        if !fresh_refs {
            tx.prepare_cached("DELETE FROM refs WHERE file_id = ?1")?
                .execute([file_id])?;
        }
        {
            let mut ins = tx.prepare_cached(
                "INSERT INTO refs (file_id, kind, target, line) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for r in &parsed.refs {
                ins.execute(params![file_id, r.kind, r.target, r.line])?;
            }
        }
        tx.prepare_cached("DELETE FROM warnings WHERE file_id = ?1 AND adapter = ?2")?
            .execute(params![file_id, EXTRACTOR])?;
        for symbol in &parsed.symbols {
            symbols::insert(&tx, *file_id, symbol)?;
        }
        for w in &parsed.warnings {
            tx.prepare_cached(
                "INSERT INTO warnings (file_id, path, line, adapter, message)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?
            .execute(params![file_id, path, w.line, EXTRACTOR, w.message])?;
        }
        tx.prepare_cached(
            "INSERT OR REPLACE INTO extractor_state (file_id, extractor, version)
             VALUES (?1, ?2, ?3)",
        )?
        .execute(params![file_id, EXTRACTOR, PHP_EXTRACTOR_VERSION])?;
        report.parsed += 1;
        report.symbols += parsed.symbols.len();
        report.warnings += parsed.warnings.len();
    }
    if bulk {
        symbols::bulk_insert_end(&tx)?;
    }
    if fresh_refs {
        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS refs_target ON refs(target COLLATE NOCASE);
             CREATE INDEX IF NOT EXISTS refs_file ON refs(file_id);
             CREATE INDEX IF NOT EXISTS refs_kind_target ON refs(kind, target);",
        )?;
    }
    tx.commit()?;
    tracing::debug!(
        ms = parsed_at.elapsed().as_millis() as u64,
        bulk,
        "php symbols written"
    );
    report.duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        parsed = report.parsed,
        symbols = report.symbols,
        ms = report.duration_ms,
        "php extraction finished"
    );
    Ok(report)
}
