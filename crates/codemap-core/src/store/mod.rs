//! SQLite index. Versioned migrations are tracked with `PRAGMA user_version`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;

use crate::scan::ScanReport;
use crate::{Error, Result};

/// Embedded migrations, applied in order. Index `i` brings the schema to version `i + 1`.
const MIGRATIONS: &[&str] = &[
    include_str!("migrations/0001_init.sql"),
    include_str!("migrations/0002_symbols.sql"),
    include_str!("migrations/0003_routes.sql"),
    include_str!("migrations/0004_laravel_data.sql"),
    include_str!("migrations/0005_vue.sql"),
    include_str!("migrations/0006_pages.sql"),
    include_str!("migrations/0007_api_calls.sql"),
    include_str!("migrations/0008_i18n.sql"),
    include_str!("migrations/0009_refs.sql"),
    include_str!("migrations/0010_api_typed.sql"),
    include_str!("migrations/0011_i18n_guarded.sql"),
    include_str!("migrations/0012_api_prefixes.sql"),
    include_str!("migrations/0013_api_sends.sql"),
    include_str!("migrations/0014_schema_source.sql"),
    include_str!("migrations/0015_refs_kind_target.sql"),
    include_str!("migrations/0016_swift.sql"),
    include_str!("migrations/0017_android.sql"),
    include_str!("migrations/0018_rust.sql"),
    include_str!("migrations/0019_table_indexes.sql"),
];

/// Schema version this build creates and understands.
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

pub mod meta_keys {
    pub const SCHEMA_VERSION: &str = "schema_version";
    pub const INDEXED_AT: &str = "indexed_at";
    pub const GIT_COMMIT: &str = "git_commit";
    pub const GIT_DIRTY: &str = "git_dirty";
    pub const PROJECT_ROOT: &str = "project_root";
    pub const PROJECT_ID: &str = "project_id";
    pub const LAST_SCAN: &str = "last_scan_json";
    pub const COMPOSER_NAME: &str = "composer_name";
    pub const GIT_REPO: &str = "git_repo";
    pub const DETECTION_VERSION: &str = "detection_version";
    pub const LARAVEL_VERSION: &str = "laravel_version";
    pub const LARAVEL_LAYOUT: &str = "laravel_layout";
    pub const PHP_CONSTRAINT: &str = "php_constraint";
    pub const DETECTION: &str = "detection_json";
    pub const ROUTES_STATE: &str = "routes_state_json";
    pub const DATA_STATE: &str = "laravel_data_state_json";
    pub const VUE_DETECTION: &str = "vue_detection_json";
    pub const VUE_REGISTRY_STATE: &str = "vue_registry_state";
    pub const PACKAGE_NAME: &str = "package_name";
}

pub struct Store {
    conn: Connection,
    path: Option<PathBuf>,
}

/// What the scanner needs to decide whether a file changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileState {
    pub id: i64,
    pub hash: String,
    pub size: u64,
    pub mtime_ns: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileRow {
    pub path: String,
    pub lang: String,
    pub size: u64,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WarningRow {
    pub path: Option<String>,
    pub line: Option<u32>,
    pub adapter: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexStats {
    pub schema_version: i64,
    pub indexed_at: Option<String>,
    pub git_commit: Option<String>,
    pub git_dirty: Option<bool>,
    pub files_total: u64,
    pub source_bytes: u64,
    pub files_by_lang: BTreeMap<String, u64>,
    pub symbols_by_kind: BTreeMap<String, u64>,
    pub warnings: u64,
    pub last_scan: Option<ScanReport>,
}

impl Store {
    /// Opens (creating if needed) the index at `path` and applies pending migrations.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        let conn = Connection::open(path)?;
        Self::init(conn, Some(path.to_owned()))
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?, None)
    }

    fn init(conn: Connection, path: Option<PathBuf>) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(5))?;
        // Switching a new database to WAL (and the first migration) can fail
        // with SQLITE_BUSY without the busy handler being consulted when
        // another process opens the same new index at the same moment.
        retry_busy(|| {
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))
                .map_err(Error::from)
        })?;
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = NORMAL;")?;
        let mut store = Self { conn, path };
        retry_busy(|| store.migrate())?;
        Ok(store)
    }

    /// Applies pending migrations. Each step takes the write lock first and
    /// re-reads the version, so two processes opening a new index at the same
    /// time (CLI + MCP server) do not both run it.
    fn migrate(&mut self) -> Result<()> {
        loop {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
            if current > SCHEMA_VERSION {
                return Err(Error::IndexTooNew {
                    path: self.path.clone().unwrap_or_default(),
                    found: current,
                    supported: SCHEMA_VERSION,
                });
            }
            let Some(sql) = MIGRATIONS.get(current as usize) else {
                return Ok(());
            };
            let version = current + 1;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", version)?;
            set_meta(&tx, meta_keys::SCHEMA_VERSION, &version.to_string())?;
            tx.commit()?;
            tracing::debug!(version, "applied index migration");
        }
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }

    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        get_meta(&self.conn, key)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        set_meta(&self.conn, key, value)
    }

    /// Current state of every indexed file, keyed by relative path.
    pub fn file_states(&self) -> Result<HashMap<String, FileState>> {
        file_states(&self.conn)
    }

    /// All indexed files, sorted by path.
    pub fn list_files(&self) -> Result<Vec<FileRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, lang, size, hash FROM files ORDER BY path")?;
        let rows = stmt.query_map([], |row| {
            Ok(FileRow {
                path: row.get(0)?,
                lang: row.get(1)?,
                size: row.get(2)?,
                hash: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// All warnings, sorted by path, line and message.
    pub fn warnings(&self) -> Result<Vec<WarningRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT COALESCE(w.path, f.path), w.line, w.adapter, w.message
             FROM warnings w LEFT JOIN files f ON f.id = w.file_id
             ORDER BY 1, 2, 4",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(WarningRow {
                path: row.get(0)?,
                line: row.get(1)?,
                adapter: row.get(2)?,
                message: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn stats(&self) -> Result<IndexStats> {
        let (files_total, source_bytes): (u64, u64) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM files",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let mut stmt = self
            .conn
            .prepare("SELECT lang, COUNT(*) FROM files GROUP BY lang")?;
        let files_by_lang = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut stmt = self
            .conn
            .prepare("SELECT kind, COUNT(*) FROM symbols GROUP BY kind")?;
        let symbols_by_kind = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let warnings = self
            .conn
            .query_row("SELECT COUNT(*) FROM warnings", [], |row| row.get(0))?;
        let last_scan = self
            .get_meta(meta_keys::LAST_SCAN)?
            .map(|json| serde_json::from_str(&json))
            .transpose()?;
        Ok(IndexStats {
            schema_version: self.schema_version()?,
            indexed_at: self.get_meta(meta_keys::INDEXED_AT)?,
            git_commit: self.get_meta(meta_keys::GIT_COMMIT)?,
            git_dirty: self.get_meta(meta_keys::GIT_DIRTY)?.map(|v| v == "true"),
            files_total,
            source_bytes,
            files_by_lang,
            symbols_by_kind,
            warnings,
            last_scan,
        })
    }
}

/// Current state of every indexed file, keyed by relative path.
pub fn file_states(conn: &Connection) -> Result<HashMap<String, FileState>> {
    let mut stmt = conn.prepare("SELECT path, id, hash, size, mtime_ns FROM files")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            FileState {
                id: row.get(1)?,
                hash: row.get(2)?,
                size: row.get(3)?,
                mtime_ns: row.get(4)?,
            },
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Retries `f` while SQLite reports the database busy, for up to 5 seconds.
fn retry_busy<T>(mut f: impl FnMut() -> Result<T>) -> Result<T> {
    let started = std::time::Instant::now();
    loop {
        match f() {
            Err(Error::Sqlite(rusqlite::Error::SqliteFailure(e, _)))
                if e.code == rusqlite::ErrorCode::DatabaseBusy
                    && started.elapsed() < Duration::from_secs(5) =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            other => return other,
        }
    }
}

pub fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .optional()?)
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// Sets `key` to `value`, or removes it when `value` is `None`.
pub fn set_meta_opt(conn: &Connection, key: &str, value: Option<&str>) -> Result<()> {
    match value {
        Some(v) => set_meta(conn, key, v),
        None => {
            conn.execute("DELETE FROM meta WHERE key = ?1", [key])?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert_file(conn: &Connection, path: &str) -> i64 {
        conn.execute(
            "INSERT INTO files (path, hash, size, mtime_ns, lang, indexed_at)
             VALUES (?1, 'h', 1, 1, 'php', 'now')",
            [path],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn fresh_store_is_migrated() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(
            store
                .get_meta(meta_keys::SCHEMA_VERSION)
                .unwrap()
                .as_deref(),
            Some(SCHEMA_VERSION.to_string().as_str())
        );
    }

    #[test]
    fn reopen_is_idempotent_and_keeps_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p/index.db");
        {
            let store = Store::open(&path).unwrap();
            store.set_meta("k", "v").unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.get_meta("k").unwrap().as_deref(), Some("v"));
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn newer_index_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.db");
        drop(Store::open(&path).unwrap());
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        match Store::open(&path) {
            Err(Error::IndexTooNew { found, .. }) => assert_eq!(found, SCHEMA_VERSION + 1),
            other => panic!("expected IndexTooNew, got {:?}", other.err()),
        }
    }

    #[test]
    fn deleting_a_file_cascades_to_its_warnings() {
        let store = Store::open_in_memory().unwrap();
        let conn = store.conn();
        let id = insert_file(conn, "app/A.php");
        conn.execute(
            "INSERT INTO warnings (file_id, line, adapter, message) VALUES (?1, 3, 'php', 'x')",
            [id],
        )
        .unwrap();
        assert_eq!(
            store.warnings().unwrap()[0].path.as_deref(),
            Some("app/A.php")
        );
        conn.execute("DELETE FROM files WHERE id = ?1", [id])
            .unwrap();
        assert!(store.warnings().unwrap().is_empty());
    }

    #[test]
    fn meta_set_and_remove() {
        let store = Store::open_in_memory().unwrap();
        set_meta_opt(store.conn(), "a", Some("1")).unwrap();
        assert_eq!(store.get_meta("a").unwrap().as_deref(), Some("1"));
        set_meta_opt(store.conn(), "a", None).unwrap();
        assert_eq!(store.get_meta("a").unwrap(), None);
    }
}

#[cfg(test)]
mod concurrency_tests {
    use super::*;

    #[test]
    fn concurrent_first_open_migrates_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.db");
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || Store::open(&path).map(|s| s.schema_version().unwrap()))
            })
            .collect();
        for h in handles {
            assert_eq!(h.join().unwrap().unwrap(), SCHEMA_VERSION);
        }
    }
}
