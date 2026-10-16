//! `database/schema/*-schema.sql` (from `php artisan schema:dump`): basic
//! `CREATE TABLE` / `ALTER TABLE ... ADD CONSTRAINT` / `CREATE INDEX` for the
//! MySQL, PostgreSQL and SQLite dialects, plus the list of migrations it holds.

use super::{Column, Table};
use crate::routes::eval::Warning;

#[derive(Debug, Default)]
pub struct Dump {
    pub tables: Vec<Table>,
    pub migrations: Vec<String>,
    pub warnings: Vec<Warning>,
}

/// Splits SQL into statements with their 1-based start line, skipping comments.
pub(super) fn statements(sql: &str) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut line = 1u32;
    let mut start_line = 1u32;
    let mut quote: Option<char> = None;
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\n' {
            line += 1;
        }
        if let Some(q) = quote {
            cur.push(c);
            if c == '\\' && q != '`' {
                if let Some(n) = chars.next() {
                    if n == '\n' {
                        line += 1;
                    }
                    cur.push(n);
                }
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' => {
                quote = Some(c);
                cur.push(c);
            }
            // PostgreSQL dollar quoting: $$ ... $$ or $tag$ ... $tag$ (function bodies).
            '$' if chars
                .peek()
                .is_some_and(|n| *n == '$' || n.is_ascii_alphabetic() || *n == '_') =>
            {
                let mut tag = String::from("$");
                let mut lookahead = chars.clone();
                let mut closed = false;
                for n in lookahead.by_ref() {
                    tag.push(n);
                    if n == '$' {
                        closed = true;
                        break;
                    }
                    if !(n.is_ascii_alphanumeric() || n == '_') {
                        break;
                    }
                }
                if !closed {
                    cur.push(c);
                    continue;
                }
                // Consume the opening tag, then everything up to the closing tag.
                for _ in 1..tag.len() {
                    chars.next();
                }
                cur.push_str(&tag);
                let mut body = String::new();
                for n in chars.by_ref() {
                    if n == '\n' {
                        line += 1;
                    }
                    body.push(n);
                    if body.ends_with(&tag) {
                        break;
                    }
                }
                cur.push_str(&body);
            }
            '-' if chars.peek() == Some(&'-') => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        line += 1;
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if n == '\n' {
                        line += 1;
                    }
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            ';' => {
                let s = cur.trim().to_owned();
                if !s.is_empty() {
                    out.push((start_line, s));
                }
                cur.clear();
            }
            _ => {
                if cur.trim().is_empty() && !c.is_whitespace() {
                    start_line = line;
                }
                cur.push(c);
            }
        }
    }
    if !cur.trim().is_empty() {
        out.push((start_line, cur.trim().to_owned()));
    }
    out
}

/// `` `orders` `` / `"public"."orders"` / `[orders]` → `orders`.
pub(super) fn ident(s: &str) -> String {
    let s = s.trim();
    let last = s.rsplit('.').next().unwrap_or(s);
    last.trim_matches(|c| matches!(c, '`' | '"' | '[' | ']'))
        .to_owned()
}

/// Splits on commas at parenthesis depth 0, outside quotes.
pub(super) fn split_top(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut cur = String::new();
    for c in s.chars() {
        if let Some(q) = quote {
            cur.push(c);
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' => {
                quote = Some(c);
                cur.push(c);
            }
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => {
                parts.push(cur.trim().to_owned());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        parts.push(cur.trim().to_owned());
    }
    parts
}

/// Identifiers inside the first `( ... )` of `s`.
pub(super) fn paren_list(s: &str) -> Vec<String> {
    let Some(open) = s.find('(') else {
        return Vec::new();
    };
    let Some(close) = s[open..].find(')') else {
        return Vec::new();
    };
    s[open + 1..open + close]
        .split(',')
        .map(|p| ident(p.split_whitespace().next().unwrap_or("")))
        .filter(|p| !p.is_empty())
        .collect()
}

/// `REFERENCES `customers` (`id`)` → (customers, id)
fn references(s: &str) -> Option<(String, String)> {
    let upper = s.to_uppercase();
    let i = upper.find("REFERENCES")?;
    let rest = &s[i + "REFERENCES".len()..];
    let table = rest.split('(').next()?.trim();
    let col = paren_list(rest)
        .into_iter()
        .next()
        .unwrap_or_else(|| "id".into());
    Some((ident(table), col))
}

const STOP: &[&str] = &[
    "NOT",
    "NULL",
    "DEFAULT",
    "AUTO_INCREMENT",
    "AUTOINCREMENT",
    "PRIMARY",
    "UNIQUE",
    "COLLATE",
    "CHARACTER",
    "COMMENT",
    "GENERATED",
    "CHECK",
    "REFERENCES",
    "CONSTRAINT",
    "ON",
    "CHARSET",
];

pub(super) fn column(def: &str, at: &str) -> Option<Column> {
    let def = def.trim();
    let (name, rest) = if let Some(q) = def.chars().next().filter(|c| matches!(c, '`' | '"' | '['))
    {
        let close = if q == '[' { ']' } else { q };
        let end = def[1..].find(close)? + 1;
        (def[1..end].to_owned(), def[end + 1..].trim())
    } else {
        let mut it = def.splitn(2, char::is_whitespace);
        (it.next()?.to_owned(), it.next().unwrap_or("").trim())
    };
    let upper = rest.to_uppercase();
    let words: Vec<&str> = rest.split_whitespace().collect();
    let mut ty_words = Vec::new();
    for (i, w) in words.iter().enumerate() {
        let up = w.to_uppercase();
        // `CHARACTER SET` (MySQL) ends the type; `character varying` (PostgreSQL) is the type.
        let charset = up == "CHARACTER"
            && words
                .get(i + 1)
                .is_some_and(|n| n.eq_ignore_ascii_case("SET"));
        if charset || (up != "CHARACTER" && STOP.contains(&up.as_str())) {
            break;
        }
        ty_words.push(*w);
    }
    let mut ty = ty_words.join(" ").to_lowercase();
    let unsigned = ty.contains("unsigned");
    ty = ty.replace(" unsigned", "").replace(" zerofill", "");
    let ty = match ty.as_str() {
        "int" | "integer" => "int".to_owned(),
        "character varying" => "varchar".to_owned(),
        t if t.starts_with("character varying(") => t.replacen("character varying", "varchar", 1),
        "timestamp(0) without time zone" | "timestamp without time zone" => "timestamp".to_owned(),
        "bigserial" => "bigint".to_owned(),
        "serial" => "int".to_owned(),
        t => t.to_owned(),
    };
    let primary = upper.contains("PRIMARY KEY");
    let default = upper.find("DEFAULT ").map(|i| {
        let after = rest[i + 8..].trim();
        let value = if let Some(stripped) = after.strip_prefix('\'') {
            let end = stripped.find('\'').unwrap_or(stripped.len());
            format!("'{}'", &stripped[..end])
        } else if let Some(stripped) = after.strip_prefix('(') {
            stripped.split(')').next().unwrap_or("").to_owned()
        } else {
            after.split_whitespace().next().unwrap_or("").to_owned()
        };
        // PostgreSQL casts: 'pending'::character varying
        value.split("::").next().unwrap_or(&value).to_owned()
    });
    let default = default.filter(|d| !d.eq_ignore_ascii_case("NULL"));
    let fk = references(rest);
    Some(Column {
        name,
        r#type: ty,
        nullable: !upper.contains("NOT NULL") && !primary,
        default,
        unsigned,
        unique: upper.contains("UNIQUE"),
        indexed: false,
        primary,
        auto_increment: upper.contains("AUTO_INCREMENT")
            || upper.contains("AUTOINCREMENT")
            || upper.contains("SERIAL")
            || upper.contains("IDENTITY"),
        fk_table: fk.as_ref().map(|f| f.0.clone()),
        fk_column: fk.map(|f| f.1),
        created_in: Some(at.to_owned()),
        last_changed_in: Some(at.to_owned()),
        confidence: super::Confidence::High,
    })
}

pub(super) fn apply_constraint(table: &mut Table, part: &str) {
    let upper = part.to_uppercase();
    let cols = paren_list(part);
    let single = cols.len() == 1;
    if upper.contains("FOREIGN KEY") {
        let fk = references(part);
        for name in &cols {
            if let Some(c) = table.column_mut(name) {
                c.fk_table = fk.as_ref().map(|f| f.0.clone());
                c.fk_column = fk.as_ref().map(|f| f.1.clone());
            }
        }
    } else if upper.contains("PRIMARY KEY") {
        for name in &cols {
            if let Some(c) = table.column_mut(name) {
                c.primary = true;
                c.nullable = false;
            }
        }
    } else if upper.contains("UNIQUE") {
        for name in &cols {
            if let Some(c) = table.column_mut(name) {
                if single {
                    c.unique = true;
                } else {
                    c.indexed = true;
                }
            }
        }
    } else if upper.starts_with("KEY") || upper.starts_with("INDEX") || upper.contains(" INDEX") {
        for name in &cols {
            if let Some(c) = table.column_mut(name) {
                c.indexed = true;
            }
        }
    }
}

fn is_migration_name(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() > 18
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'_'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'_'
        && b[8..10].iter().all(u8::is_ascii_digit)
        && b[10] == b'_'
        && b[11..17].iter().all(u8::is_ascii_digit)
        && b[17] == b'_'
}

pub fn parse(sql: &str, rel: &str) -> Dump {
    let mut dump = Dump::default();
    for (line, stmt) in statements(sql) {
        let upper = stmt.to_uppercase();
        let at = format!("{rel}:{line}");
        if upper.starts_with("CREATE TABLE") {
            let Some(open) = stmt.find('(') else { continue };
            let Some(close) = stmt.rfind(')') else {
                continue;
            };
            let head = stmt[..open]
                .split_whitespace()
                .filter(|w| {
                    !matches!(
                        w.to_uppercase().as_str(),
                        "CREATE" | "TABLE" | "IF" | "NOT" | "EXISTS" | "UNLOGGED" | "TEMPORARY"
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            let name = ident(&head);
            // Laravel's own bookkeeping table is not part of the app schema.
            if name == "migrations" {
                continue;
            }
            let mut table = Table {
                name,
                created_in: Some(at.clone()),
                last_changed_in: Some(at.clone()),
                ..Table::default()
            };
            let mut constraints = Vec::new();
            for part in split_top(&stmt[open + 1..close]) {
                let pu = part.to_uppercase();
                if [
                    "PRIMARY KEY",
                    "UNIQUE",
                    "KEY ",
                    "INDEX ",
                    "CONSTRAINT",
                    "FOREIGN KEY",
                    "FULLTEXT",
                    "SPATIAL",
                    "CHECK",
                ]
                .iter()
                .any(|p| pu.starts_with(p))
                {
                    constraints.push(part);
                } else if let Some(c) = column(&part, &at) {
                    table.columns.push(c);
                }
            }
            for c in constraints {
                apply_constraint(&mut table, &c);
            }
            dump.tables.push(table);
        } else if upper.starts_with("ALTER TABLE") && upper.contains(" ADD ") {
            let words: Vec<&str> = stmt.split_whitespace().collect();
            let name_word = words
                .iter()
                .skip(2)
                .find(|w| !matches!(w.to_uppercase().as_str(), "ONLY" | "IF" | "EXISTS"))
                .copied()
                .unwrap_or("");
            let name = ident(name_word);
            let add = &stmt[upper.find(" ADD ").unwrap_or(0) + 5..];
            if let Some(t) = dump.tables.iter_mut().find(|t| t.name == name) {
                apply_constraint(t, add);
            }
        } else if upper.starts_with("CREATE INDEX") || upper.starts_with("CREATE UNIQUE INDEX") {
            let Some(on) = upper.find(" ON ") else {
                continue;
            };
            let rest = &stmt[on + 4..];
            let rest_trim = rest.trim_start();
            let rest_trim = rest_trim
                .strip_prefix("ONLY ")
                .or_else(|| rest_trim.strip_prefix("only "))
                .unwrap_or(rest_trim);
            let name = ident(
                rest_trim
                    .split(|c: char| c.is_whitespace() || c == '(')
                    .next()
                    .unwrap_or(""),
            );
            let unique = upper.starts_with("CREATE UNIQUE");
            let cols = paren_list(rest_trim);
            if let Some(t) = dump.tables.iter_mut().find(|t| t.name == name) {
                for col in &cols {
                    if let Some(c) = t.column_mut(col) {
                        if unique && cols.len() == 1 {
                            c.unique = true;
                        } else {
                            c.indexed = true;
                        }
                    }
                }
            }
        } else if upper.starts_with("INSERT INTO") && upper.contains("MIGRATIONS") {
            for piece in stmt.split('\'') {
                if is_migration_name(piece) {
                    dump.migrations.push(piece.to_owned());
                }
            }
        }
    }
    if dump.tables.is_empty() {
        dump.warnings.push(Warning {
            file: Some(rel.to_owned()),
            line: None,
            message: "no CREATE TABLE statements understood in the schema dump".into(),
        });
    }
    dump
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mysql_dump() {
        let sql = "/*!40101 SET x */;\nCREATE TABLE `orders` (\n  `id` bigint unsigned NOT NULL AUTO_INCREMENT,\n  `state` varchar(20) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT 'pending',\n  `notes` text,\n  `customer_id` bigint unsigned NOT NULL,\n  PRIMARY KEY (`id`),\n  KEY `orders_state_index` (`state`),\n  CONSTRAINT `fk` FOREIGN KEY (`customer_id`) REFERENCES `customers` (`id`) ON DELETE CASCADE\n) ENGINE=InnoDB;\nINSERT INTO `migrations` VALUES (1,'2014_10_12_000000_create_users_table',1);\n";
        let d = parse(sql, "d.sql");
        assert_eq!(d.migrations, ["2014_10_12_000000_create_users_table"]);
        let t = &d.tables[0];
        assert_eq!(t.name, "orders");
        assert_eq!(t.created_in.as_deref(), Some("d.sql:2"));
        let id = &t.columns[0];
        assert!(id.primary && id.auto_increment && id.unsigned && !id.nullable);
        assert_eq!(id.r#type, "bigint");
        let state = &t.columns[1];
        assert_eq!(
            (
                state.r#type.as_str(),
                state.default.as_deref(),
                state.indexed
            ),
            ("varchar(20)", Some("'pending'"), true)
        );
        assert!(t.columns[2].nullable);
        assert_eq!(t.columns[3].fk_table.as_deref(), Some("customers"));
    }

    #[test]
    fn postgres_dump() {
        let sql = "CREATE TABLE public.orders (\n    id bigint NOT NULL,\n    status character varying(255) DEFAULT 'pending'::character varying NOT NULL\n);\nALTER TABLE ONLY public.orders\n    ADD CONSTRAINT orders_pkey PRIMARY KEY (id);\nCREATE INDEX orders_status_index ON public.orders USING btree (status);\nINSERT INTO public.migrations VALUES (1, '2024_01_01_000000_create_orders_table', 1);";
        let d = parse(sql, "p.sql");
        let t = &d.tables[0];
        assert_eq!(t.name, "orders");
        assert!(t.columns[0].primary);
        assert_eq!(t.columns[1].r#type, "varchar(255)");
        assert_eq!(t.columns[1].default.as_deref(), Some("'pending'"));
        assert!(t.columns[1].indexed);
        assert_eq!(d.migrations.len(), 1);
    }

    #[test]
    fn sqlite_dump() {
        let sql = "CREATE TABLE IF NOT EXISTS \"tags\" (\"id\" integer primary key autoincrement not null, \"name\" varchar not null);\nCREATE UNIQUE INDEX \"tags_name_unique\" on \"tags\" (\"name\");";
        let d = parse(sql, "s.sql");
        let t = &d.tables[0];
        assert_eq!(t.name, "tags");
        assert!(t.columns[0].primary && t.columns[0].auto_increment);
        assert!(t.columns[1].unique);
    }
}
