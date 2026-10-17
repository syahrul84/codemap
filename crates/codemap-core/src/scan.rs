//! Incremental scanner. A file is re-read only when its mtime or size changed,
//! and reported as changed (to be re-parsed) only when its blake3 hash changed.

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use rayon::prelude::*;
use rusqlite::{Connection, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::store::{Store, file_states, meta_keys, set_meta, set_meta_opt};
use crate::{Config, Error, Result, git, time};

/// Files bigger than this are generated or vendored; skip them with a warning.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Adapter name used for scanner warnings.
pub const SCANNER: &str = "scanner";

/// Never indexed, whatever the config says (relative to the project root).
const ALWAYS_EXCLUDED: &[&str] = &[
    "/vendor/",
    "/storage/",
    "/bootstrap/cache/",
    "node_modules/",
    ".git/",
];

/// Build output, dependencies and asset catalogs of Swift projects.
const APPLE_EXCLUDED: &[&str] = &[
    ".build/",
    "DerivedData/",
    "/Pods/",
    "/Carthage/",
    "*.xcassets/",
    "*.xcodeproj/",
    "*.xcworkspace/",
    "*.xcframework/",
    "*.framework/",
];

/// Build output of Cargo (Rust) projects.
const CARGO_EXCLUDED: &[&str] = &["target/"];

/// Build output and IDE folders of Gradle (Android) projects.
const GRADLE_EXCLUDED: &[&str] = &["build/", ".gradle/", ".idea/", ".cxx/", ".kotlin/"];

/// A Gradle build at the root, or one folder down (`android/` beside an
/// iOS app).
pub fn is_gradle_project(root: &Path) -> bool {
    let has = |d: &Path| {
        ["settings.gradle", "settings.gradle.kts"]
            .iter()
            .any(|f| d.join(f).is_file())
    };
    has(root)
        || std::fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| e.path().is_dir() && has(&e.path()))
}

/// A Swift package or an Xcode project at the root.
pub fn is_apple_project(root: &Path) -> bool {
    root.join("Package.swift").is_file()
        || std::fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.ends_with(".xcodeproj") || n.ends_with(".xcworkspace")
            })
}

/// Indexed from the project root in addition to the include dirs.
const ROOT_FILES: &[&str] = &[
    "Package.swift",
    "composer.json",
    "composer.lock",
    "artisan",
    "package.json",
    "nuxt.config.ts",
    "nuxt.config.js",
    "nuxt.config.mjs",
    "vite.config.ts",
    "vite.config.js",
    "vite.config.mjs",
    "vue.config.js",
    "tsconfig.json",
    "jsconfig.json",
    "app.vue",
    "error.vue",
    "app.config.ts",
];

#[derive(Debug, Clone, Copy, Default)]
pub struct ScanOptions {
    /// Treat every file as changed.
    pub full: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanReport {
    pub walked: usize,
    /// Same mtime and size: not read.
    pub unchanged: usize,
    /// mtime or size changed but content is identical: not re-parsed.
    pub touched: usize,
    pub added: usize,
    pub modified: usize,
    pub removed: usize,
    /// Found but not indexed (too large, unreadable).
    pub skipped: usize,
    /// `added + modified`: files whose facts must be re-extracted.
    pub reparsed: usize,
    pub warnings: usize,
    pub duration_ms: u64,
    /// Added or modified paths, sorted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_paths: Vec<String>,
    /// Removed paths, sorted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_paths: Vec<String>,
}

#[derive(Debug, Clone)]
struct Walked {
    rel: String,
    abs: PathBuf,
    size: u64,
    mtime_ns: i64,
    lang: &'static str,
}

#[derive(Debug)]
struct Warning {
    path: Option<String>,
    message: String,
}

/// Scans `root` and brings the `files` table up to date.
pub fn scan(
    store: &mut Store,
    root: &Path,
    config: &Config,
    opts: ScanOptions,
) -> Result<ScanReport> {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut report = ScanReport::default();

    let walked = walk(root, config, &mut warnings, &mut report)?;
    report.walked = walked.len();
    if walked.is_empty() {
        // Usually an empty mount: Docker Desktop shows unshared host paths as empty dirs.
        tracing::warn!(
            root = %root.display(),
            "no indexable files found; check the project path (and Docker file sharing)"
        );
    }

    // Git runs a subprocess; do it before taking the write lock.
    let head = git::head(root);
    let now = time::now_rfc3339();

    // Read, compare and write under one write lock, so a concurrent scan of the
    // same index (CLI + MCP server) waits and then sees our result.
    let tx = store
        .conn_mut()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing = file_states(&tx)?;
    let seen: HashSet<&str> = walked.iter().map(|w| w.rel.as_str()).collect();
    let mut removed: Vec<String> = existing
        .keys()
        .filter(|path| !seen.contains(path.as_str()))
        .cloned()
        .collect();
    removed.sort();

    let candidates: Vec<&Walked> = walked
        .iter()
        .filter(|w| {
            let same_stat = existing
                .get(&w.rel)
                .is_some_and(|s| s.size == w.size && s.mtime_ns == w.mtime_ns);
            opts.full || !same_stat
        })
        .collect();
    report.unchanged = walked.len() - candidates.len();

    let hashed: Vec<(&Walked, std::io::Result<String>)> = candidates
        .into_par_iter()
        .map(|w| (w, hash_file(&w.abs)))
        .collect();

    let mut added = Vec::new();
    let mut modified = Vec::new();
    let mut touched = Vec::new();
    for (w, hash) in hashed {
        let hash = match hash {
            Ok(h) => h,
            Err(e) => {
                report.skipped += 1;
                warnings.push(Warning {
                    path: Some(w.rel.clone()),
                    message: format!("could not read file: {e}"),
                });
                continue;
            }
        };
        match existing.get(&w.rel) {
            None => added.push((w, hash)),
            Some(s) if !opts.full && s.hash == hash => touched.push(w),
            Some(_) => modified.push((w, hash)),
        }
    }

    report.added = added.len();
    report.modified = modified.len();
    report.touched = touched.len();
    report.removed = removed.len();
    report.reparsed = report.added + report.modified;
    report.warnings = warnings.len();
    report.changed_paths = added
        .iter()
        .chain(&modified)
        .map(|(w, _)| w.rel.clone())
        .collect();
    report.changed_paths.sort();
    report.removed_paths = removed;

    tx.execute("DELETE FROM warnings WHERE adapter = ?1", [SCANNER])?;
    for path in &report.removed_paths {
        tx.execute("DELETE FROM files WHERE path = ?1", [path])?;
    }
    // Modified files get a new row so `ON DELETE CASCADE` drops their old facts.
    for (w, _) in &modified {
        tx.execute("DELETE FROM files WHERE path = ?1", [&w.rel])?;
    }
    for (w, hash) in added.iter().chain(&modified) {
        insert_file(&tx, w, hash, &now)?;
    }
    for w in &touched {
        tx.execute(
            "UPDATE files SET size = ?2, mtime_ns = ?3 WHERE path = ?1",
            params![w.rel, w.size as i64, w.mtime_ns],
        )?;
    }
    for warning in &warnings {
        tx.execute(
            "INSERT INTO warnings (file_id, path, adapter, message)
             VALUES ((SELECT id FROM files WHERE path = ?1), ?1, ?2, ?3)",
            params![warning.path, SCANNER, warning.message],
        )?;
    }

    report.duration_ms = started.elapsed().as_millis() as u64;
    let summary = ScanReport {
        changed_paths: Vec::new(),
        removed_paths: Vec::new(),
        ..report.clone()
    };
    set_meta(&tx, meta_keys::INDEXED_AT, &now)?;
    set_meta(&tx, meta_keys::PROJECT_ROOT, &root.to_string_lossy())?;
    set_meta(&tx, meta_keys::LAST_SCAN, &serde_json::to_string(&summary)?)?;
    // `git status` is slow on big repos and Docker Desktop mounts: only re-run
    // it when the commit or the indexed files changed.
    let previous_commit = crate::store::get_meta(&tx, meta_keys::GIT_COMMIT)?;
    let previous_dirty = crate::store::get_meta(&tx, meta_keys::GIT_DIRTY)?.map(|d| d == "true");
    let files_changed = report.reparsed + report.removed + report.touched > 0;
    let dirty = match (&head, previous_dirty) {
        (None, _) => None,
        (Some(h), Some(d)) if !files_changed && previous_commit.as_deref() == Some(h.as_str()) => {
            Some(d)
        }
        (Some(_), _) => git::dirty(root),
    };
    let git = git::GitInfo {
        commit: head,
        dirty,
    };
    set_meta_opt(&tx, meta_keys::GIT_COMMIT, git.commit.as_deref())?;
    set_meta_opt(
        &tx,
        meta_keys::GIT_DIRTY,
        git.dirty.map(|d| if d { "true" } else { "false" }),
    )?;
    tx.commit()?;

    tracing::info!(
        walked = report.walked,
        reparsed = report.reparsed,
        removed = report.removed,
        ms = report.duration_ms,
        "scan finished"
    );
    Ok(report)
}

fn insert_file(conn: &Connection, w: &Walked, hash: &str, now: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO files (path, hash, size, mtime_ns, lang, indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![w.rel, hash, w.size as i64, w.mtime_ns, w.lang, now],
    )?;
    Ok(())
}

/// Lists indexable files under the include dirs plus the root files, keyed by relative path.
fn walk(
    root: &Path,
    config: &Config,
    warnings: &mut Vec<Warning>,
    report: &mut ScanReport,
) -> Result<Vec<Walked>> {
    let mut found: BTreeMap<String, Walked> = BTreeMap::new();

    for name in ROOT_FILES {
        let abs = root.join(name);
        if abs.is_file() {
            consider(root, &abs, warnings, report, &mut found);
        }
    }

    let mut starts: Vec<PathBuf> = config
        .scan
        .include
        .iter()
        .map(|inc| root.join(inc.trim_matches('/')))
        .filter(|p| p.is_dir())
        .collect();
    // Swift packages, Xcode apps, Gradle (Android) apps and Cargo workspaces
    // keep code in folders of any name (`Sources/`, `app/`, `crates/`): every
    // top-level folder is scanned.
    let apple = is_apple_project(root);
    let cargo = root.join("Cargo.toml").is_file();
    let mobile = apple || is_gradle_project(root) || cargo;
    if mobile {
        for e in std::fs::read_dir(root).into_iter().flatten().flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            let wanted = p.is_dir() || name.ends_with(".swift") || name.ends_with(".rs");
            if wanted && !name.starts_with('.') && !starts.contains(&p) {
                starts.push(p);
            }
        }
    }
    let Some((first, rest)) = starts.split_first() else {
        return Ok(found.into_values().collect());
    };

    let mut overrides = OverrideBuilder::new(root);
    let apple_excluded = APPLE_EXCLUDED
        .iter()
        .chain(GRADLE_EXCLUDED)
        .filter(|_| mobile)
        .chain(CARGO_EXCLUDED.iter().filter(|_| cargo))
        .map(|p| (*p).to_owned());
    for pattern in ALWAYS_EXCLUDED
        .iter()
        .map(|p| (*p).to_owned())
        .chain(apple_excluded)
        .chain(config.scan.exclude.iter().cloned())
    {
        overrides
            .add(&format!("!{pattern}"))
            .map_err(|e| Error::Config(format!("invalid exclude pattern {pattern:?}: {e}")))?;
    }

    let mut builder = WalkBuilder::new(first);
    for start in rest {
        builder.add(start);
    }
    builder
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(false)
        .require_git(false)
        .follow_links(false)
        .overrides(overrides.build()?)
        .filter_entry(|entry| !entry.file_name().to_string_lossy().starts_with(".env"));

    // Walk and stat in parallel: on Docker Desktop bind mounts every directory
    // read and stat is a round trip to the host, so latency (not CPU) dominates.
    let paths: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());
    let errors: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    builder.build_parallel().run(|| {
        let (paths, errors) = (&paths, &errors);
        Box::new(move |entry| {
            match entry {
                Ok(e) if e.file_type().is_some_and(|t| t.is_file()) => {
                    if lang_for(e.path()).is_some() {
                        paths.lock().expect("walk lock").push(e.into_path());
                    }
                }
                Ok(_) => {}
                Err(err) => errors
                    .lock()
                    .expect("walk lock")
                    .push(format!("walk error: {err}")),
            }
            ignore::WalkState::Continue
        })
    });
    for message in errors.into_inner().expect("walk lock") {
        warnings.push(Warning {
            path: None,
            message,
        });
    }
    let stats: Vec<(PathBuf, std::io::Result<std::fs::Metadata>)> = paths
        .into_inner()
        .expect("walk lock")
        .into_par_iter()
        .map(|p| {
            let m = std::fs::metadata(&p);
            (p, m)
        })
        .collect();
    for (abs, meta) in stats {
        consider_with(root, &abs, meta, warnings, report, &mut found);
    }
    Ok(found.into_values().collect())
}

fn consider(
    root: &Path,
    abs: &Path,
    warnings: &mut Vec<Warning>,
    report: &mut ScanReport,
    found: &mut BTreeMap<String, Walked>,
) {
    consider_with(root, abs, std::fs::metadata(abs), warnings, report, found);
}

fn consider_with(
    root: &Path,
    abs: &Path,
    meta: std::io::Result<std::fs::Metadata>,
    warnings: &mut Vec<Warning>,
    report: &mut ScanReport,
    found: &mut BTreeMap<String, Walked>,
) {
    let Some(rel) = relative(root, abs) else {
        return;
    };
    let Some(lang) = lang_for(abs) else {
        return;
    };
    let meta = match meta {
        Ok(m) => m,
        Err(e) => {
            report.skipped += 1;
            warnings.push(Warning {
                path: Some(rel),
                message: format!("could not stat file: {e}"),
            });
            return;
        }
    };
    if meta.len() > MAX_FILE_BYTES {
        report.skipped += 1;
        warnings.push(Warning {
            path: Some(rel),
            message: format!(
                "skipped: {} bytes is over the {MAX_FILE_BYTES}-byte limit",
                meta.len()
            ),
        });
        return;
    }
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i64);
    found.insert(
        rel.clone(),
        Walked {
            rel,
            abs: abs.to_owned(),
            size: meta.len(),
            mtime_ns,
            lang,
        },
    );
}

/// `abs` relative to `root` with `/` separators.
fn relative(root: &Path, abs: &Path) -> Option<String> {
    let rel = abs.strip_prefix(root).ok()?;
    let parts: Vec<_> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect();
    Some(parts.join("/"))
}

fn lang_for(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_str()?;
    match name {
        "artisan" => return Some("php"),
        "composer.lock" => return Some("json"),
        // Type declarations and minified bundles hold no project facts.
        _ if name.ends_with(".d.ts") || name.contains(".min.") => return None,
        _ => {}
    }
    match path.extension()?.to_str()? {
        "php" => Some("php"),
        "sql" => Some("sql"),
        "json" => Some("json"),
        "vue" => Some("vue"),
        "swift" => Some("swift"),
        "strings" => Some("strings"),
        "xcstrings" => Some("xcstrings"),
        "kt" => Some("kotlin"),
        "java" => Some("java"),
        "rs" => Some("rust"),
        // Android resources (strings, layouts, menus) and the manifest.
        "xml" if name == "AndroidManifest.xml" || is_android_res(path) => Some("androidxml"),
        "js" | "mjs" | "cjs" | "jsx" => Some("js"),
        "ts" | "mts" | "cts" | "tsx" => Some("ts"),
        _ => None,
    }
}

fn is_android_res(path: &Path) -> bool {
    let mut parts = path.iter().rev().skip(1).map(|p| p.to_string_lossy());
    let dir = parts.next().unwrap_or_default();
    parts.next().is_some_and(|p| p == "res")
        && ["values", "layout", "menu", "navigation", "xml"]
            .iter()
            .any(|d| dir == *d || dir.starts_with(&format!("{d}-")))
}

fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(File::open(path)?)?;
    Ok(hasher.finalize().to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_by_extension() {
        assert_eq!(lang_for(Path::new("app/User.php")), Some("php"));
        assert_eq!(lang_for(Path::new("artisan")), Some("php"));
        assert_eq!(
            lang_for(Path::new("database/schema/mysql-schema.sql")),
            Some("sql")
        );
        assert_eq!(lang_for(Path::new("composer.json")), Some("json"));
        assert_eq!(lang_for(Path::new("composer.lock")), Some("json"));
        assert_eq!(lang_for(Path::new("yarn.lock")), None);
        assert_eq!(
            lang_for(Path::new("resources/views/a.blade.php")),
            Some("php")
        );
        assert_eq!(lang_for(Path::new("README.md")), None);
        assert_eq!(lang_for(Path::new("src/App.vue")), Some("vue"));
        assert_eq!(lang_for(Path::new("src/main.mjs")), Some("js"));
        assert_eq!(lang_for(Path::new("app/x.tsx")), Some("ts"));
        assert_eq!(lang_for(Path::new("env.d.ts")), None);
        assert_eq!(lang_for(Path::new("src/lib/jquery.min.js")), None);
    }

    #[test]
    fn relative_uses_forward_slashes() {
        assert_eq!(
            relative(Path::new("/w"), Path::new("/w/app/Models/User.php")).as_deref(),
            Some("app/Models/User.php")
        );
        assert_eq!(relative(Path::new("/w"), Path::new("/x/a.php")), None);
    }
}
