//! Migrations not yet applied: the migration files compared with the
//! `migrations` table of real databases (local, staging, …). Connections
//! come only from environment variables named in `.codemap.toml` `[[databases]]`
//! (or `CODEMAP_DB_<NAME>`); `.env` is never read. Each check is one `SELECT`
//! in a read-only session, with a short timeout. Off unless configured.

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use codemap_core::config::DatabaseConfig;
use serde_json::{Value, json};

use crate::config_php::ConfigReader;

const TIMEOUT: Duration = Duration::from_secs(5);
const ENV_PREFIX: &str = "CODEMAP_DB_";
/// The name before codemap (pidx); still read.
const OLD_ENV_PREFIX: &str = "PIDX_DB_";

/// The databases to check: configured ones, plus `CODEMAP_DB_<NAME>` variables.
pub fn databases(configured: &[DatabaseConfig], env: &[(String, String)]) -> Vec<DatabaseConfig> {
    let mut out: Vec<DatabaseConfig> = configured.to_vec();
    let mut vars: Vec<(String, String)> = env
        .iter()
        .filter(|(k, v)| {
            [ENV_PREFIX, OLD_ENV_PREFIX]
                .iter()
                .any(|p| k.starts_with(p) && k.len() > p.len())
                && !v.is_empty()
        })
        .cloned()
        .collect();
    vars.sort();
    for (k, _) in vars {
        let prefix = if k.starts_with(ENV_PREFIX) {
            ENV_PREFIX
        } else {
            OLD_ENV_PREFIX
        };
        let name = k[prefix.len()..].to_ascii_lowercase();
        if out.iter().any(|d| d.env == k || d.name == name) {
            continue;
        }
        out.push(DatabaseConfig {
            name,
            env: k,
            ..DatabaseConfig::default()
        });
    }
    out
}

/// Migration names from the files (`2024_01_01_000000_create_users_table`).
pub fn migration_names(files: &[String]) -> Vec<String> {
    let mut names: Vec<String> = files
        .iter()
        .filter(|f| f.starts_with("database/migrations/") && f.ends_with(".php"))
        .filter_map(|f| f.rsplit('/').next())
        .map(|f| f.trim_end_matches(".php").to_owned())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// `config('database.migrations.table')` (Laravel 11+) or `database.migrations`.
fn table_name(root: &Path, db: &DatabaseConfig) -> String {
    if let Some(t) = &db.table {
        return t.clone();
    }
    let cfg = ConfigReader::new(root);
    cfg.get("database.migrations.table")
        .or_else(|| cfg.get("database.migrations"))
        .and_then(|v| v.value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "migrations".into())
}

fn valid_ident(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 128
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}

/// `postgres://user:secret@db:5432/app` → `postgres db:5432/app` (no credentials).
pub fn describe_url(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("?", url));
    let rest = rest.rsplit_once('@').map_or(rest, |(_, r)| r);
    let rest = rest.split('?').next().unwrap_or(rest);
    format!("{scheme} {rest}")
}

/// Removes the password (and user) from a driver's error text.
pub(crate) fn scrub(msg: &str, url: &str) -> String {
    let mut m = msg.to_owned();
    if let Some((_, rest)) = url.split_once("://")
        && let Some((cred, _)) = rest.rsplit_once('@')
    {
        for part in cred.split(':') {
            if part.len() >= 3 {
                m = m.replace(part, "***");
            }
        }
    }
    m
}

/// `(migration, batch)` rows of the migrations table.
fn read_rows(url: &str, table: &str) -> Result<Vec<(String, i64)>, String> {
    let scheme = url.split("://").next().unwrap_or("").to_ascii_lowercase();
    match scheme.as_str() {
        "postgres" | "postgresql" => {
            let mut cfg: postgres::Config =
                url.parse().map_err(|e: postgres::Error| e.to_string())?;
            cfg.connect_timeout(TIMEOUT);
            let mut client = cfg.connect(postgres::NoTls).map_err(|e| e.to_string())?;
            client
                .batch_execute("SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY")
                .map_err(|e| e.to_string())?;
            let rows = client
                .query(
                    &format!("SELECT migration, CAST(batch AS BIGINT) FROM {table}"),
                    &[],
                )
                .map_err(|e| e.to_string())?;
            Ok(rows
                .iter()
                .map(|r| (r.get::<_, String>(0), r.get::<_, i64>(1)))
                .collect())
        }
        "mysql" | "mariadb" => {
            use mysql::prelude::Queryable;
            let url = url.replacen("mariadb://", "mysql://", 1);
            let opts = mysql::Opts::from_url(&url).map_err(|e| e.to_string())?;
            let opts = mysql::OptsBuilder::from_opts(opts)
                .tcp_connect_timeout(Some(TIMEOUT))
                .read_timeout(Some(TIMEOUT));
            let mut conn = mysql::Conn::new(opts).map_err(|e| e.to_string())?;
            conn.query_drop("SET SESSION TRANSACTION READ ONLY")
                .map_err(|e| e.to_string())?;
            conn.query::<(String, i64), _>(format!(
                "SELECT migration, CAST(batch AS SIGNED) FROM {table}"
            ))
            .map_err(|e| e.to_string())
        }
        "sqlite" | "file" => {
            let path = url.split_once("://").map_or(url, |(_, p)| p);
            let conn = rusqlite::Connection::open_with_flags(
                path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(|e| e.to_string())?;
            let mut stmt = conn
                .prepare(&format!("SELECT migration, batch FROM {table}"))
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                .map_err(|e| e.to_string())?;
            rows.collect::<rusqlite::Result<_>>()
                .map_err(|e| e.to_string())
        }
        other => Err(format!(
            "unsupported URL scheme `{other}` (use postgres://, mysql://, mariadb:// or sqlite://)"
        )),
    }
}

/// Runs the query on its own thread so an unreachable server can't stall the answer.
fn read_with_timeout(url: &str, table: &str) -> Result<Vec<(String, i64)>, String> {
    let (tx, rx) = mpsc::channel();
    let (u, t) = (url.to_owned(), table.to_owned());
    std::thread::spawn(move || {
        let _ = tx.send(read_rows(&u, &t));
    });
    rx.recv_timeout(TIMEOUT + Duration::from_secs(2))
        .map_err(|_| format!("no answer within {} s", TIMEOUT.as_secs() + 2))?
}

/// The status of every configured database, for `get_schema` with `migrations: true`.
pub fn status(root: &Path, files: &[String], configured: &[DatabaseConfig], full: bool) -> Value {
    let env: Vec<(String, String)> = std::env::vars().collect();
    status_with(root, files, configured, full, &env)
}

fn status_with(
    root: &Path,
    files: &[String],
    configured: &[DatabaseConfig],
    full: bool,
    env: &[(String, String)],
) -> Value {
    let names = migration_names(files);
    if names.is_empty() {
        // Not a Laravel app with migrations (e.g. a frontend in a workspace).
        return json!({ "found": false, "migration_files": 0 });
    }
    let dbs = databases(configured, env);
    if dbs.is_empty() {
        return json!({
            "configured": false,
            "migration_files": names.len(),
            "hint": "No database to compare with. Add `[[databases]] name = \"local\"`, `env = \"CODEMAP_DB_LOCAL\"` to .codemap.toml (or just set CODEMAP_DB_LOCAL) and pass the variable to the container, e.g. -e CODEMAP_DB_LOCAL=postgres://user:pass@host:5432/db. codemap only reads the migrations table, read-only.",
        });
    }
    let limit = if full { 1000 } else { 30 };
    let list: Vec<Value> = dbs
        .iter()
        .map(|db| {
            let table = table_name(root, db);
            let Some(url) = env
                .iter()
                .find(|(k, v)| *k == db.env && !v.is_empty())
                .map(|(_, v)| v.clone())
            else {
                return json!({
                    "name": db.name,
                    "status": "not_configured",
                    "detail": format!("environment variable {} is not set in codemap's container", db.env),
                });
            };
            let target = describe_url(&url);
            if !valid_ident(&table) {
                return json!({ "name": db.name, "database": target, "status": "error", "detail": format!("invalid table name `{table}`") });
            }
            match read_with_timeout(&url, &table) {
                Err(e) => json!({
                    "name": db.name,
                    "database": target,
                    "status": "unreachable",
                    "detail": scrub(&e, &url),
                }),
                Ok(rows) => {
                    let ran: std::collections::HashSet<&str> =
                        rows.iter().map(|(m, _)| m.as_str()).collect();
                    let pending: Vec<&String> =
                        names.iter().filter(|n| !ran.contains(n.as_str())).collect();
                    let file_set: std::collections::HashSet<&str> =
                        names.iter().map(String::as_str).collect();
                    let mut missing: Vec<&str> = rows
                        .iter()
                        .map(|(m, _)| m.as_str())
                        .filter(|m| !file_set.contains(m))
                        .collect();
                    missing.sort();
                    let last_batch = rows.iter().map(|(_, b)| *b).max();
                    let matching = rows.len() - missing.len();
                    // Mostly unknown rows: probably another app's database.
                    let foreign = !rows.is_empty() && matching * 2 < rows.len();
                    let mut v = json!({
                        "name": db.name,
                        "database": target,
                        "status": if foreign { "other_app" } else if pending.is_empty() { "up_to_date" } else { "pending" },
                        "ran": rows.len(),
                        "last_batch": last_batch,
                        "pending_total": pending.len(),
                        "pending": pending.iter().take(limit).collect::<Vec<_>>(),
                    });
                    if foreign {
                        v["detail"] = json!(format!(
                            "only {matching} of the {} rows in `{table}` match this app's migration files: this looks like another app's database",
                            rows.len()
                        ));
                        v["pending"] = json!([]);
                        v["pending_total"] = Value::Null;
                    } else if !missing.is_empty() {
                        v["ran_without_file_total"] = json!(missing.len());
                        v["ran_without_file"] = json!(missing.iter().take(limit).collect::<Vec<_>>());
                    }
                    v
                }
            }
        })
        .collect();
    json!({
        "configured": true,
        "migration_files": names.len(),
        "databases": list,
        "note": "Read from each database's migrations table (one read-only SELECT). Pending = migration files the database has not run; ran_without_file = rows whose file is gone (squashed into a schema dump, or deleted).",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_never_show_credentials() {
        assert_eq!(
            describe_url("postgres://app:s3cret@db:5432/shop?sslmode=disable"),
            "postgres db:5432/shop"
        );
        assert_eq!(
            scrub(
                "password authentication failed for app with s3cret",
                "postgres://app:s3cret@db/x"
            ),
            "password authentication failed for *** with ***"
        );
        assert!(valid_ident("migrations"));
        assert!(!valid_ident("migrations; DROP TABLE x"));
    }

    #[test]
    fn sqlite_status() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("app.sqlite");
        {
            let c = rusqlite::Connection::open(&db).unwrap();
            c.execute_batch(
                "CREATE TABLE migrations (id INTEGER PRIMARY KEY, migration TEXT, batch INTEGER);
                 INSERT INTO migrations (migration, batch) VALUES
                   ('2024_01_01_000000_create_users_table', 1),
                   ('2023_01_01_000000_squashed_old', 1);",
            )
            .unwrap();
        }
        let env = vec![(
            "CODEMAP_TEST_DB_A".to_owned(),
            format!("sqlite://{}", db.display()),
        )];
        let files = vec![
            "database/migrations/2024_01_01_000000_create_users_table.php".to_owned(),
            "database/migrations/2024_02_01_000000_add_phone_to_users.php".to_owned(),
        ];
        let cfg = vec![DatabaseConfig {
            name: "local".into(),
            env: "CODEMAP_TEST_DB_A".into(),
            ..DatabaseConfig::default()
        }];
        let v = status_with(dir.path(), &files, &cfg, false, &env);
        let d = &v["databases"][0];
        assert_eq!(d["status"], "pending");
        assert_eq!(
            d["pending"],
            json!(["2024_02_01_000000_add_phone_to_users"])
        );
        assert_eq!(
            d["ran_without_file"],
            json!(["2023_01_01_000000_squashed_old"])
        );
        assert_eq!(d["last_batch"], 1);
        assert!(d["database"].as_str().unwrap().starts_with("sqlite "));

        let none = status_with(dir.path(), &files, &[], false, &[]);
        assert_eq!(none["configured"], false);
        // `CODEMAP_DB_STAGING` is found without configuration.
        let auto = databases(&[], &[("CODEMAP_DB_STAGING".into(), "mysql://x".into())]);
        assert_eq!(auto[0].name, "staging");
        // A configured variable that is not set.
        let unset = status_with(dir.path(), &files, &cfg, false, &[]);
        assert_eq!(unset["databases"][0]["status"], "not_configured");
    }
}
