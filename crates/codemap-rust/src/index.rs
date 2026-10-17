//! Rust refresh: crates are read from `Cargo.toml`, each file's module path
//! follows Cargo's layout, new or changed files are extracted, then the
//! entry points (binaries, subcommands) are rebuilt.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use codemap_core::symbols;
use codemap_core::{Result, ScanReport, Store};
use rayon::prelude::*;
use rusqlite::{Connection, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::extract::{self, Extracted, RustFacts};

/// Extractor name in `extractor_state`.
pub const EXTRACTOR: &str = "rust";

/// Bump when per-file output changes; every Rust file is then re-parsed.
pub const RUST_EXTRACTOR_VERSION: i64 = 1;

/// `pages.source` for Rust entry points.
pub const SOURCE: &str = "rust";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RustReport {
    pub detected: bool,
    pub parsed: usize,
    pub failed: usize,
    pub symbols: usize,
    pub crates: usize,
    pub entry_points: usize,
    pub duration_ms: u64,
}

/// A package from a `Cargo.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Crate {
    /// As in `Cargo.toml`: `codemap-core`.
    pub name: String,
    /// Folder, relative to the project root (`` for the root).
    pub dir: String,
    #[serde(default)]
    pub lib: bool,
    /// Binaries: name → file.
    #[serde(default)]
    pub bins: Vec<(String, String)>,
    /// Dependencies by name, as in `[dependencies]`.
    #[serde(default)]
    pub deps: Vec<String>,
}

impl Crate {
    /// The name in Rust paths: `codemap_core`.
    pub fn ident(&self) -> String {
        self.name.replace('-', "_")
    }
}

/// Every package under `root` that has a `Cargo.toml` with `[package]`,
/// found by walking up from the indexed Rust files.
pub fn crates(root: &Path, rust_files: &[String]) -> Vec<Crate> {
    let mut dirs: Vec<String> = Vec::new();
    for f in rust_files {
        let mut dir = Path::new(f).parent().map(Path::to_path_buf);
        while let Some(d) = dir {
            let s = d.to_string_lossy().into_owned();
            if dirs.contains(&s) {
                break;
            }
            if root.join(&d).join("Cargo.toml").is_file() {
                dirs.push(s);
                break;
            }
            dir = d.parent().map(Path::to_path_buf);
        }
    }
    dirs.sort();
    dirs.dedup();
    dirs.into_iter()
        .filter_map(|dir| read_crate(root, &dir))
        .collect()
}

fn read_crate(root: &Path, dir: &str) -> Option<Crate> {
    let base: PathBuf = root.join(dir);
    let text = std::fs::read_to_string(base.join("Cargo.toml")).ok()?;
    let doc: toml::Table = text.parse().ok()?;
    let name = doc.get("package")?.get("name")?.as_str()?.to_owned();
    let rel = |p: &str| {
        if dir.is_empty() {
            p.to_owned()
        } else {
            format!("{dir}/{p}")
        }
    };
    let lib = doc.contains_key("lib") || base.join("src/lib.rs").is_file();
    let mut bins: Vec<(String, String)> = Vec::new();
    if let Some(list) = doc.get("bin").and_then(|b| b.as_array()) {
        for b in list {
            let bname = b.get("name").and_then(|n| n.as_str()).unwrap_or(&name);
            let path = b
                .get("path")
                .and_then(|p| p.as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("src/bin/{bname}.rs"));
            bins.push((bname.to_owned(), rel(&path)));
        }
    }
    if base.join("src/main.rs").is_file() && !bins.iter().any(|(_, p)| p == &rel("src/main.rs")) {
        bins.push((name.clone(), rel("src/main.rs")));
    }
    if let Ok(rd) = std::fs::read_dir(base.join("src/bin")) {
        for e in rd.flatten() {
            let fname = e.file_name().to_string_lossy().into_owned();
            let (bname, path) = if let Some(stem) = fname.strip_suffix(".rs") {
                (stem.to_owned(), format!("src/bin/{fname}"))
            } else if e.path().join("main.rs").is_file() {
                (fname.clone(), format!("src/bin/{fname}/main.rs"))
            } else {
                continue;
            };
            if !bins.iter().any(|(n, _)| n == &bname) {
                bins.push((bname, rel(&path)));
            }
        }
    }
    let mut deps: Vec<String> = Vec::new();
    for table in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(t) = doc.get(table).and_then(|t| t.as_table()) {
            for k in t.keys() {
                if !deps.contains(k) {
                    deps.push(k.clone());
                }
            }
        }
    }
    Some(Crate {
        name,
        dir: dir.to_owned(),
        lib,
        bins,
        deps,
    })
}

/// The Rust path of a file: `codemap_core::store` for
/// `crates/codemap-core/src/store/mod.rs`.
pub fn module_of(path: &str, crates: &[Crate]) -> String {
    let krate = crates
        .iter()
        .filter(|c| c.dir.is_empty() || path.starts_with(&format!("{}/", c.dir)))
        .max_by_key(|c| c.dir.len());
    let Some(k) = krate else {
        let stem = path.trim_end_matches(".rs").replace(['/', '-'], "_");
        return stem;
    };
    let rel = if k.dir.is_empty() {
        path
    } else {
        &path[k.dir.len() + 1..]
    };
    // In a crate with only a binary, every module hangs off the binary.
    let ident = if !k.lib && k.bins.len() == 1 {
        k.bins[0].0.replace('-', "_")
    } else {
        k.ident()
    };
    let to_path = |s: &str| -> String {
        let s = s.trim_end_matches(".rs");
        let s = s.strip_suffix("/mod").unwrap_or(s);
        s.replace('/', "::").replace('-', "_")
    };
    // A binary is its own crate, named after the binary.
    if let Some((bname, _)) = k.bins.iter().find(|(_, p)| p == path) {
        return bname.replace('-', "_");
    }
    if let Some(inner) = rel.strip_prefix("src/") {
        if let Some(bin) = inner.strip_prefix("bin/") {
            let mut parts = bin.splitn(2, '/');
            let bname = parts
                .next()
                .unwrap_or(bin)
                .trim_end_matches(".rs")
                .replace('-', "_");
            return match parts.next() {
                Some(rest) if rest != "main.rs" => format!("{bname}::{}", to_path(rest)),
                _ => bname,
            };
        }
        return match inner {
            "lib.rs" | "main.rs" => ident,
            _ => format!("{ident}::{}", to_path(inner)),
        };
    }
    if rel == "build.rs" {
        return format!("{ident}_build");
    }
    // `tests/`, `examples/`, `benches/`: each file is a crate of its own.
    for dir in ["tests/", "examples/", "benches/"] {
        if let Some(inner) = rel.strip_prefix(dir) {
            return to_path(inner);
        }
    }
    format!("{ident}::{}", to_path(rel))
}

pub fn refresh(
    store: &mut Store,
    root: &Path,
    scan: &ScanReport,
    full: bool,
) -> Result<RustReport> {
    let started = Instant::now();
    let paths: Vec<String> = {
        let mut stmt = store
            .conn()
            .prepare("SELECT path FROM files WHERE lang = 'rust' ORDER BY path")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    if paths.is_empty() {
        store
            .conn()
            .execute("DELETE FROM pages WHERE source = ?1", [SOURCE])?;
        return Ok(RustReport::default());
    }
    let cargo_changed = scan
        .changed_paths
        .iter()
        .chain(&scan.removed_paths)
        .any(|p| p.ends_with("Cargo.toml"));
    let crates = crates(root, &paths);
    let mut report = index_pending(store, root, &crates)?;
    report.detected = true;
    report.crates = crates.len();
    let removed = scan.removed_paths.iter().any(|p| p.ends_with(".rs"));
    let never_ran: bool = store.conn().query_row(
        "SELECT NOT EXISTS (SELECT 1 FROM meta WHERE key = 'rust_entry_points')",
        [],
        |r| r.get(0),
    )?;
    if full || report.parsed > 0 || removed || never_ran || cargo_changed {
        report.entry_points = rebuild(store.conn_mut(), &crates)?;
    }
    report.duration_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

type FileRef = (i64, String);

pub fn index_pending(store: &mut Store, root: &Path, crates: &[Crate]) -> Result<RustReport> {
    let started = Instant::now();
    let tx = store
        .conn_mut()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let pending: Vec<FileRef> = {
        let mut stmt = tx.prepare(
            "SELECT f.id, f.path FROM files f
             WHERE f.lang = 'rust' AND NOT EXISTS (
                 SELECT 1 FROM extractor_state e
                 WHERE e.file_id = f.id AND e.extractor = ?1 AND e.version = ?2)
             ORDER BY f.path",
        )?;
        let rows = stmt.query_map(params![EXTRACTOR, RUST_EXTRACTOR_VERSION], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let parsed: Vec<(i64, String, std::io::Result<Extracted>)> = pending
        .into_par_iter()
        .map(|(id, path)| {
            let module = module_of(&path, crates);
            let result = std::fs::read(root.join(&path)).map(|bytes| {
                let src = String::from_utf8_lossy(&bytes);
                // A bug in one file's analysis must not stop the whole index.
                std::panic::catch_unwind(|| extract::extract_file(&module, &src)).unwrap_or_else(
                    |_| Extracted {
                        facts: RustFacts {
                            module: module.clone(),
                            errors: 1,
                            ..RustFacts::default()
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
    let mut report = RustReport::default();
    {
        let mut insert_file = tx.prepare(
            "INSERT OR REPLACE INTO rust_files (file_id, path, facts_json) VALUES (?1, ?2, ?3)",
        )?;
        let mut insert_ref =
            tx.prepare("INSERT INTO refs (file_id, kind, target, line) VALUES (?1, ?2, ?3, ?4)")?;
        for (file_id, path, result) in &parsed {
            let x = match result {
                Ok(x) => x,
                Err(e) => {
                    report.failed += 1;
                    tracing::warn!(path, error = %e, "could not read Rust file");
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
            tx.execute(
                "INSERT OR REPLACE INTO extractor_state (file_id, extractor, version) VALUES (?1, ?2, ?3)",
                params![file_id, EXTRACTOR, RUST_EXTRACTOR_VERSION],
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
            "rust extraction finished"
        );
    }
    Ok(report)
}

/// One Rust file's facts, as stored.
pub struct RustFile {
    pub file_id: i64,
    pub path: String,
    pub facts: RustFacts,
}

pub fn load_files(conn: &Connection) -> Result<Vec<RustFile>> {
    let mut stmt =
        conn.prepare_cached("SELECT file_id, path, facts_json FROM rust_files ORDER BY path")?;
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
        out.push(RustFile {
            file_id,
            path,
            facts,
        });
    }
    Ok(out)
}

/// Line ranges of test code, per file (`#[cfg(test)]` modules, `#[test]`
/// functions), for telling test uses apart.
pub fn test_ranges(conn: &Connection) -> Result<HashMap<String, Vec<(u32, u32)>>> {
    Ok(load_files(conn)?
        .into_iter()
        .filter(|f| !f.facts.test_ranges.is_empty())
        .map(|f| (f.path, f.facts.test_ranges))
        .collect())
}

/// Rebuilds the `rust` pages: binaries and their command-line subcommands.
fn rebuild(conn: &mut Connection, crates: &[Crate]) -> Result<usize> {
    let files = load_files(conn)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute("DELETE FROM pages WHERE source = ?1", [SOURCE])?;
    let mut n = 0;
    {
        let mut ins = tx.prepare(
            "INSERT INTO pages (route_path, name, file, source, defined_in, parent, meta)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        let main_line: HashMap<&str, u32> = files
            .iter()
            .filter_map(|f| f.facts.main.map(|l| (f.path.as_str(), l)))
            .collect();
        for c in crates {
            for (bin, path) in &c.bins {
                let line = main_line.get(path.as_str()).copied().unwrap_or(1);
                ins.execute(params![
                    "binary",
                    bin,
                    path,
                    SOURCE,
                    format!("{path}:{line}"),
                    Option::<String>::None,
                    serde_json::json!({"crate": c.name}).to_string(),
                ])?;
                n += 1;
            }
        }
        for f in &files {
            // The binary of the crate the file belongs to, when there is one.
            let krate = crates
                .iter()
                .filter(|c| c.dir.is_empty() || f.path.starts_with(&format!("{}/", c.dir)))
                .max_by_key(|c| c.dir.len());
            let bin = krate.and_then(|c| (c.bins.len() == 1).then(|| c.bins[0].0.clone()));
            for cmd in &f.facts.commands {
                ins.execute(params![
                    "command",
                    cmd.name,
                    f.path,
                    SOURCE,
                    format!("{}:{}", f.path, cmd.line),
                    bin,
                    serde_json::json!({"variant": cmd.variant, "title": cmd.doc}).to_string(),
                ])?;
                n += 1;
            }
        }
    }
    tx.execute(
        "INSERT INTO meta (key, value) VALUES ('rust_entry_points', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [n.to_string()],
    )?;
    tx.commit()?;
    Ok(n)
}

/// For `project_overview`: crates, binaries, commands and counts. Null when
/// there is no Rust code.
pub fn overview(store: &Store, root: &Path) -> Result<serde_json::Value> {
    let files = load_files(store.conn())?;
    if files.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    let paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
    let crates = crates(root, &paths);
    let names: Vec<&str> = crates.iter().map(|c| c.name.as_str()).collect();
    let mut per_crate: HashMap<&str, usize> = HashMap::new();
    for p in &paths {
        if let Some(c) = crates
            .iter()
            .filter(|c| c.dir.is_empty() || p.starts_with(&format!("{}/", c.dir)))
            .max_by_key(|c| c.dir.len())
        {
            *per_crate.entry(c.name.as_str()).or_default() += 1;
        }
    }
    let list: Vec<serde_json::Value> = crates
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "dir": if c.dir.is_empty() { "." } else { c.dir.as_str() },
                "lib": c.lib,
                "bins": c.bins.iter().map(|(n, _)| n).collect::<Vec<_>>(),
                "files": per_crate.get(c.name.as_str()).copied().unwrap_or(0),
                // Other crates of this workspace it uses.
                "uses": c.deps.iter().filter(|d| names.contains(&d.as_str())).collect::<Vec<_>>(),
            })
        })
        .collect();
    let commands: usize = files.iter().map(|f| f.facts.commands.len()).sum();
    let tests: usize = files
        .iter()
        .filter(|f| !f.facts.test_ranges.is_empty() || f.path.contains("tests/"))
        .count();
    let mut by_kind: std::collections::BTreeMap<String, i64> = Default::default();
    {
        let mut stmt = store.conn().prepare(
            "SELECT s.kind, COUNT(*) FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE f.lang = 'rust' GROUP BY s.kind",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (k, c) = row?;
            by_kind.insert(k, c);
        }
    }
    Ok(serde_json::json!({
        "rust_files": files.len(),
        "files_with_tests": tests,
        "crates": list,
        "commands": commands,
        "symbols": by_kind,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_paths() {
        let crates = vec![
            Crate {
                name: "my-core".into(),
                dir: "crates/my-core".into(),
                lib: true,
                ..Crate::default()
            },
            Crate {
                name: "my-cli".into(),
                dir: "crates/my-cli".into(),
                bins: vec![("my".into(), "crates/my-cli/src/main.rs".into())],
                ..Crate::default()
            },
        ];
        let m = |p: &str| module_of(p, &crates);
        assert_eq!(m("crates/my-core/src/lib.rs"), "my_core");
        assert_eq!(m("crates/my-core/src/store/mod.rs"), "my_core::store");
        assert_eq!(
            m("crates/my-core/src/store/migrations.rs"),
            "my_core::store::migrations"
        );
        assert_eq!(m("crates/my-cli/src/main.rs"), "my");
        assert_eq!(m("crates/my-cli/src/update.rs"), "my::update");
        assert_eq!(m("crates/my-cli/src/bin/tool.rs"), "tool");
        assert_eq!(m("crates/my-core/tests/scan.rs"), "scan");
        assert_eq!(m("crates/my-core/build.rs"), "my_core_build");
    }
}
