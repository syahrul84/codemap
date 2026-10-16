//! Cache map: every cache key (or key pattern, `brands:{}`) with where it is
//! written and for how long, where it is read, and where it is cleared —
//! from `Cache::remember/put/forever/get/forget/flush`, tagged caches and the
//! `cache()` helper. Two real bug classes this answers: a cached payload that
//! is missing a field (where is it built and how long does it live), and a
//! value that stays stale (is it ever cleared when its source changes).

use std::collections::BTreeMap;

use codemap_core::envelope::Detail;
use codemap_core::{Result, Store, symbols};
use serde_json::{Value, json};

#[derive(Default)]
struct Entry {
    writes: Vec<(String, String)>,
    reads: Vec<String>,
    forgets: Vec<String>,
}

/// `«const:App\X::KEY»:{}` → `popups:{}` (constants replaced by their literal value).
fn resolve(conn: &rusqlite::Connection, key: &str) -> String {
    let mut out = String::new();
    let mut rest = key;
    while let Some(i) = rest.find("«const:") {
        out.push_str(&rest[..i]);
        let after = &rest[i + "«const:".len()..];
        let Some(end) = after.find('»') else {
            out.push_str(&rest[i..]);
            return out;
        };
        let c = &after[..end];
        let value = symbols::by_fqn(conn, c)
            .ok()
            .and_then(|s| s.into_iter().next())
            .and_then(|s| s.meta.value)
            .map(|v| v.trim_matches(['\'', '"']).to_owned())
            .unwrap_or_else(|| c.rsplit('\\').next().unwrap_or(c).to_owned());
        out.push_str(&value);
        rest = &after[end + '»'.len_utf8()..];
    }
    out.push_str(rest);
    out
}

pub fn describe(store: &Store, prefix: Option<&str>, detail: Detail) -> Result<Value> {
    let conn = store.conn();
    let mut stmt = conn.prepare(
        "SELECT f.path, r.line, r.kind, r.target FROM refs r JOIN files f ON f.id = r.file_id
         WHERE r.kind IN ('cache_write', 'cache_read', 'cache_forget', 'cache_flush')
         ORDER BY f.path, r.line",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, u32>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    let mut keys: BTreeMap<String, Entry> = BTreeMap::new();
    let mut flushes: Vec<(String, String)> = Vec::new(); // (tags or *, at)
    for row in rows {
        let (file, line, kind, target) = row?;
        if file.starts_with("tests/") || file.contains("/tests/") || file.ends_with("Test.php") {
            continue;
        }
        let at = format!("{file}:{line}");
        if kind == "cache_flush" {
            flushes.push((target, at));
            continue;
        }
        let (raw_key, ttl) = match target.split_once('\t') {
            Some((k, t)) => (k.to_owned(), t.to_owned()),
            None => (target, String::new()),
        };
        let key = resolve(conn, &raw_key);
        if prefix.is_some_and(|p| !key.starts_with(p) && !key.contains(&format!("|{p}"))) {
            continue;
        }
        let e = keys.entry(key).or_default();
        match kind.as_str() {
            "cache_write" => e.writes.push((at, ttl)),
            "cache_read" => e.reads.push(at),
            _ => e.forgets.push(at),
        }
    }
    let flush_all = flushes
        .iter()
        .find(|(t, _)| t == "*")
        .map(|(_, at)| at.clone());
    let cleared_by_flush = |key: &str| -> Option<String> {
        if let Some((tags, _)) = key.split_once('|') {
            let tags = tags.trim_start_matches("tags:");
            for (t, at) in &flushes {
                let ft = t.trim_start_matches("tags:");
                if ft.split(',').any(|x| tags.split(',').any(|y| y == x)) {
                    return Some(at.clone());
                }
            }
        }
        flush_all.clone()
    };
    let mut problems: Vec<String> = Vec::new();
    let limit = if detail == Detail::Full { 1000 } else { 60 };
    let mut list: Vec<Value> = Vec::new();
    for (key, e) in &keys {
        let flushed = cleared_by_flush(key);
        let forever = e.writes.iter().any(|(_, t)| t == "forever");
        if !e.writes.is_empty() && e.forgets.is_empty() && flushed.is_none() && forever {
            problems.push(format!(
                "{key}: stored forever ({}) and never cleared",
                e.writes[0].0
            ));
        }
        if e.writes.is_empty() && !e.reads.is_empty() {
            problems.push(format!(
                "{key}: read ({}) but never written in this app",
                e.reads[0]
            ));
        }
        if e.writes.is_empty() && e.reads.is_empty() && !e.forgets.is_empty() {
            problems.push(format!(
                "{key}: cleared ({}) but never written: a typo, or another app's key",
                e.forgets[0]
            ));
        }
        if list.len() >= limit {
            continue;
        }
        if detail == Detail::Full {
            list.push(json!({
                "key": key,
                "written": e.writes.iter().map(|(at, ttl)| json!({ "at": at, "ttl": ttl })).collect::<Vec<_>>(),
                "read": e.reads,
                "cleared": e.forgets,
                "cleared_by_flush": flushed,
            }));
        } else {
            let mut s = key.clone();
            match e.writes.as_slice() {
                [] => {}
                [(at, ttl)] => s.push_str(&format!(" · written {at} (ttl {ttl})")),
                [(at, ttl), rest @ ..] => {
                    s.push_str(&format!(" · written {at} (ttl {ttl}) +{}", rest.len()))
                }
            }
            if let Some(r) = e.reads.first() {
                s.push_str(&format!(" · read {r}"));
                if e.reads.len() > 1 {
                    s.push_str(&format!(" +{}", e.reads.len() - 1));
                }
            }
            match (e.forgets.first(), &flushed) {
                (Some(f), _) => s.push_str(&format!(" · cleared {f}")),
                (None, Some(f)) => s.push_str(&format!(" · cleared by flush {f}")),
                (None, None) if !e.writes.is_empty() => {
                    s.push_str(" · never cleared (expires only)")
                }
                _ => {}
            }
            list.push(json!(s));
        }
    }
    let mut v = json!({
        "found": !keys.is_empty(),
        "keys_total": keys.len(),
        "keys": list,
        "note": "Keys with variable parts are shown as patterns ({}). TTLs as written. Pass key_prefix to narrow.",
    });
    if keys.len() > limit {
        v["truncated"] = json!(true);
    }
    if !problems.is_empty() {
        v["problems"] = json!(problems);
    }
    if !flushes.is_empty() {
        v["flushes"] = json!(
            flushes
                .iter()
                .map(|(t, at)| format!("{t} {at}"))
                .collect::<Vec<_>>()
        );
    }
    Ok(v)
}
