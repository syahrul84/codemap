//! Tables and columns read from a real database (`[[databases]] schema =
//! "live"`) or a structure dump (`dump = "…"`), merged over the schema the
//! migrations build. Columns say where they come from; where both sources
//! know a table, their differences are kept as drift.
//!
//! The connection URL comes only from the environment variable named in the
//! config; every read is a `SELECT` in a read-only session with a timeout.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use super::{Column, Schema, Table};
use crate::migration_status::scrub;

const TIMEOUT: Duration = Duration::from_secs(10);

/// One column as information_schema / pragma report it.
#[derive(Debug, Default)]
struct Raw {
    table: String,
    column: String,
    r#type: String,
    nullable: bool,
    default: Option<String>,
    primary: bool,
    unique: bool,
    indexed: bool,
    auto_increment: bool,
}

fn tables_of(raw: Vec<Raw>, fks: Vec<(String, String, String, String)>) -> Vec<Table> {
    let mut out: Vec<Table> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for r in raw {
        let i = *at.entry(r.table.clone()).or_insert_with(|| {
            out.push(Table {
                name: r.table.clone(),
                ..Table::default()
            });
            out.len() - 1
        });
        let unsigned = r.r#type.to_ascii_lowercase().contains("unsigned");
        out[i].columns.push(Column {
            name: r.column,
            r#type: r.r#type,
            nullable: r.nullable,
            default: r.default,
            unsigned,
            unique: r.unique,
            indexed: r.indexed || r.unique || r.primary,
            primary: r.primary,
            auto_increment: r.auto_increment,
            ..Column::default()
        });
    }
    for (t, c, ft, fc) in fks {
        if let Some(col) = at
            .get(&t)
            .and_then(|i| out[*i].columns.iter_mut().find(|x| x.name == c))
        {
            col.fk_table = Some(ft);
            col.fk_column = Some(fc);
            col.indexed = true;
        }
    }
    out
}

fn read_mysql(url: &str) -> Result<Vec<Table>, String> {
    use mysql::prelude::Queryable;
    let url = url.replacen("mariadb://", "mysql://", 1);
    let opts = mysql::Opts::from_url(&url).map_err(|e| e.to_string())?;
    let opts = mysql::OptsBuilder::from_opts(opts)
        .tcp_connect_timeout(Some(TIMEOUT))
        .read_timeout(Some(TIMEOUT));
    let mut conn = mysql::Conn::new(opts).map_err(|e| e.to_string())?;
    conn.query_drop("SET SESSION TRANSACTION READ ONLY")
        .map_err(|e| e.to_string())?;
    type Row = (
        String,
        String,
        String,
        String,
        Option<String>,
        String,
        String,
    );
    let rows: Vec<Row> = conn
        .query(
            "SELECT c.TABLE_NAME, c.COLUMN_NAME, c.COLUMN_TYPE, c.IS_NULLABLE, c.COLUMN_DEFAULT,
                    c.COLUMN_KEY, c.EXTRA
             FROM information_schema.COLUMNS c
             JOIN information_schema.TABLES t
               ON t.TABLE_SCHEMA = c.TABLE_SCHEMA AND t.TABLE_NAME = c.TABLE_NAME
             WHERE c.TABLE_SCHEMA = DATABASE() AND t.TABLE_TYPE = 'BASE TABLE'
             ORDER BY c.TABLE_NAME, c.ORDINAL_POSITION",
        )
        .map_err(|e| e.to_string())?;
    let fks: Vec<(String, String, String, String)> = conn
        .query(
            "SELECT TABLE_NAME, COLUMN_NAME, REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME
             FROM information_schema.KEY_COLUMN_USAGE
             WHERE TABLE_SCHEMA = DATABASE() AND REFERENCED_TABLE_NAME IS NOT NULL",
        )
        .map_err(|e| e.to_string())?;
    let raw = rows
        .into_iter()
        .map(|(table, column, ty, null, default, key, extra)| Raw {
            table,
            column,
            r#type: ty,
            nullable: null == "YES",
            default,
            primary: key == "PRI",
            unique: key == "UNI",
            indexed: key == "MUL",
            auto_increment: extra.to_ascii_lowercase().contains("auto_increment"),
        })
        .collect();
    Ok(tables_of(raw, fks))
}

fn read_postgres(url: &str) -> Result<Vec<Table>, String> {
    let mut cfg: postgres::Config = url.parse().map_err(|e: postgres::Error| e.to_string())?;
    cfg.connect_timeout(TIMEOUT);
    let mut client = cfg.connect(postgres::NoTls).map_err(|e| e.to_string())?;
    client
        .batch_execute(&format!(
            "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY; SET statement_timeout = {}",
            TIMEOUT.as_millis()
        ))
        .map_err(|e| e.to_string())?;
    let rows = client
        .query(
            "SELECT c.table_name::text, c.column_name::text,
                    CASE WHEN c.character_maximum_length IS NOT NULL
                         THEN c.data_type::text || '(' || c.character_maximum_length || ')'
                         ELSE c.data_type::text END,
                    c.is_nullable::text, c.column_default::text, c.is_identity::text
             FROM information_schema.columns c
             JOIN information_schema.tables t
               ON t.table_schema = c.table_schema AND t.table_name = c.table_name
             WHERE c.table_schema = current_schema() AND t.table_type = 'BASE TABLE'
             ORDER BY c.table_name, c.ordinal_position",
            &[],
        )
        .map_err(|e| e.to_string())?;
    let keys = client
        .query(
            "SELECT kcu.table_name::text, kcu.column_name::text, tc.constraint_type::text,
                    ccu.table_name::text, ccu.column_name::text,
                    (SELECT COUNT(*) FROM information_schema.key_column_usage k2
                      WHERE k2.constraint_name = tc.constraint_name
                        AND k2.table_schema = tc.table_schema)
             FROM information_schema.table_constraints tc
             JOIN information_schema.key_column_usage kcu
               ON kcu.constraint_name = tc.constraint_name AND kcu.table_schema = tc.table_schema
             LEFT JOIN information_schema.constraint_column_usage ccu
               ON tc.constraint_type = 'FOREIGN KEY'
              AND ccu.constraint_name = tc.constraint_name AND ccu.table_schema = tc.table_schema
             WHERE tc.table_schema = current_schema()",
            &[],
        )
        .map_err(|e| e.to_string())?;
    let mut kinds: HashMap<(String, String), Vec<String>> = HashMap::new();
    let mut fks = Vec::new();
    for k in &keys {
        let (t, c, kind): (String, String, String) = (k.get(0), k.get(1), k.get(2));
        let parts: i64 = k.get(5);
        if kind == "FOREIGN KEY" {
            if let (Some(ft), Some(fc)) =
                (k.get::<_, Option<String>>(3), k.get::<_, Option<String>>(4))
            {
                fks.push((t.clone(), c.clone(), ft, fc));
            }
        } else if parts == 1 || kind == "PRIMARY KEY" {
            kinds.entry((t, c)).or_default().push(kind);
        }
    }
    let raw = rows
        .iter()
        .map(|r| {
            let (table, column): (String, String) = (r.get(0), r.get(1));
            let default: Option<String> = r.get(4);
            let identity: Option<String> = r.get(5);
            let k = kinds.get(&(table.clone(), column.clone()));
            let has = |x: &str| k.is_some_and(|v| v.iter().any(|y| y == x));
            Raw {
                r#type: r.get(2),
                nullable: r.get::<_, String>(3) == "YES",
                auto_increment: identity.as_deref() == Some("YES")
                    || default
                        .as_deref()
                        .is_some_and(|d| d.starts_with("nextval(")),
                default: default.filter(|d| !d.starts_with("nextval(")),
                primary: has("PRIMARY KEY"),
                unique: has("UNIQUE"),
                indexed: false,
                table,
                column,
            }
        })
        .collect();
    Ok(tables_of(raw, fks))
}

fn read_sqlite(path: &str) -> Result<Vec<Table>, String> {
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| e.to_string())?;
    let e = |e: rusqlite::Error| e.to_string();
    let names: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .map_err(e)?
        .query_map([], |r| r.get(0))
        .map_err(e)?
        .collect::<rusqlite::Result<_>>()
        .map_err(e)?;
    let mut raw = Vec::new();
    let mut fks = Vec::new();
    for t in &names {
        let q = t.replace('"', "\"\"");
        // Single-column indexes: (column, unique).
        let mut idx: HashMap<String, bool> = HashMap::new();
        let list: Vec<(String, bool)> = conn
            .prepare(&format!("PRAGMA index_list(\"{q}\")"))
            .map_err(e)?
            .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, bool>(2)?)))
            .map_err(e)?
            .collect::<rusqlite::Result<_>>()
            .map_err(e)?;
        for (name, unique) in list {
            let cols: Vec<String> = conn
                .prepare(&format!(
                    "PRAGMA index_info(\"{}\")",
                    name.replace('"', "\"\"")
                ))
                .map_err(e)?
                .query_map([], |r| r.get::<_, String>(2))
                .map_err(e)?
                .collect::<rusqlite::Result<_>>()
                .map_err(e)?;
            if let [c] = cols.as_slice() {
                let u = idx.entry(c.clone()).or_insert(false);
                *u |= unique;
            }
        }
        let cols: Vec<(String, String, bool, Option<String>, i64)> = conn
            .prepare(&format!("PRAGMA table_info(\"{q}\")"))
            .map_err(e)?
            .query_map([], |r| {
                Ok((r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
            })
            .map_err(e)?
            .collect::<rusqlite::Result<_>>()
            .map_err(e)?;
        let pk_cols = cols.iter().filter(|c| c.4 > 0).count();
        for (column, ty, notnull, default, pk) in cols {
            let primary = pk > 0 && pk_cols == 1;
            raw.push(Raw {
                table: t.clone(),
                auto_increment: primary && ty.eq_ignore_ascii_case("integer"),
                unique: idx.get(&column).copied().unwrap_or(false),
                indexed: idx.contains_key(&column),
                column,
                r#type: ty,
                nullable: !notnull && !primary,
                default,
                primary,
            });
        }
        let list: Vec<(String, String, Option<String>)> = conn
            .prepare(&format!("PRAGMA foreign_key_list(\"{q}\")"))
            .map_err(e)?
            .query_map([], |r| Ok((r.get(2)?, r.get(3)?, r.get(4)?)))
            .map_err(e)?
            .collect::<rusqlite::Result<_>>()
            .map_err(e)?;
        for (ft, from, to) in list {
            fks.push((t.clone(), from, ft, to.unwrap_or_else(|| "id".into())));
        }
    }
    Ok(tables_of(raw, fks))
}

fn read_now(url: &str) -> Result<Vec<Table>, String> {
    let scheme = url.split("://").next().unwrap_or("").to_ascii_lowercase();
    match scheme.as_str() {
        "postgres" | "postgresql" => read_postgres(url),
        "mysql" | "mariadb" => read_mysql(url),
        "sqlite" | "file" => read_sqlite(url.split_once("://").map_or(url, |(_, p)| p)),
        other => Err(format!(
            "unsupported URL scheme `{other}` (use postgres://, mysql://, mariadb:// or sqlite://)"
        )),
    }
}

/// The database's tables, read on a thread of its own so an unreachable
/// server cannot stall a refresh. Errors never include the credentials.
pub fn read(url: &str) -> Result<Vec<Table>, String> {
    let (tx, rx) = mpsc::channel();
    let u = url.to_owned();
    std::thread::spawn(move || {
        let _ = tx.send(read_now(&u));
    });
    rx.recv_timeout(TIMEOUT + Duration::from_secs(2))
        .map_err(|_| format!("no answer within {} s", TIMEOUT.as_secs() + 2))?
        .map_err(|m| scrub(&m, url))
}

/// Puts `other` (from `source`, e.g. `live:local`) over the migrations'
/// schema: its definitions win; tables and columns only one side has are
/// kept and marked. With no migrations at all, nothing is marked as drift.
pub fn merge(schema: &mut Schema, other: Vec<Table>, source: &str) {
    let had_migrations = schema.tables.iter().any(|t| !t.dropped);
    let mut seen: Vec<String> = Vec::new();
    for mut t in other {
        seen.push(t.name.clone());
        t.source = Some(source.to_owned());
        for c in &mut t.columns {
            c.source = Some(source.to_owned());
        }
        match schema.table_mut(&t.name) {
            Some(mine) => {
                let mut cols: Vec<Column> = Vec::new();
                for mut c in t.columns {
                    match mine.columns.iter().find(|m| m.name == c.name) {
                        Some(m) => {
                            c.created_in = m.created_in.clone();
                            c.last_changed_in = m.last_changed_in.clone();
                            if m.nullable != c.nullable {
                                c.drift = Some(format!(
                                    "migrations make it {}",
                                    if m.nullable { "nullable" } else { "NOT NULL" }
                                ));
                            }
                        }
                        None => c.drift = Some("no migration creates this column".into()),
                    }
                    cols.push(c);
                }
                for m in &mine.columns {
                    if !cols.iter().any(|c| c.name == m.name) {
                        let mut m = m.clone();
                        m.drift = Some(format!("in migrations, not in {source}"));
                        cols.push(m);
                    }
                }
                mine.columns = cols;
                mine.source = Some(source.to_owned());
            }
            None => {
                if had_migrations {
                    t.drift = Some("no migration creates this table".into());
                }
                schema.tables.push(t);
            }
        }
    }
    for t in schema.tables.iter_mut().filter(|t| !t.dropped) {
        if !seen.contains(&t.name) {
            t.drift = Some(format!("in migrations, not in {source}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_tables_and_merge_with_migrations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.sqlite");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT NOT NULL UNIQUE, nick TEXT);
             CREATE TABLE posts (id INTEGER PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id), title TEXT NOT NULL);
             CREATE INDEX posts_title ON posts (title);",
        )
        .unwrap();
        drop(conn);
        let tables = read(&format!("sqlite://{}", path.display())).unwrap();
        let names: Vec<&str> = tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["posts", "users"]);
        let posts = &tables[0];
        let user_id = posts.columns.iter().find(|c| c.name == "user_id").unwrap();
        assert_eq!(user_id.fk_table.as_deref(), Some("users"));
        assert!(!user_id.nullable);
        assert!(
            posts
                .columns
                .iter()
                .find(|c| c.name == "title")
                .unwrap()
                .indexed
        );
        let users = &tables[1];
        assert!(users.columns[0].primary && users.columns[0].auto_increment);
        assert!(
            users
                .columns
                .iter()
                .find(|c| c.name == "email")
                .unwrap()
                .unique
        );

        // Migrations know users (with a column the database lacks) and an old table.
        let mut schema = Schema {
            tables: vec![
                Table {
                    name: "users".into(),
                    columns: vec![
                        Column {
                            name: "email".into(),
                            nullable: true,
                            created_in: Some("database/migrations/x.php:5".into()),
                            ..Column::default()
                        },
                        Column {
                            name: "avatar".into(),
                            ..Column::default()
                        },
                    ],
                    ..Table::default()
                },
                Table {
                    name: "legacy".into(),
                    ..Table::default()
                },
            ],
            ..Schema::default()
        };
        merge(&mut schema, tables, "live:local");
        let t = |n: &str| schema.tables.iter().find(|t| t.name == n).unwrap();
        let email = t("users")
            .columns
            .iter()
            .find(|c| c.name == "email")
            .unwrap();
        assert_eq!(email.source.as_deref(), Some("live:local"));
        assert_eq!(email.drift.as_deref(), Some("migrations make it nullable"));
        assert_eq!(
            email.created_in.as_deref(),
            Some("database/migrations/x.php:5")
        );
        let nick = t("users")
            .columns
            .iter()
            .find(|c| c.name == "nick")
            .unwrap();
        assert_eq!(
            nick.drift.as_deref(),
            Some("no migration creates this column")
        );
        let avatar = t("users")
            .columns
            .iter()
            .find(|c| c.name == "avatar")
            .unwrap();
        assert_eq!(
            avatar.drift.as_deref(),
            Some("in migrations, not in live:local")
        );
        assert_eq!(
            t("posts").drift.as_deref(),
            Some("no migration creates this table")
        );
        assert_eq!(
            t("legacy").drift.as_deref(),
            Some("in migrations, not in live:local")
        );
    }

    #[test]
    fn no_migrations_means_no_drift_and_errors_hide_credentials() {
        let mut schema = Schema::default();
        merge(
            &mut schema,
            vec![Table {
                name: "orders".into(),
                ..Table::default()
            }],
            "live:local",
        );
        assert_eq!(schema.tables[0].drift, None);
        let err = read("postgres://codemap:s3cretpass@127.0.0.1:1/x").unwrap_err();
        assert!(!err.contains("s3cretpass"), "{err}");
    }
}
