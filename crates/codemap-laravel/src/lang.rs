//! Laravel translations: keys from `lang/` (or `resources/lang/`) PHP arrays
//! and JSON files, and keys used through `__()`, `trans()`, `trans_choice()`,
//! `@lang()` and `Lang::get()` in PHP and Blade files.

use std::path::Path;

use codemap_core::{Result, ScanReport, Store};
use codemap_php::Scope;
use codemap_php::ast::named_children;
use rusqlite::{TransactionBehavior, params};

use crate::literal::{self, Lit};

/// `(locale, keys)` of one lang file.
type LocaleKeys = (String, Vec<String>);

/// Extractor name for per-file translation uses.
pub const USES_EXTRACTOR: &str = "php-i18n";
pub const USES_VERSION: i64 = 3;

/// Bump when the keys read from lang files change (rebuilds them once).
const KEYS_VERSION: &str = "2";
const KEYS_META: &str = "laravel_lang_keys_version";

/// `(locale, namespace)` of a Laravel lang file: `lang/en/auth.php` →
/// (`en`, `auth`); `lang/en.json` → (`en`, ``); `lang/vendor/pkg/en/x.php` → (`en`, `pkg::x`).
pub fn lang_file(path: &str) -> Option<(String, String)> {
    let rest = path
        .strip_prefix("lang/")
        .or_else(|| path.strip_prefix("resources/lang/"))?;
    if let Some(stem) = rest.strip_suffix(".json") {
        return (!stem.contains('/')).then(|| (stem.to_owned(), String::new()));
    }
    let rest = rest.strip_suffix(".php")?;
    let (pkg, rest) = match rest.strip_prefix("vendor/") {
        Some(r) => {
            let (pkg, r) = r.split_once('/')?;
            (Some(pkg), r)
        }
        None => (None, rest),
    };
    let (locale, file) = rest.split_once('/')?;
    Some((
        locale.to_owned(),
        match pkg {
            Some(p) => format!("{p}::{file}"),
            None => file.to_owned(),
        },
    ))
}

fn flatten(v: &Lit, prefix: &str, out: &mut Vec<String>) {
    match v {
        Lit::Arr(items) if !items.is_empty() => {
            for (k, child) in items {
                let Some(k) = k else { continue };
                flatten(child, &format!("{prefix}.{k}"), out);
            }
        }
        // `'attributes' => []`: an empty group defines no text.
        Lit::Arr(_) => {}
        _ => out.push(prefix.to_owned()),
    }
}

/// Keys defined by one lang file.
pub fn keys_of(root: &Path, path: &str) -> Option<LocaleKeys> {
    let (locale, ns) = lang_file(path)?;
    let src = std::fs::read(root.join(path)).ok()?;
    if path.ends_with(".json") {
        let v: serde_json::Value = serde_json::from_slice(&src).ok()?;
        return Some((locale, v.as_object()?.keys().cloned().collect()));
    }
    let tree = codemap_php::parse::parse(&src)?;
    let ret = named_children(tree.root_node())
        .into_iter()
        .find(|n| n.kind() == "return_statement")
        .and_then(|r| named_children(r).into_iter().next())?;
    let value = literal::eval(ret, &src, &Scope::default());
    let mut keys = Vec::new();
    match &value {
        Lit::Arr(items) => {
            for (k, child) in items {
                if let Some(k) = k {
                    flatten(child, &format!("{ns}.{k}"), &mut keys);
                }
            }
        }
        _ => return None,
    }
    Some((locale, keys))
}

/// Keys the code checks before relying on them, supplying its own text when
/// missing: `__('k') === 'k' ? 'Text' : …`, `Lang::has('k')`, `trans()->has('k')`.
pub fn guarded_keys(src: &str) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let compact: String = src.chars().filter(|c| !c.is_whitespace()).collect();
    for (k, _, _) in uses_in(src) {
        for q in ['\'', '"'] {
            let lit = format!("{q}{k}{q}");
            let checks = [
                format!("==={lit}"),
                format!("=={lit}"),
                format!("!=={lit}"),
                format!("!={lit}"),
                format!("has({lit}"),
                format!("hasForLocale({lit}"),
                format!("exists({lit}"),
            ];
            if checks.iter().any(|c| compact.contains(c.as_str())) {
                out.insert(k.clone());
            }
        }
    }
    out
}

/// Keys used in PHP / Blade source: `(key, line, dynamic)`.
pub fn uses_in(src: &str) -> Vec<(String, u32, bool)> {
    const CALLS: &[&str] = &[
        "__(",
        "trans(",
        "trans_choice(",
        "@lang(",
        "Lang::get(",
        "Lang::choice(",
        "@choice(",
    ];
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut line = 1u32;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\n' {
            line += 1;
            i += 1;
            continue;
        }
        // Every pattern starts with an ASCII byte; skip the rest (and the
        // middle of multi-byte characters).
        if !matches!(b[i], b'_' | b't' | b'@' | b'L') {
            i += 1;
            continue;
        }
        let rest = &src[i..];
        let hit = CALLS.iter().find(|c| rest.starts_with(**c));
        let boundary = i == 0
            || !(b[i - 1].is_ascii_alphanumeric()
                || b[i - 1] == b'_'
                || b[i - 1] == b'>'
                || b[i - 1] == b'$');
        if let (Some(c), true) = (hit, boundary) {
            let arg = rest[c.len()..].trim_start();
            if let Some(q) = arg.chars().next().filter(|c| matches!(c, '\'' | '"'))
                && let Some(end) = arg[1..].find(q)
            {
                let lit = &arg[1..1 + end];
                if q == '"' && lit.contains('$') {
                    let prefix = lit.split(['$', '{']).next().unwrap_or("");
                    if !prefix.is_empty() {
                        out.push((prefix.to_owned(), line, true));
                    }
                } else if lit.contains(['{', '|']) && !lit.contains(' ') {
                    // `notifications.{type}.{title|body}`: a placeholder in docs.
                } else if !lit.is_empty() {
                    out.push((lit.to_owned(), line, false));
                }
            }
            i += c.len();
            continue;
        }
        i += 1;
    }
    out
}

/// Re-reads lang files when one changed, and translation uses of pending PHP files.
pub fn refresh(store: &mut Store, root: &Path, scan: &ScanReport, full: bool) -> Result<()> {
    let lang_changed = full
        || scan
            .changed_paths
            .iter()
            .chain(&scan.removed_paths)
            .any(|p| lang_file(p).is_some());
    let has_keys: bool = store.conn().query_row(
        "SELECT EXISTS (SELECT 1 FROM i18n_keys WHERE source = 'laravel')",
        [],
        |r| r.get(0),
    )?;
    let lang_files: Vec<(i64, String)> = {
        let mut stmt = store.conn().prepare(
            "SELECT id, path FROM files WHERE path LIKE 'lang/%' OR path LIKE 'resources/lang/%'",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<(i64, String)>>>()?
            .into_iter()
            .filter(|(_, p)| lang_file(p).is_some())
            .collect()
    };
    let stale_keys = store.get_meta(KEYS_META)?.as_deref() != Some(KEYS_VERSION);
    if lang_changed || stale_keys || (!has_keys && !lang_files.is_empty()) {
        let parsed: Vec<(i64, Option<LocaleKeys>)> = lang_files
            .iter()
            .map(|(id, p)| (*id, keys_of(root, p)))
            .collect();
        let tx = store.conn_mut().transaction()?;
        tx.execute("DELETE FROM i18n_keys WHERE source = 'laravel'", [])?;
        codemap_core::store::set_meta(&tx, KEYS_META, KEYS_VERSION)?;
        {
            let mut ins = tx.prepare("INSERT INTO i18n_keys (file_id, source, locale, key) VALUES (?1, 'laravel', ?2, ?3)")?;
            for (id, keys) in &parsed {
                if let Some((locale, keys)) = keys {
                    for k in keys {
                        ins.execute(params![id, locale, k])?;
                    }
                }
            }
        }
        tx.commit()?;
    }

    // Uses: per file, like symbols.
    let tx = store
        .conn_mut()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let pending: Vec<(i64, String)> = {
        let mut stmt = tx.prepare(
            "SELECT f.id, f.path FROM files f WHERE f.lang = 'php' AND NOT EXISTS (
                 SELECT 1 FROM extractor_state e WHERE e.file_id = f.id AND e.extractor = ?1 AND e.version = ?2)",
        )?;
        let rows = stmt.query_map(params![USES_EXTRACTOR, USES_VERSION], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    // Read and scan in parallel; write in one go.
    use rayon::prelude::*;
    type Uses = Vec<(String, u32, bool, bool)>;
    let found: Vec<(i64, Uses)> = pending
        .par_iter()
        .map(|(id, path)| {
            let uses = if lang_file(path).is_none()
                && let Ok(src) = std::fs::read_to_string(root.join(path))
            {
                let guards = guarded_keys(&src);
                uses_in(&src)
                    .into_iter()
                    .map(|(key, line, dynamic)| {
                        let guarded = guards.contains(&key);
                        (key, line, dynamic, guarded)
                    })
                    .collect()
            } else {
                Vec::new()
            };
            (*id, uses)
        })
        .collect();
    {
        let mut del =
            tx.prepare_cached("DELETE FROM i18n_uses WHERE file_id = ?1 AND source = 'laravel'")?;
        let mut ins = tx.prepare_cached(
            "INSERT INTO i18n_uses (file_id, source, key, line, dynamic, guarded) VALUES (?1, 'laravel', ?2, ?3, ?4, ?5)",
        )?;
        let mut done = tx.prepare_cached(
            "INSERT OR REPLACE INTO extractor_state (file_id, extractor, version) VALUES (?1, ?2, ?3)",
        )?;
        for (id, uses) in &found {
            del.execute([id])?;
            for (key, line, dynamic, guarded) in uses {
                ins.execute(params![id, key, line, dynamic, guarded])?;
            }
            done.execute(params![id, USES_EXTRACTOR, USES_VERSION])?;
        }
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_files_and_uses() {
        assert_eq!(
            lang_file("lang/en/auth.php"),
            Some(("en".into(), "auth".into()))
        );
        assert_eq!(
            lang_file("lang/ms.json"),
            Some(("ms".into(), String::new()))
        );
        assert_eq!(
            lang_file("resources/lang/en/admin/users.php"),
            Some(("en".into(), "admin/users".into()))
        );
        assert_eq!(
            lang_file("lang/vendor/backup/en/notifications.php"),
            Some(("en".into(), "backup::notifications".into()))
        );
        assert_eq!(lang_file("app/lang.php"), None);
        assert_eq!(
            uses_in("// café — ok __('x.y')"),
            [("x.y".to_owned(), 1, false)]
        );
        let src = "<?php\n$a = __('auth.failed');\n{{ trans(\"orders.status.$s\") }}\n@lang('menu.home') $x->__('no') trans_choice('items.count', 2)";
        assert_eq!(
            uses_in(src),
            [
                ("auth.failed".to_owned(), 2, false),
                ("orders.status.".to_owned(), 3, true),
                ("menu.home".to_owned(), 4, false),
                ("items.count".to_owned(), 4, false)
            ]
        );
    }

    #[test]
    fn php_lang_keys() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("lang/en")).unwrap();
        std::fs::write(dir.path().join("lang/en/orders.php"), "<?php\nreturn ['title' => 'Orders', 'status' => ['paid' => 'Paid', 'open' => 'Open'], 'empty' => []];\n").unwrap();
        let (locale, keys) = keys_of(dir.path(), "lang/en/orders.php").unwrap();
        assert_eq!(locale, "en");
        assert_eq!(
            keys,
            [
                "orders.title",
                "orders.status.paid",
                "orders.status.open",
                // `'empty' => []` defines no text: not a key a locale can miss.
            ]
        );
    }
}
