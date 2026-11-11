//! The check that runs after an AI edits a file: names the edit brought in
//! that do not exist in the project.
//!
//! `verify` answers when it is asked. This asks for the assistant, right
//! after each edit, and only about what the edit added: the references of
//! the file as it is now, minus those it already had. What was wrong before
//! the edit is not the edit's news.

use std::collections::BTreeSet;

use codemap_core::store::Store;
use serde_json::{Value, json};

use crate::tools::Project;
use crate::verify::{Claims, VerifyParams};

/// What the edit added, by list.
fn added(new: Vec<String>, old: &[String]) -> Vec<String> {
    new.into_iter().filter(|x| !old.contains(x)).collect()
}

/// `kind `item` does not exist (closest: …)` for each not-found result.
fn not_found(v: &Value, out: &mut Vec<String>) {
    for r in v["results"].as_array().into_iter().flatten() {
        if r["status"] != "not_found" {
            continue;
        }
        let mut line = format!(
            "{} `{}` does not exist",
            r["kind"].as_str().unwrap_or("name"),
            r["item"].as_str().unwrap_or_default()
        );
        if let Some(s) = r["suggestions"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(Value::as_str)
        {
            line.push_str(&format!(" (closest: `{s}`)"));
        }
        if !out.contains(&line) {
            out.push(line);
        }
    }
}

fn verify(store: &Store, project: &Project, claims: Claims, out: &mut Vec<String>) {
    if claims.is_empty() {
        return;
    }
    let params = VerifyParams {
        claims: Some(claims),
        ..VerifyParams::default()
    };
    if let Ok(v) = crate::verify::verify(store, project, params) {
        not_found(&v, out);
    }
}

fn php(store: &Store, project: &Project, old: &str, new: &str, out: &mut Vec<String>) {
    let (was, now) = (
        crate::verify::snippet_claims(old),
        crate::verify::snippet_claims(new),
    );
    // Calls a class makes on itself (`$this->total()`, `self::make()`): the
    // method must be one it has. Calls on other values are left out: what
    // class a variable holds is a guess once it comes from a call.
    let parsed = codemap_php::extract(new.as_bytes());
    let own: Vec<&str> = parsed
        .symbols
        .iter()
        // A trait or an abstract class calls methods its users provide.
        .filter(|s| {
            s.kind.is_class_like()
                && s.kind != codemap_core::symbols::SymbolKind::Trait
                && !s.is_abstract
        })
        .map(|s| s.fqn.as_str())
        .collect();
    let calls = |refs: &[codemap_php::refs::Ref], src: &str| -> BTreeSet<String> {
        let rebound = rebound_lines(src);
        refs.iter()
            .filter(|r| r.kind == "call")
            .filter(|r| {
                r.target.split_once("::").is_some_and(|(class, method)| {
                    own.contains(&class)
                        // `$this->$handler()` and the like name no method.
                        && !method.is_empty()
                        && method.chars().all(|c| c.is_alphanumeric() || c == '_')
                })
            })
            // In a closure or an anonymous class `$this` may be another object.
            .filter(|r| !rebound.iter().any(|(a, b)| *a <= r.line && r.line <= *b))
            .map(|r| r.target.clone())
            .collect()
    };
    let before = calls(&codemap_php::extract(old.as_bytes()).refs, old);
    let mut methods = added(now.methods, &was.methods);
    for call in calls(&parsed.refs, new).difference(&before) {
        if !methods.contains(call) {
            methods.push(call.clone());
        }
    }
    // Functions are left out: PHP's own and the packages' helpers are not
    // all known, so a wrong one cannot be told from a built-in.
    let claims = Claims {
        classes: added(now.classes, &was.classes),
        methods,
        routes: added(now.routes, &was.routes),
        tables: added(now.tables, &was.tables),
        model_columns: now
            .model_columns
            .into_iter()
            .filter(|x| !was.model_columns.contains(x))
            .collect(),
        qualified_columns: added(now.qualified_columns, &was.qualified_columns),
        ..Claims::default()
    };
    let mut found = Vec::new();
    verify(store, project, claims, &mut found);
    // A column the query names itself (`count(*) as total`) is not a table's
    // column, and after a join an unqualified column may be the other table's.
    let lower = new.to_lowercase();
    // `->join('orders', …)`: a table, not `collect($x)->join(', ')`.
    let joins = [
        "->join(",
        "->leftjoin(",
        "->rightjoin(",
        "->crossjoin(",
        "->joinsub(",
        "->leftjoinsub(",
    ]
    .iter()
    .any(|j| {
        lower.match_indices(j).any(|(i, _)| {
            let mut rest = lower[i + j.len()..].chars();
            j.ends_with("sub(")
                || (matches!(rest.next(), Some('\'' | '"'))
                    && rest.next().is_some_and(|c| c.is_ascii_alphabetic()))
        })
    });
    found.retain(|f| {
        // `Schema::hasTable('legacy')` asks whether the table is there: code
        // that asks is ready for it to be missing.
        if let Some(table) = f.strip_prefix("table `").and_then(|r| r.split('`').next()) {
            let t = table.to_lowercase();
            return !(lower.contains(&format!("hastable('{t}'"))
                || lower.contains(&format!("hastable(\"{t}\"")));
        }
        let Some(item) = f.strip_prefix("column `").and_then(|r| r.split('`').next()) else {
            return true;
        };
        let (owner, column) = item.rsplit_once('.').unwrap_or(("", item));
        let alias = format!(" as {}", column.to_lowercase());
        // Inside a string (`'count(*) as total'`), not `use X as Total;`.
        let named_here = lower.match_indices(&alias).any(|(i, _)| {
            let line_start = lower[..i].rfind('\n').map_or(0, |p| p + 1);
            let quotes = lower[line_start..i]
                .chars()
                .filter(|c| matches!(c, '\'' | '"'))
                .count();
            quotes % 2 == 1
                && !lower[i + alias.len()..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_')
        });
        // `->addSelect(['is_read' => …])`, `->selectSub($q, 'is_read')`.
        let col = column.to_lowercase();
        let selected_as = (lower.contains("addselect(") || lower.contains("selectsub("))
            && (lower.contains(&format!("'{col}'")) || lower.contains(&format!("\"{col}\"")));
        let by_model = owner.chars().next().is_some_and(char::is_uppercase);
        !named_here && !selected_as && !(joins && by_model)
    });
    out.extend(found);
}

/// The lines of closures and anonymous classes, first and last.
fn rebound_lines(src: &str) -> Vec<(u32, u32)> {
    let Some(tree) = codemap_php::parse::parse(src.as_bytes()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if n.kind().contains("anonymous") || n.kind() == "arrow_function" {
            out.push((
                n.start_position().row as u32 + 1,
                n.end_position().row as u32 + 1,
            ));
        }
        let mut cursor = n.walk();
        stack.extend(n.named_children(&mut cursor));
    }
    out
}

fn blade(store: &Store, project: &Project, old: &str, new: &str, out: &mut Vec<String>) {
    use codemap_php::blade::facts;
    let names = |src: &str| -> BTreeSet<String> {
        let f = facts(src);
        f.extends
            .into_iter()
            .chain(f.includes)
            .map(|n| n.name)
            // A package's view, or a name built at run time: not ours to check.
            .filter(|n| !n.contains("::") && !n.ends_with('*'))
            .collect()
    };
    let was = names(old);
    let conn = store.conn();
    for name in names(new).difference(&was) {
        // `@includeIf` and `@includeFirst` are for views that may be missing.
        if !must_exist(new, name) {
            continue;
        }
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM views WHERE framework = 'blade' AND name = ?1 COLLATE NOCASE)",
                [name],
                |r| r.get(0),
            )
            .unwrap_or(true);
        if exists {
            continue;
        }
        let mut line = format!("view `{name}` does not exist");
        if let Some(s) = codemap_laravel::blade::suggestions(conn, name)
            .ok()
            .and_then(|s| s.into_iter().next())
        {
            line.push_str(&format!(" (closest: `{s}`)"));
        }
        out.push(line);
    }
    let routes = |src: &str| -> Vec<String> {
        codemap_php::refs::blade_refs(src)
            .into_iter()
            .filter(|r| r.kind == "route")
            .map(|r| r.target)
            .collect()
    };
    let claims = Claims {
        routes: added(routes(new), &routes(old)),
        ..Claims::default()
    };
    verify(store, project, claims, out);
}

/// Whether the template names `view` in a directive that fails when the
/// view is missing (`@include`, `@extends`, `@each`, …), not only in
/// `@includeIf` / `@includeFirst`.
fn must_exist(src: &str, view: &str) -> bool {
    let slashed = view.replace('.', "/");
    let mut strict = false;
    for name in [view, slashed.as_str()] {
        for quote in ['\'', '"'] {
            let needle = format!("{quote}{name}{quote}");
            let mut from = 0;
            while let Some(at) = src
                .get(from..)
                .and_then(|s| s.find(&needle))
                .map(|i| i + from)
            {
                from = at + needle.len();
                // The directive this name belongs to: the last `@word(` before it.
                let directive = src[..at]
                    .rfind('@')
                    .map(|i| {
                        src[i + 1..at]
                            .chars()
                            .take_while(|c| c.is_ascii_alphabetic())
                            .collect::<String>()
                    })
                    .unwrap_or_default();
                if !matches!(directive.as_str(), "includeIf" | "includeFirst") {
                    strict = true;
                }
            }
        }
    }
    strict
}

/// Line numbers of `new` whose text `old` did not have.
fn changed_lines(old: &str, new: &str) -> BTreeSet<u32> {
    let had: BTreeSet<&str> = old.lines().map(str::trim).collect();
    new.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty() && !had.contains(l.trim()))
        .map(|(i, _)| i as u32 + 1)
        .collect()
}

fn line_of(at: &str) -> Option<u32> {
    at.rsplit(':').next()?.trim().parse().ok()
}

fn frontend(
    store: &Store,
    project: &Project,
    file: &str,
    old: &str,
    new: &str,
    out: &mut Vec<String>,
) {
    let lines = changed_lines(old, new);
    if lines.is_empty() {
        return;
    }
    // What the file called and translated before the edit, read the same way.
    let was = codemap_vue::extract::extract_file(file, old);
    let old_urls: BTreeSet<&str> = was
        .script
        .api_calls
        .iter()
        .map(|c| c.url.as_str())
        .collect();
    let old_keys: BTreeSet<&str> = was
        .script
        .i18n_uses
        .iter()
        .chain(&was.template_i18n)
        .map(|u| u.key.as_str())
        .collect();
    // Calls to the backend that no route serves.
    let params = serde_json::from_value(json!({ "file": file, "limit": 100 }));
    if let Ok(p) = params
        && let Ok(answer) = crate::api::api_usage(store, project, p)
        && answer.data["backends"]
            .as_array()
            .is_some_and(|b| !b.is_empty())
    {
        for c in answer.data["calls"].as_array().into_iter().flatten() {
            let at = c["at"].as_str().unwrap_or_default();
            // `at` is `file:line`; the file may only be part of another's path.
            if !at.starts_with(&format!("{file}:"))
                || !line_of(at).is_some_and(|l| lines.contains(&l))
            {
                continue;
            }
            let call = c["call"].as_str().unwrap_or_default();
            // A call the file already made (only its options changed on this
            // line) is not the edit's.
            let path = call.split_once(' ').map_or(call, |(_, p)| p);
            if old_urls.contains(path) {
                continue;
            }
            match c["status"].as_str().unwrap_or_default() {
                "missing" => out.push(format!(
                    "endpoint `{call}` (line {}) matches no backend route",
                    line_of(at).unwrap_or(0)
                )),
                "method_mismatch" => {
                    let as_route: Vec<&str> = c["path_exists_as"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .take(2)
                        .collect();
                    out.push(format!(
                        "endpoint `{call}` (line {}): the route exists with another method{}",
                        line_of(at).unwrap_or(0),
                        if as_route.is_empty() {
                            String::new()
                        } else {
                            format!(" (`{}`)", as_route.join("`, `"))
                        }
                    ));
                }
                _ => {}
            }
        }
    }
    // Translation keys no locale defines.
    if let Ok(keys) = crate::i18n::file_keys(store.conn(), &project.config.i18n.locales, file) {
        for k in keys["undefined"].as_array().into_iter().flatten() {
            let Some((key, line)) = k.as_str().and_then(|s| s.rsplit_once(" :")) else {
                continue;
            };
            if line.parse().is_ok_and(|l: u32| lines.contains(&l))
                && !key.ends_with('…')
                && !old_keys.contains(key)
            {
                out.push(format!(
                    "translation key `{key}` (line {line}) is not defined in any locale"
                ));
            }
        }
    }
}

/// What an edit of `file` (a path from the project root) brought in that
/// does not exist: classes, methods, functions, routes, tables and columns
/// in PHP; views and route names in Blade; endpoints and translation keys in
/// Vue / JS / TS. `old` is the file before the edit, `new` after; the index
/// must already hold `new`. Never fails: a check that cannot run says nothing.
pub fn check_edit(
    store: &Store,
    project: &Project,
    file: &str,
    old: &str,
    new: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    if old == new {
        return out;
    }
    let lower = file.to_lowercase();
    if lower.ends_with(".blade.php") {
        blade(store, project, old, new, &mut out);
    } else if lower.ends_with(".php") {
        php(store, project, old, new, &mut out);
    } else if [
        ".vue", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".mts", ".cjs", ".cts",
    ]
    .iter()
    .any(|e| lower.ends_with(e))
    {
        frontend(store, project, file, old, new, &mut out);
    }
    out.truncate(40);
    out
}

/// The file as it was before an Edit: `new_string` put back to `old_string`
/// (every occurrence when the edit replaced all). `None` when the new text
/// is not in the file, so the old one cannot be known.
pub fn before_edit(now: &str, old_string: &str, new_string: &str, all: bool) -> Option<String> {
    if new_string.is_empty() {
        // A deletion adds nothing.
        return Some(now.to_owned());
    }
    if !now.contains(new_string) {
        return None;
    }
    Some(if all {
        now.replace(new_string, old_string)
    } else {
        now.replacen(new_string, old_string, 1)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_before_an_edit() {
        assert_eq!(
            before_edit("a\nNEW\nb\nNEW\n", "old", "NEW", false).as_deref(),
            Some("a\nold\nb\nNEW\n")
        );
        assert_eq!(
            before_edit("a\nNEW\nb\nNEW\n", "old", "NEW", true).as_deref(),
            Some("a\nold\nb\nold\n")
        );
        assert_eq!(before_edit("abc", "x", "missing", false), None);
        assert_eq!(
            before_edit("abc", "gone", "", false).as_deref(),
            Some("abc")
        );
        assert_eq!(changed_lines("a\nb\n", "a\nc\n\nb\n"), BTreeSet::from([2]));
        let src = "@includeIf('promo.banner')\n@includeFirst(['a.custom', 'a.default'])\n@include('a.row')\n@include(\"b/row\")";
        assert!(!must_exist(src, "promo.banner"));
        assert!(!must_exist(src, "a.custom") && !must_exist(src, "a.default"));
        assert!(must_exist(src, "a.row") && must_exist(src, "b.row"));
    }
}
