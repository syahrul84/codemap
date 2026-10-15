//! HTTP calls found in the frontend, with the response fields each one reads
//! (script reads of the result, plus template reads of the state it is
//! stored in). Matching to backend routes happens at query time.

use pidx_core::Result;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::registry::Module;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallRow {
    pub file: String,
    pub line: u32,
    pub method: Option<String>,
    pub url: String,
    pub raw: String,
    pub via: String,
    pub base_var: bool,
    pub optional_tail: bool,
    pub external: bool,
    /// Paths read from the response (as written, e.g. `data.data.name`);
    /// see [`field_path`] for the field they name.
    pub fields: Vec<String>,
}

/// Segments that wrap a payload rather than name a field.
const WRAPPERS: &[&str] = &["data", "value", "body", "[]", "_data"];

/// Properties of JS values, not response fields.
const BUILTINS: &[&str] = &[
    "length",
    "map",
    "filter",
    "forEach",
    "find",
    "findIndex",
    "some",
    "every",
    "reduce",
    "slice",
    "includes",
    "indexOf",
    "push",
    "concat",
    "join",
    "sort",
    "toString",
    "toFixed",
    "then",
    "catch",
    "finally",
    "keys",
    "values",
    "entries",
    "trim",
    "split",
    "replace",
    "toLowerCase",
    "toUpperCase",
    "status",
    "statusText",
    "headers",
    "config",
    "ok",
    "json",
    "pending",
    "error",
    "refresh",
    "execute",
    "flatMap",
    "at",
    "pop",
    "shift",
    "splice",
];

/// `data.data.items[].name` → `items[].name`… minus wrappers → `items.name`;
/// `value.address.city` → `address.city`.
pub fn field_path(path: &str) -> Option<String> {
    let segs: Vec<&str> = path
        .split('.')
        .flat_map(|s| s.split("[]"))
        .filter(|s| !s.is_empty())
        .collect();
    let rest: Vec<&str> = segs
        .into_iter()
        .skip_while(|s| WRAPPERS.contains(s))
        .collect();
    let rest: Vec<&str> = rest.into_iter().filter(|s| !BUILTINS.contains(s)).collect();
    let first = rest.first()?;
    if first.starts_with('$') || first.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(rest.join("."))
}

fn state_name(target: &str) -> String {
    let t = target.trim_start_matches("this.");
    let t = t.strip_suffix(".value").unwrap_or(t);
    t.to_owned()
}

/// Test files (`tests/`, `__tests__`, `*.spec.*`, `*.test.*`): their HTTP
/// calls are mocks, not app traffic.
pub fn is_test_file(p: &str) -> bool {
    p.starts_with("tests/")
        || p.starts_with("test/")
        || p.contains("/tests/")
        || p.contains("/__tests__/")
        || p.contains(".spec.")
        || p.contains(".test.")
        || p.contains("/e2e/")
}

pub fn rows(modules: &[Module]) -> Vec<CallRow> {
    let mut out = Vec::new();
    for m in modules.iter().filter(|m| !is_test_file(&m.path)) {
        for c in &m.facts.script.api_calls {
            let mut paths: Vec<String> = c.reads.clone();
            for (target, from) in &c.stored_as {
                let name = state_name(target);
                for tp in &m.facts.template_paths {
                    let rest = tp
                        .strip_prefix(&format!("{name}[]."))
                        .or_else(|| tp.strip_prefix(&format!("{name}.")));
                    if let Some(rest) = rest {
                        paths.push(if from.is_empty() {
                            rest.to_owned()
                        } else {
                            format!("{from}.{rest}")
                        });
                    }
                }
            }
            let mut fields: Vec<String> = paths
                .into_iter()
                .filter(|p| field_path(p).is_some())
                .collect();
            fields.sort();
            fields.dedup();
            out.push(CallRow {
                file: m.path.clone(),
                line: c.line,
                method: c.method.clone(),
                url: c.url.clone(),
                raw: c.raw.clone(),
                via: c.via.clone(),
                base_var: c.base_var,
                optional_tail: c.optional_tail,
                external: c.external,
                fields,
            });
        }
    }
    out
}

pub fn store(tx: &rusqlite::Transaction<'_>, rows: &[CallRow]) -> Result<()> {
    tx.execute("DELETE FROM api_calls", [])?;
    let mut ins = tx.prepare(
        "INSERT INTO api_calls (file, line, method, url, raw, via, base_var, external, fields_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for r in rows {
        ins.execute(params![
            r.file,
            r.line,
            r.method,
            r.url,
            r.raw,
            r.via,
            i64::from(r.base_var) | (i64::from(r.optional_tail) << 1),
            r.external,
            (!r.fields.is_empty()).then(|| serde_json::to_string(&r.fields).unwrap_or_default())
        ])?;
    }
    Ok(())
}

pub fn load(conn: &rusqlite::Connection) -> Result<Vec<CallRow>> {
    let mut stmt = conn.prepare(
        "SELECT file, line, method, url, raw, via, base_var, external, fields_json FROM api_calls ORDER BY file, line",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(CallRow {
            file: r.get(0)?,
            line: r.get(1)?,
            method: r.get(2)?,
            url: r.get(3)?,
            raw: r.get(4)?,
            via: r.get(5)?,
            base_var: r.get::<_, i64>(6)? & 1 == 1,
            optional_tail: r.get::<_, i64>(6)? & 2 == 2,
            external: r.get(7)?,
            fields: r
                .get::<_, Option<String>>(8)?
                .and_then(|j| serde_json::from_str(&j).ok())
                .unwrap_or_default(),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_paths() {
        assert_eq!(field_path("data.data.name").as_deref(), Some("name"));
        assert_eq!(
            field_path("value.address.city").as_deref(),
            Some("address.city")
        );
        assert_eq!(
            field_path("data.items[].name").as_deref(),
            Some("items.name")
        );
        assert_eq!(field_path("data.data.length"), None);
        assert_eq!(field_path("data"), None);
        assert_eq!(field_path("data.meta.total").as_deref(), Some("meta.total"));
    }
}
