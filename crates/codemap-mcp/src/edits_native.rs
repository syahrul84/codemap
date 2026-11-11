//! The check after an edit, for Swift, Kotlin and Rust.
//!
//! These languages lean on an SDK and on packages the index does not hold,
//! so most names an edit uses cannot be told from a mistake. What can be:
//! a name the code itself says is the project's own.
//!
//! - Rust: a path into one of the workspace's crates (`crate::scan::walk`,
//!   `codemap_core::Store`).
//! - Kotlin: an import from one of the project's packages, and a member of
//!   one of its enums (`Theme.DRAK`).
//! - Swift: a member of one of the project's enums, or of a struct or class
//!   that conforms to nothing (`Route.detial`, `Theme.primryColor`).
//!
//! As everywhere in the check: only what the edit added, and a name is a
//! finding only when nothing in the project declares it.

use std::collections::BTreeSet;
use std::path::Path;

use codemap_core::store::Store;
use rusqlite::Connection;

use crate::tools::Project;

/// The code with comments dropped and the inside of strings blanked, so a
/// name in a comment or a message is not read as code.
fn code_only(src: &str) -> String {
    let b: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let next = b.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                if b[i] == '\n' {
                    out.push('\n');
                }
                i += 1;
            }
            i += 2;
        } else if c == '"' {
            let triple = next == Some('"') && b.get(i + 2) == Some(&'"');
            let quote = if triple { 3 } else { 1 };
            i += quote;
            out.push('"');
            while i < b.len() {
                if b[i] == '\\' && !triple {
                    i += 2;
                    continue;
                }
                let closes = b[i] == '"'
                    && (!triple || (b.get(i + 1) == Some(&'"') && b.get(i + 2) == Some(&'"')));
                if closes {
                    i += quote;
                    break;
                }
                if b[i] == '\n' {
                    out.push('\n');
                }
                i += 1;
            }
            out.push('"');
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

fn ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `Type.member` uses of `ty` in `code`: the member names.
fn members_used(code: &str, ty: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (i, _) in code.match_indices(ty) {
        let before = code[..i].chars().next_back();
        if before.is_some_and(|c| ident(c) || c == '.') {
            continue;
        }
        let rest = &code[i + ty.len()..];
        let Some(rest) = rest.strip_prefix('.') else {
            continue;
        };
        let name: String = rest.chars().take_while(|c| ident(*c)).collect();
        if !name.is_empty() && !name.starts_with(|c: char| c.is_ascii_digit()) {
            out.insert(name);
        }
    }
    out
}

/// The closest of `names` to `name`, a letter or two away.
fn near<'a>(name: &str, names: impl IntoIterator<Item = &'a String>) -> Option<&'a String> {
    let l = name.to_lowercase();
    let allowed = if l.len() > 5 { 2 } else { 1 };
    names
        .into_iter()
        .map(|k| (strsim::levenshtein(&k.to_lowercase(), &l), k))
        .filter(|(d, _)| *d > 0 && *d <= allowed)
        .min_by_key(|(d, _)| *d)
        .map(|(_, k)| k)
}

fn strings(conn: &Connection, sql: &str, args: &[&str]) -> Vec<String> {
    conn.prepare(sql)
        .and_then(|mut s| {
            s.query_map(rusqlite::params_from_iter(args), |r| r.get::<_, String>(0))
                .map(|rows| rows.flatten().collect())
        })
        .unwrap_or_default()
}

/// The text of every indexed file of a language, for a last look before a
/// finding: a declaration the index does not hold (an extension in another
/// file, a type alias, a macro) still counts.
fn sources(conn: &Connection, project: &Project, lang: &str) -> Vec<String> {
    strings(conn, "SELECT path FROM files WHERE lang = ?1", &[lang])
        .into_iter()
        .filter_map(|p| std::fs::read_to_string(project.root.join(p)).ok())
        .map(|t| code_only(&t))
        .collect()
}

/// Whether `text` declares `name` with one of `keywords` before it
/// (`let name`, `case a, name`, `fun Type.name(`).
fn declares(text: &str, name: &str, keywords: &[&str]) -> bool {
    text.lines().any(|line| {
        line.match_indices(name).any(|(i, _)| {
            let before = &line[..i];
            let after = line[i + name.len()..].chars().next();
            !after.is_some_and(ident)
                && !before.chars().next_back().is_some_and(ident)
                && keywords.iter().any(|k| {
                    before.contains(k)
                        && before
                            .rsplit_once(k)
                            .is_some_and(|(_, between)| !between.contains(['=', '(', '{', ';']))
                })
        })
    })
}

// ---------------------------------------------------------------- Rust

/// Every path of two or more names in `code`, `use` groups spread out:
/// `use crate::store::{Store, meta_keys::X}` → `crate::store::Store`,
/// `crate::store::meta_keys::X`.
fn rust_paths(code: &str) -> BTreeSet<String> {
    fn group(prefix: &str, body: &str, out: &mut BTreeSet<String>) {
        let mut depth = 0;
        let mut start = 0;
        let mut items = Vec::new();
        for (i, c) in body.char_indices() {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                ',' if depth == 0 => {
                    items.push(&body[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        items.push(&body[start..]);
        for item in items {
            let item = item.trim();
            let item = item.split(" as ").next().unwrap_or(item).trim();
            if item.is_empty() || item == "self" || item == "*" {
                continue;
            }
            if let Some((head, rest)) = item.split_once("::{") {
                let inner = rest.rsplit_once('}').map_or(rest, |(i, _)| i);
                let prefix = format!("{prefix}::{}", head.trim());
                out.insert(prefix.clone());
                group(&prefix, inner, out);
            } else {
                let item = item.trim_end_matches("::*");
                if item
                    .split("::")
                    .all(|s| !s.is_empty() && s.chars().all(ident))
                {
                    out.insert(format!("{prefix}::{item}"));
                }
            }
        }
    }
    let mut out = BTreeSet::new();
    let b = code.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        let starts = (c.is_ascii_alphabetic() || c == '_')
            && (i == 0 || {
                let p = b[i - 1] as char;
                !ident(p) && p != ':' && p != '$' && p != '.' && p != '\''
            });
        if !starts {
            i += 1;
            continue;
        }
        let mut end = i;
        while end < b.len() && ident(b[end] as char) {
            end += 1;
        }
        let mut path = code[i..end].to_owned();
        let mut segments = 1;
        loop {
            if !code[end..].starts_with("::") {
                break;
            }
            let after = end + 2;
            match b.get(after).map(|c| *c as char) {
                Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                    let mut e = after;
                    while e < b.len() && ident(b[e] as char) {
                        e += 1;
                    }
                    path.push_str("::");
                    path.push_str(&code[after..e]);
                    segments += 1;
                    end = e;
                }
                Some('{') => {
                    // To the brace that closes the group.
                    let mut depth = 0;
                    let mut close = after;
                    for (k, ch) in code[after..].char_indices() {
                        match ch {
                            '{' => depth += 1,
                            '}' => {
                                depth -= 1;
                                if depth == 0 {
                                    close = after + k;
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    if close > after {
                        group(&path, &code[after + 1..close], &mut out);
                        end = close + 1;
                    }
                    break;
                }
                _ => break,
            }
        }
        // `crate::log!(…)`: a macro, which the index does not hold.
        let is_macro = code[end..].starts_with('!');
        if segments >= 2 && !is_macro {
            out.insert(path);
        }
        i = end.max(i + 1);
    }
    out
}

pub fn rust(
    store: &Store,
    project: &Project,
    file: &str,
    old: &str,
    new: &str,
    out: &mut Vec<String>,
) {
    let conn = store.conn();
    let crates = strings(
        conn,
        "SELECT DISTINCT substr(s.fqn, 1, instr(s.fqn, '::') - 1) FROM symbols s
         JOIN files f ON f.id = s.file_id WHERE f.lang = 'rust' AND instr(s.fqn, '::') > 0",
        &[],
    );
    if crates.is_empty() {
        return;
    }
    // The crate this file is in: the one its own items are under.
    let own = strings(
        conn,
        "SELECT substr(s.fqn, 1, instr(s.fqn, '::') - 1) FROM symbols s
         JOIN files f ON f.id = s.file_id WHERE f.path = ?1 AND instr(s.fqn, '::') > 0 LIMIT 1",
        &[file],
    )
    .into_iter()
    .next();
    let before = rust_paths(&code_only(old));
    let mut texts: Option<Vec<String>> = None;
    for path in rust_paths(&code_only(new)).difference(&before) {
        let mut parts = path.split("::");
        let first = parts.next().unwrap_or_default();
        // Only a path into the project's own code is looked at.
        let _ours = match first {
            "crate" | "self" | "super" => match &own {
                Some(c) => c.clone(),
                None => continue,
            },
            c if crates.iter().any(|k| k == c) => c.to_owned(),
            _ => continue,
        };
        let name = path.rsplit("::").next().unwrap_or_default();
        if matches!(name, "self" | "super" | "crate" | "Self") {
            continue;
        }
        // Anywhere in the project's Rust, as an item or as a module on the
        // way to one: a re-export gives an item a shorter path than its
        // own, and a binary's modules are not always under its crate's name.
        let known: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM symbols s JOIN files f ON f.id = s.file_id
                 WHERE f.lang = 'rust' AND (s.name = ?1
                    OR s.fqn LIKE ?1 || '::%'
                    OR s.fqn LIKE '%::' || ?1 || '::%'))",
                [name],
                |r| r.get(0),
            )
            .unwrap_or(true);
        if known {
            continue;
        }
        // Kinds of item the index does not hold.
        let texts = texts.get_or_insert_with(|| sources(conn, project, "rust"));
        let keywords = [
            "type ",
            "static ",
            "mod ",
            "macro_rules! ",
            "union ",
            "const ",
            "fn ",
            "struct ",
            "enum ",
            "trait ",
            // A re-export (`pub use x::Name`, `pub(crate) use x as Name`). A
            // plain `use` of the name is what is being checked, not a
            // declaration of it.
            "pub use ",
            ") use ",
        ];
        if texts.iter().any(|t| declares(t, name, &keywords)) {
            continue;
        }
        let names = strings(
            conn,
            "SELECT DISTINCT s.name FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE f.lang = 'rust'",
            &[],
        );
        let mut line = format!("`{path}` does not exist");
        if let Some(n) = near(name, &names) {
            line.push_str(&format!(" (closest: `{n}`)"));
        }
        if !out.contains(&line) {
            out.push(line);
        }
    }
}

// ---------------------------------------------------------------- Kotlin

/// Names the build makes, which no source file declares.
const KOTLIN_GENERATED: &[&str] = &["R", "BuildConfig", "BR", "Manifest"];

fn kotlin_imports(code: &str) -> BTreeSet<String> {
    code.lines()
        .filter_map(|l| l.trim().strip_prefix("import "))
        .map(|p| {
            p.split(" as ")
                .next()
                .unwrap_or(p)
                .trim()
                .trim_end_matches(';')
        })
        .filter(|p| !p.ends_with('*') && p.contains('.'))
        .map(str::to_owned)
        .collect()
}

pub fn kotlin(store: &Store, project: &Project, old: &str, new: &str, out: &mut Vec<String>) {
    let conn = store.conn();
    let (was, now) = (code_only(old), code_only(new));
    let mut texts: Option<Vec<String>> = None;
    let keywords = [
        "class ",
        "object ",
        "interface ",
        "typealias ",
        "val ",
        "var ",
        "fun ",
        "const val ",
    ];
    // An import from one of the project's own packages.
    for path in kotlin_imports(&now).difference(&kotlin_imports(&was)) {
        let Some((package, name)) = path.rsplit_once('.') else {
            continue;
        };
        if KOTLIN_GENERATED.contains(&name) {
            continue;
        }
        let exists = |sql: &str, args: &[&str]| -> bool {
            conn.query_row(sql, rusqlite::params_from_iter(args), |r| r.get(0))
                .unwrap_or(true)
        };
        // A package: something top-level sits directly in it.
        let ours = exists(
            "SELECT EXISTS(SELECT 1 FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE f.lang = 'kotlin' AND s.parent_fqn IS NULL AND s.fqn LIKE ?1 || '.%'
               AND instr(substr(s.fqn, length(?1) + 2), '.') = 0)",
            &[package],
        );
        if !ours
            || exists(
                "SELECT EXISTS(SELECT 1 FROM symbols s JOIN files f ON f.id = s.file_id
                 WHERE f.lang = 'kotlin' AND (s.fqn = ?1 OR (s.name = ?2 AND s.fqn LIKE ?3 || '.%')))",
                &[path.as_str(), name, package],
            )
        {
            continue;
        }
        let texts = texts.get_or_insert_with(|| sources(conn, project, "kotlin"));
        if texts
            .iter()
            .any(|t| t.contains(&format!("package {package}")) && declares(t, name, &keywords))
        {
            continue;
        }
        let names = strings(
            conn,
            "SELECT DISTINCT s.name FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE f.lang = 'kotlin' AND s.parent_fqn IS NULL AND s.fqn LIKE ?1 || '.%'",
            &[package],
        );
        let mut line = format!("`{path}` does not exist");
        if let Some(n) = near(name, &names) {
            line.push_str(&format!(" (closest: `{package}.{n}`)"));
        }
        if !out.contains(&line) {
            out.push(line);
        }
    }
    // A member of one of the project's enums.
    let enums = strings(
        conn,
        "SELECT s.fqn FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE f.lang = 'kotlin' AND s.kind = 'enum'
           AND (SELECT COUNT(*) FROM symbols o JOIN files g ON g.id = o.file_id
                WHERE g.lang = 'kotlin' AND o.name = s.name AND o.kind IN ('class', 'enum', 'interface')) = 1",
        &[],
    );
    for fqn in enums {
        let ty = fqn.rsplit('.').next().unwrap_or(&fqn).to_owned();
        let used = members_used(&now, &ty);
        if used.is_empty() {
            continue;
        }
        let had = members_used(&was, &ty);
        let members = strings(
            conn,
            "SELECT name FROM symbols WHERE parent_fqn = ?1 OR parent_fqn LIKE ?1 || '.%'",
            &[fqn.as_str()],
        );
        for m in used.difference(&had) {
            if members.iter().any(|k| k == m)
                || matches!(
                    m.as_str(),
                    "values" | "valueOf" | "entries" | "Companion" | "name" | "ordinal"
                )
            {
                continue;
            }
            let texts = texts.get_or_insert_with(|| sources(conn, project, "kotlin"));
            // An extension declared elsewhere: `fun Theme.Companion.of(…)`.
            if texts.iter().any(|t| {
                t.contains(&format!("{ty}.{m}")) && declares(t, m, &keywords)
                    || t.contains(&format!("enum class {ty}")) && declares(t, m, &keywords)
            }) {
                continue;
            }
            let mut line = format!("`{ty}.{m}` does not exist");
            if let Some(n) = near(m, &members) {
                line.push_str(&format!(" (closest: `{ty}.{n}`)"));
            }
            if !out.contains(&line) {
                out.push(line);
            }
        }
    }
}

// ---------------------------------------------------------------- Swift

/// Protocols that give a type no member to call on the type itself beyond
/// those every type has (`allCases` is allowed for all).
const SWIFT_PLAIN: &[&str] = &[
    "String",
    "Int",
    "Int8",
    "Int16",
    "Int32",
    "Int64",
    "UInt",
    "UInt8",
    "UInt16",
    "UInt32",
    "UInt64",
    "Double",
    "Float",
    "Character",
    "CaseIterable",
    "Identifiable",
    "Hashable",
    "Equatable",
    "Comparable",
    "Codable",
    "Encodable",
    "Decodable",
    "Sendable",
    "Error",
    "LocalizedError",
    "CustomStringConvertible",
    "CustomDebugStringConvertible",
    "CodingKey",
    "RawRepresentable",
];

pub fn swift(store: &Store, project: &Project, old: &str, new: &str, out: &mut Vec<String>) {
    let conn = store.conn();
    let (was, now) = (code_only(old), code_only(new));
    // Top-level types with one declaration of that name. An enum may conform
    // to the plain protocols; a struct or class to nothing at all, since a
    // protocol or a parent class can add members the index does not hold.
    let types: Vec<(String, String)> = conn
        .prepare(
            "SELECT s.fqn, s.kind FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE f.lang = 'swift' AND s.kind IN ('enum', 'struct', 'class') AND s.parent_fqn IS NULL
               AND (SELECT COUNT(*) FROM symbols o JOIN files g ON g.id = o.file_id
                    WHERE g.lang = 'swift' AND o.fqn = s.fqn) = 1",
        )
        .and_then(|mut s| {
            s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map(|rows| rows.flatten().collect())
        })
        .unwrap_or_default();
    let mut texts: Option<Vec<String>> = None;
    let keywords = [
        "let ",
        "var ",
        "func ",
        "case ",
        "enum ",
        "struct ",
        "class ",
        "typealias ",
        "actor ",
    ];
    for (ty, kind) in types {
        let used = members_used(&now, &ty);
        if used.is_empty() {
            continue;
        }
        let conforms = strings(
            conn,
            "SELECT r.target_fqn FROM symbol_refs r JOIN symbols s ON s.id = r.symbol_id
             WHERE s.fqn = ?1",
            &[ty.as_str()],
        );
        let plain = if kind == "enum" {
            conforms.iter().all(|c| SWIFT_PLAIN.contains(&c.as_str()))
        } else {
            conforms.is_empty()
        };
        if !plain {
            continue;
        }
        let had = members_used(&was, &ty);
        let members = strings(
            conn,
            "SELECT name FROM symbols WHERE parent_fqn = ?1",
            &[ty.as_str()],
        );
        for m in used.difference(&had) {
            if members.iter().any(|k| k == m)
                || matches!(
                    m.as_str(),
                    "allCases" | "init" | "self" | "Type" | "Protocol" | "RawValue" | "AllCases"
                )
            {
                continue;
            }
            // An extension of the type, in any file.
            let texts = texts.get_or_insert_with(|| sources(conn, project, "swift"));
            if texts.iter().any(|t| {
                (t.contains(&format!("extension {ty}"))
                    || t.contains(&format!("enum {ty}"))
                    || t.contains(&format!("struct {ty}"))
                    || t.contains(&format!("class {ty}")))
                    && declares(t, m, &keywords)
            }) {
                continue;
            }
            let mut line = format!("`{ty}.{m}` does not exist");
            if let Some(n) = near(m, &members) {
                line.push_str(&format!(" (closest: `{ty}.{n}`)"));
            }
            if !out.contains(&line) {
                out.push(line);
            }
        }
    }
}

/// Whether the file is one this module checks.
pub fn handles(file: &Path) -> Option<&'static str> {
    match file.extension()?.to_str()? {
        "rs" => Some("rust"),
        "kt" | "kts" => Some("kotlin"),
        "swift" => Some("swift"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_paths_and_use_groups() {
        let code = code_only(
            "use crate::store::{Store, meta_keys::{A, B as C}, self};\n// crate::in::a_comment\nlet x = codemap_core::scan::walk(\"crate::in::a_string\");\ncrate::log!(1);\nlet v = Vec::<u8>::new();\nfn f<'a>(x: &'a str) {}\n",
        );
        let p = rust_paths(&code);
        for want in [
            "crate::store",
            "crate::store::Store",
            "crate::store::meta_keys",
            "crate::store::meta_keys::A",
            "crate::store::meta_keys::B",
            "codemap_core::scan::walk",
        ] {
            assert!(p.contains(want), "{want}: {p:?}");
        }
        for not in ["crate::in::a_comment", "crate::in::a_string", "crate::log"] {
            assert!(!p.contains(not), "{not}: {p:?}");
        }
    }

    #[test]
    fn members_and_declarations() {
        let code = code_only(
            "let a = Theme.primary\nlet b = AppTheme.other // Theme.inComment\nx.Theme.notThis\n\"Theme.inString\"",
        );
        assert_eq!(
            members_used(&code, "Theme"),
            BTreeSet::from(["primary".to_owned()])
        );
        assert!(declares(
            "    static let primary = Color.red",
            "primary",
            &["let "]
        ));
        assert!(declares(
            "    case home, detail, settings",
            "detail",
            &["case "]
        ));
        assert!(!declares("    let x = primary", "primary", &["let "]));
        assert_eq!(
            kotlin_imports(
                "import com.app.ui.Home\nimport com.app.ui.*\nimport com.app.data.Repo as R2\n"
            ),
            BTreeSet::from(["com.app.ui.Home".to_owned(), "com.app.data.Repo".to_owned()])
        );
    }
}
