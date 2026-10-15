//! Settings and their defaults: `config/*.php` keys read from the environment
//! (`env('APP_TIMEZONE', 'UTC')`), and key/value settings read and written in
//! code (`Setting::get(self::KEY, self::DEFAULT)`, `setting('site.name', 'X')`),
//! with the Filament admin page that edits them. Values in `.env` are never read.

use std::collections::BTreeMap;
use std::path::Path;

use pidx_core::envelope::Detail;
use pidx_core::{Result, Store, symbols};
use pidx_php::Scope;
use pidx_php::ast::{named_children, text};
use rusqlite::Connection;
use serde_json::{Value, json};
use tree_sitter::Node;

use crate::literal::{self, Lit};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigEnv {
    pub key: String,
    pub env: String,
    pub default: String,
    pub at: String,
}

fn first_env<'t>(n: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    if n.kind() == "function_call_expression"
        && n.child_by_field_name("function")
            .map(|f| text(f, src).trim_start_matches('\\'))
            == Some("env")
    {
        return Some(n);
    }
    named_children(n)
        .into_iter()
        .find_map(|c| first_env(c, src))
}

fn walk_array(n: Node<'_>, src: &[u8], prefix: &str, file: &str, out: &mut Vec<ConfigEnv>) {
    for el in named_children(n) {
        if el.kind() != "array_element_initializer" {
            continue;
        }
        let parts = named_children(el);
        let [key, value] = parts.as_slice() else {
            continue;
        };
        let Some(k) = literal::eval(*key, src, &Scope::default())
            .as_str()
            .map(str::to_owned)
        else {
            continue;
        };
        let path = format!("{prefix}.{k}");
        if value.kind() == "array_creation_expression" {
            walk_array(*value, src, &path, file, out);
            continue;
        }
        let Some(env) = first_env(*value, src) else {
            continue;
        };
        let args: Vec<Node<'_>> = env
            .child_by_field_name("arguments")
            .map(named_children)
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a.kind() == "argument")
            .filter_map(|a| named_children(a).into_iter().last())
            .collect();
        let Some(name) = args.first().and_then(|a| {
            literal::eval(*a, src, &Scope::default())
                .as_str()
                .map(str::to_owned)
        }) else {
            continue;
        };
        let default = match args.get(1) {
            Some(d) => match literal::eval(*d, src, &Scope::default()) {
                Lit::Raw(_) => pidx_php::ast::truncate(&pidx_php::ast::compact(text(*d, src)), 60),
                other => other.display(),
            },
            None => "null".into(),
        };
        out.push(ConfigEnv {
            key: path,
            env: name,
            default,
            at: format!("{file}:{}", value.start_position().row + 1),
        });
    }
}

/// `config/<file>.php` entries backed by `env()`.
pub fn config_env(root: &Path, files: &[String]) -> Vec<ConfigEnv> {
    let mut out = Vec::new();
    for f in files
        .iter()
        .filter(|f| f.starts_with("config/") && f.ends_with(".php") && f.matches('/').count() == 1)
    {
        let Ok(src) = std::fs::read(root.join(f)) else {
            continue;
        };
        let Some(tree) = pidx_php::parse::parse(&src) else {
            continue;
        };
        let Some(ret) = named_children(tree.root_node())
            .into_iter()
            .find(|n| n.kind() == "return_statement")
            .and_then(|r| named_children(r).into_iter().next())
        else {
            continue;
        };
        if ret.kind() != "array_creation_expression" {
            continue;
        }
        let name = f.trim_start_matches("config/").trim_end_matches(".php");
        walk_array(ret, &src, name, f, &mut out);
    }
    out
}

/// `const:App\X::KEY` → the constant's literal value, unquoted.
fn resolve(conn: &Connection, v: &str) -> (String, Option<String>) {
    let Some(c) = v.strip_prefix("const:") else {
        return (v.strip_prefix("expr:").unwrap_or(v).to_owned(), None);
    };
    let value = symbols::by_fqn(conn, c)
        .ok()
        .and_then(|s| s.into_iter().next())
        .and_then(|s| s.meta.value)
        .map(|x| x.trim_matches(['\'', '"']).to_owned());
    let short = c.rsplit('\\').next().unwrap_or(c).to_owned();
    match value {
        Some(x) => (x, Some(short)),
        None => (short.clone(), Some(short)),
    }
}

#[derive(Default)]
struct Entry {
    defaults: Vec<String>,
    reads: Vec<String>,
    writes: Vec<String>,
}

pub fn describe(store: &Store, root: &Path, prefix: Option<&str>, detail: Detail) -> Result<Value> {
    let conn = store.conn();
    let files: Vec<String> = store.list_files()?.into_iter().map(|f| f.path).collect();
    let keep = |k: &str| prefix.is_none_or(|p| k.starts_with(p));
    let config: Vec<ConfigEnv> = config_env(root, &files)
        .into_iter()
        .filter(|c| keep(&c.key))
        .collect();

    let mut settings: BTreeMap<String, Entry> = BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT f.path, r.line, r.kind, r.target FROM refs r JOIN files f ON f.id = r.file_id
             WHERE r.kind IN ('setting_read', 'setting_write') ORDER BY f.path, r.line",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, u32>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (file, line, kind, target) = row?;
            // Tests read and write throwaway keys.
            if file.starts_with("tests/") || file.contains("/tests/") || file.ends_with("Test.php")
            {
                continue;
            }
            let (raw_key, raw_default) = match target.split_once('\t') {
                Some((k, d)) => (k.to_owned(), Some(d.to_owned())),
                None => (target, None),
            };
            let (key, _) = resolve(conn, &raw_key);
            if !keep(&key) {
                continue;
            }
            let e = settings.entry(key).or_default();
            let at = format!("{file}:{line}");
            if kind == "setting_write" {
                e.writes.push(at);
            } else {
                e.reads.push(at);
                if let Some(d) = raw_default {
                    let (value, constant) = resolve(conn, &d);
                    let shown = match constant {
                        Some(c) if c != value => format!("{value} ({c})"),
                        _ => value,
                    };
                    if !e.defaults.contains(&shown) {
                        e.defaults.push(shown);
                    }
                }
            }
        }
    }

    // Admin pages that edit a setting: the page writes it, or uses the class that does.
    let admin: Vec<(String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT route_path, file FROM pages WHERE source = 'filament' AND file IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let page_for = |writer_file: &str| -> Option<String> {
        if let Some((url, _)) = admin.iter().find(|(_, f)| f == writer_file) {
            return Some(url.clone());
        }
        let classes: Vec<String> = symbols::all(conn)
            .ok()?
            .into_iter()
            .filter(|s| s.file == writer_file && s.kind.is_class_like())
            .map(|s| s.fqn)
            .collect();
        for (url, page_file) in &admin {
            for c in &classes {
                let hit: bool = conn
                    .query_row(
                        "SELECT EXISTS (SELECT 1 FROM refs r JOIN files f ON f.id = r.file_id
                         WHERE f.path = ?1 AND (r.target = ?2 OR r.target LIKE ?2 || '::%'))",
                        [page_file, c],
                        |r| r.get(0),
                    )
                    .unwrap_or(false);
                if hit {
                    return Some(url.clone());
                }
            }
        }
        None
    };

    let limit = if detail == Detail::Full { 1000 } else { 80 };
    let setting_lines: Vec<Value> = settings
        .iter()
        .take(limit)
        .map(|(k, e)| {
            let pages: Vec<String> = {
                let mut v: Vec<String> = e.writes.iter().filter_map(|w| page_for(w.split(':').next().unwrap_or(w))).collect();
                v.dedup();
                v
            };
            if detail == Detail::Full {
                json!({ "key": k, "defaults": e.defaults, "read_at": e.reads, "written_at": e.writes, "admin_pages": pages })
            } else {
                let mut s = k.clone();
                if !e.defaults.is_empty() {
                    s.push_str(&format!(" = {}", e.defaults.join(" | ")));
                }
                if let Some(r) = e.reads.first() {
                    s.push_str(&format!(" · read {r}{}", if e.reads.len() > 1 { format!(" (+{})", e.reads.len() - 1) } else { String::new() }));
                }
                if let Some(w) = e.writes.first() {
                    s.push_str(&format!(" · written {w}"));
                }
                if !pages.is_empty() {
                    s.push_str(&format!(" · admin {}", pages.join(", ")));
                }
                json!(s)
            }
        })
        .collect();
    let config_lines: Vec<Value> = config
        .iter()
        .take(match (detail, prefix) {
            (Detail::Full, _) => 2000,
            (_, Some(_)) => 120,
            // Without a prefix the config list is long; settings matter most.
            _ => 40,
        })
        .map(|c| {
            if detail == Detail::Full {
                json!({ "key": c.key, "env": c.env, "default": c.default, "at": c.at })
            } else {
                json!(format!(
                    "{} = {} (env {}) {}",
                    c.key, c.default, c.env, c.at
                ))
            }
        })
        .collect();
    let mut v = json!({
        "found": !settings.is_empty() || !config.is_empty(),
        "settings_total": settings.len(),
        "settings": setting_lines,
        "config_total": config.len(),
        "config": config_lines,
        "note": "Defaults as written in code; values in .env and in the settings table are never read. Pass key_prefix to narrow.",
    });
    if settings.len() > limit || config.len() > config_lines.len() {
        v["truncated"] = json!(true);
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_env_entries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("config")).unwrap();
        std::fs::write(
            dir.path().join("config/app.php"),
            "<?php\nreturn [\n  'name' => env('APP_NAME', 'Laravel'),\n  'debug' => (bool) env('APP_DEBUG', false),\n  'fixed' => 'x',\n  'mail' => ['from' => env('MAIL_FROM')],\n  'tz' => env('APP_TZ', config('x.y')),\n];\n",
        )
        .unwrap();
        let got: Vec<String> = config_env(dir.path(), &["config/app.php".into()])
            .into_iter()
            .map(|c| format!("{} {} {} {}", c.key, c.env, c.default, c.at))
            .collect();
        assert_eq!(
            got,
            [
                "app.name APP_NAME 'Laravel' config/app.php:3",
                "app.debug APP_DEBUG false config/app.php:4",
                "app.mail.from MAIL_FROM null config/app.php:6",
                "app.tz APP_TZ config('x.y') config/app.php:7",
            ]
        );
    }
}
