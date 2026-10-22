//! One model of UI views across frameworks (Vue / React components, SwiftUI
//! views, Compose functions) and the edges "this view shows that one". Each
//! framework's adapter replaces its own rows; tools read them all alike.

use rusqlite::{Connection, params};
use serde::Serialize;

use crate::Result;

/// A view as written by an adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct View {
    pub file_id: i64,
    pub name: String,
    pub path: String,
    pub line: u32,
    /// component | page | layout | view | composable
    pub kind: String,
}

/// `from` shows `to_name` at `line`; `to_path` when it is a project view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Use {
    pub from_path: String,
    pub from_name: Option<String>,
    pub to_name: String,
    pub to_path: Option<String>,
    pub line: u32,
}

/// A stored view with its framework.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ViewRow {
    pub framework: String,
    pub name: String,
    pub path: String,
    pub line: u32,
    pub kind: Option<String>,
}

/// Replaces `framework`'s views and uses (inside the caller's transaction).
pub fn replace(conn: &Connection, framework: &str, views: &[View], uses: &[Use]) -> Result<()> {
    conn.execute("DELETE FROM views WHERE framework = ?1", [framework])?;
    conn.execute("DELETE FROM view_uses WHERE framework = ?1", [framework])?;
    {
        let mut ins = conn.prepare_cached(
            "INSERT INTO views (file_id, framework, name, path, line, kind) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for v in views {
            ins.execute(params![
                v.file_id, framework, v.name, v.path, v.line, v.kind
            ])?;
        }
    }
    let mut ins = conn.prepare_cached(
        "INSERT INTO view_uses (framework, from_path, from_name, to_name, to_path, line) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for u in uses {
        ins.execute(params![
            framework,
            u.from_path,
            u.from_name,
            u.to_name,
            u.to_path,
            u.line
        ])?;
    }
    Ok(())
}

fn rows(conn: &Connection, sql: &str, arg: &str) -> Result<Vec<ViewRow>> {
    let mut stmt = conn.prepare_cached(sql)?;
    let rows = stmt.query_map([arg], |r| {
        Ok(ViewRow {
            framework: r.get(0)?,
            name: r.get(1)?,
            path: r.get(2)?,
            line: r.get(3)?,
            kind: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Views with this name (case-insensitive), or at this path. `order-card`
/// also finds `OrderCard`; `Row` finds a nested `ContentView.Row`.
pub fn find(conn: &Connection, query: &str) -> Result<Vec<ViewRow>> {
    let q = query.trim();
    const COLS: &str = "SELECT framework, name, path, line, kind FROM views";
    let mut out = rows(
        conn,
        &format!("{COLS} WHERE name = ?1 COLLATE NOCASE ORDER BY path"),
        q,
    )?;
    if out.is_empty() && q.contains('-') {
        let pascal: String = q
            .split('-')
            .map(|p| {
                let mut c = p.chars();
                c.next()
                    .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                    .unwrap_or_default()
            })
            .collect();
        out = rows(
            conn,
            &format!("{COLS} WHERE name = ?1 COLLATE NOCASE ORDER BY path"),
            &pascal,
        )?;
    }
    if out.is_empty() {
        out = rows(conn, &format!("{COLS} WHERE path = ?1 ORDER BY line"), q)?;
    }
    if out.is_empty() {
        out = rows(
            conn,
            &format!("{COLS} WHERE name LIKE ?1 ESCAPE '!' ORDER BY path"),
            &format!("%.{}", q.replace(['!', '%', '_'], "!$0")),
        )?;
    }
    Ok(out)
}

/// Every view name, for suggestions.
pub fn names(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare_cached("SELECT DISTINCT name FROM views ORDER BY name")?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Whether any view of `framework` (or any at all) is stored.
pub fn any(conn: &Connection, framework: Option<&str>) -> Result<bool> {
    Ok(match framework {
        Some(f) => conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM views WHERE framework = ?1)",
            [f],
            |r| r.get(0),
        )?,
        None => conn.query_row("SELECT EXISTS (SELECT 1 FROM views)", [], |r| r.get(0))?,
    })
}

/// Places in other files that show the views of `path`.
pub fn shown_from(conn: &Connection, path: &str) -> Result<Vec<(String, u32, String)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT from_path, line, to_name FROM view_uses WHERE to_path = ?1 AND from_path != ?1 ORDER BY from_path, line",
    )?;
    let rows = stmt.query_map([path], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Views stored for `path`.
pub fn in_file(conn: &Connection, path: &str) -> Result<Vec<ViewRow>> {
    rows(
        conn,
        "SELECT framework, name, path, line, kind FROM views WHERE path = ?1 ORDER BY line",
        path,
    )
}
