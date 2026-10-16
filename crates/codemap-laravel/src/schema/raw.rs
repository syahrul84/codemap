//! Schema changes made with raw SQL in migrations (`DB::statement('ALTER TABLE ...')`).
//! Column/table DDL is applied; statements that cannot change columns (data
//! updates, functions, triggers, extensions) are recognised and ignored.
//! Anything else is reported back so the caller can warn.

use super::dump::{apply_constraint, column, ident, paren_list, split_top, statements};
use super::{Confidence, Schema, rename_table};

/// Statements that never add, drop or change columns.
const NO_COLUMN_CHANGE: &[&str] = &[
    "UPDATE",
    "INSERT",
    "DELETE",
    "SELECT",
    "WITH",
    "CREATE EXTENSION",
    "DROP EXTENSION",
    "CREATE FUNCTION",
    "CREATE OR REPLACE FUNCTION",
    "DROP FUNCTION",
    "CREATE TRIGGER",
    "CREATE OR REPLACE TRIGGER",
    "DROP TRIGGER",
    "COMMENT ON",
    "SET ",
    "ANALYZE",
    "VACUUM",
    "GRANT",
    "REVOKE",
    "CREATE SEQUENCE",
    "ALTER SEQUENCE",
    "DROP SEQUENCE",
    "SELECT SETVAL",
    "DROP INDEX",
    "LOCK ",
    "REFRESH ",
    "DO ",
    "DO$",
    "CREATE TYPE",
    "DROP TYPE",
    "ALTER TYPE",
    "CREATE VIEW",
    "CREATE OR REPLACE VIEW",
    "DROP VIEW",
    "CREATE MATERIALIZED VIEW",
    "DROP MATERIALIZED VIEW",
    "CREATE POLICY",
    "DROP POLICY",
    "TRUNCATE",
    "PRAGMA",
];

/// Applies `sql` to the schema; returns the statements it did not understand.
pub fn apply(schema: &mut Schema, sql: &str, at: &str, conditional: bool) -> Vec<String> {
    let mut not_understood = Vec::new();
    for (_, stmt) in statements(sql) {
        if !apply_one(schema, &stmt, at, conditional) {
            let short: String = stmt
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(80)
                .collect();
            not_understood.push(short);
        }
    }
    not_understood
}

fn strip_words<'a>(mut s: &'a str, words: &[&str]) -> &'a str {
    loop {
        let trimmed = s.trim_start();
        let Some(w) = words.iter().find(|w| {
            trimmed.len() >= w.len()
                && trimmed[..w.len()].eq_ignore_ascii_case(w)
                && trimmed[w.len()..].starts_with(char::is_whitespace)
        }) else {
            return trimmed;
        };
        s = &trimmed[w.len()..];
    }
}

fn first_word(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    match s.find(char::is_whitespace) {
        Some(i) => (&s[..i], s[i..].trim_start()),
        None => (s, ""),
    }
}

fn apply_one(schema: &mut Schema, stmt: &str, at: &str, conditional: bool) -> bool {
    let upper = stmt.trim_start().to_uppercase();
    if NO_COLUMN_CHANGE.iter().any(|p| upper.starts_with(p)) {
        return true;
    }
    let confidence = if conditional {
        Confidence::Low
    } else {
        Confidence::High
    };
    if upper.starts_with("CREATE TABLE") {
        let file = at.rsplit_once(':').map_or(at, |(f, _)| f);
        let parsed = super::dump::parse(&format!("{stmt};"), file);
        let ok = !parsed.tables.is_empty();
        for mut t in parsed.tables {
            t.created_in = Some(at.to_owned());
            t.last_changed_in = Some(at.to_owned());
            t.confidence = confidence;
            for c in &mut t.columns {
                c.created_in = Some(at.to_owned());
                c.last_changed_in = Some(at.to_owned());
                c.confidence = confidence;
            }
            schema.tables.retain(|o| o.name != t.name || o.dropped);
            schema.tables.push(t);
        }
        return ok;
    }
    if upper.starts_with("CREATE INDEX") || upper.starts_with("CREATE UNIQUE INDEX") {
        let unique = upper.starts_with("CREATE UNIQUE");
        let Some(on) = upper.find(" ON ") else {
            return false;
        };
        let rest = strip_words(&stmt[on + 4..], &["ONLY"]);
        let name = ident(
            rest.split(|c: char| c.is_whitespace() || c == '(')
                .next()
                .unwrap_or(""),
        );
        let cols = paren_list(rest);
        if let Some(t) = schema.table_mut(&name) {
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
        // Expression indexes (e.g. `((geom::geography))`) change no columns.
        return true;
    }
    if upper.starts_with("DROP TABLE") {
        let names = strip_words(&stmt["DROP TABLE".len()..], &["IF", "EXISTS"]);
        for name in names.split(',') {
            let name = ident(name.split_whitespace().next().unwrap_or(""));
            if let Some(t) = schema.table_mut(&name) {
                t.dropped = true;
                t.last_changed_in = Some(at.to_owned());
            }
        }
        return true;
    }
    if !upper.starts_with("ALTER TABLE") {
        return false;
    }
    let rest = strip_words(&stmt["ALTER TABLE".len()..], &["IF", "EXISTS", "ONLY"]);
    let (name_raw, actions) = first_word(rest);
    let table = ident(name_raw);
    // ALTER TABLE a RENAME TO b
    let renamed = strip_words(actions, &["RENAME"]);
    if renamed.len() < actions.len() && renamed.to_uppercase().starts_with("TO ") {
        let to = ident(renamed[3..].trim());
        rename_table(schema, &table, &to, at);
        return true;
    }
    let Some(t) = schema.table_mut(&table) else {
        return false;
    };
    t.last_changed_in = Some(at.to_owned());
    let mut ok = true;
    for action in split_top(actions) {
        let up = action.to_uppercase();
        let done = if up.starts_with("ADD CONSTRAINT")
            || up.starts_with("ADD PRIMARY KEY")
            || up.starts_with("ADD UNIQUE")
            || up.starts_with("ADD FOREIGN KEY")
            || up.starts_with("ADD CHECK")
            || up.starts_with("DROP CONSTRAINT")
            || up.starts_with("RENAME CONSTRAINT")
            || up.starts_with("VALIDATE CONSTRAINT")
        {
            if up.starts_with("ADD") {
                apply_constraint(t, &action);
            }
            true
        } else if up.starts_with("ADD") {
            let def = strip_words(&action[3..], &["COLUMN", "IF", "NOT", "EXISTS"]);
            match column(def, at) {
                Some(mut c) => {
                    c.confidence = confidence;
                    t.columns.retain(|o| o.name != c.name);
                    t.columns.push(c);
                    true
                }
                None => false,
            }
        } else if up.starts_with("DROP") {
            let (col, _) = first_word(strip_words(&action[4..], &["COLUMN", "IF", "EXISTS"]));
            let col = ident(col);
            t.columns.retain(|c| c.name != col);
            true
        } else if up.starts_with("RENAME") {
            let rest = strip_words(&action[6..], &["COLUMN"]);
            let (from, after) = first_word(rest);
            let (to_kw, to) = first_word(after);
            match (to_kw.eq_ignore_ascii_case("TO"), t.column_mut(&ident(from))) {
                (true, Some(c)) => {
                    c.name = ident(to);
                    c.last_changed_in = Some(at.to_owned());
                    true
                }
                _ => false,
            }
        } else if up.starts_with("ALTER") {
            let rest = strip_words(&action[5..], &["COLUMN"]);
            let (col, change) = first_word(rest);
            let change_up = change.to_uppercase();
            match t.column_mut(&ident(col)) {
                Some(c) => {
                    c.last_changed_in = Some(at.to_owned());
                    if conditional {
                        c.confidence = Confidence::Low;
                    }
                    if change_up.starts_with("SET NOT NULL") {
                        c.nullable = false;
                    } else if change_up.starts_with("DROP NOT NULL") {
                        c.nullable = true;
                    } else if change_up.starts_with("DROP DEFAULT") {
                        c.default = None;
                    } else if change_up.starts_with("SET DEFAULT") {
                        let v = change["SET DEFAULT".len()..].trim();
                        c.default = Some(v.split("::").next().unwrap_or(v).trim().to_owned());
                    } else if change_up.starts_with("TYPE")
                        || change_up.starts_with("SET DATA TYPE")
                    {
                        let ty = strip_words(change, &["SET", "DATA", "TYPE"]);
                        let ty = ty
                            .split(" USING")
                            .next()
                            .unwrap_or(ty)
                            .split(" using")
                            .next()
                            .unwrap_or(ty);
                        c.r#type = ty.trim().to_lowercase();
                    }
                    true
                }
                None => false,
            }
        } else {
            false
        };
        ok &= done;
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::super::{Column, Table};
    use super::*;

    fn schema() -> Schema {
        let col = |n: &str| Column {
            name: n.into(),
            r#type: "int".into(),
            ..Column::default()
        };
        Schema {
            tables: vec![Table {
                name: "outlets".into(),
                columns: vec![col("id"), col("lat"), col("cuisine_type_id"), col("old")],
                ..Table::default()
            }],
            ..Schema::default()
        }
    }

    fn names(s: &Schema, t: &str) -> Vec<String> {
        s.tables
            .iter()
            .find(|x| x.name == t && !x.dropped)
            .unwrap()
            .columns
            .iter()
            .map(|c| c.name.clone())
            .collect()
    }

    #[test]
    fn alter_table_column_changes() {
        let mut s = schema();
        let bad = apply(
            &mut s,
            "ALTER TABLE outlets ADD COLUMN geom geometry(Point, 4326); ALTER TABLE outlets ALTER COLUMN geom SET NOT NULL;\nALTER TABLE outlets DROP COLUMN IF EXISTS cuisine_type_id; ALTER TABLE outlets RENAME COLUMN old TO new, ALTER COLUMN lat TYPE numeric(10,7) USING lat::numeric",
            "m.php:3",
            false,
        );
        assert!(bad.is_empty(), "{bad:?}");
        assert_eq!(names(&s, "outlets"), ["id", "lat", "new", "geom"]);
        let t = &s.tables[0];
        let geom = t.columns.iter().find(|c| c.name == "geom").unwrap();
        assert_eq!(
            (geom.r#type.as_str(), geom.nullable),
            ("geometry(point, 4326)", false)
        );
        assert_eq!(t.columns[1].r#type, "numeric(10,7)");
    }

    #[test]
    fn data_and_function_statements_are_ignored_unknown_reported() {
        let mut s = schema();
        let bad = apply(
            &mut s,
            "UPDATE outlets SET lat = 0; CREATE OR REPLACE FUNCTION f() RETURNS trigger AS $$ BEGIN RETURN NEW; END $$ LANGUAGE plpgsql; CREATE INDEX outlets_geog_idx ON outlets USING GIST ((geom::geography)); FROBNICATE outlets",
            "m.php:1",
            false,
        );
        assert_eq!(bad, ["FROBNICATE outlets"]);
    }

    #[test]
    fn raw_create_and_drop_table() {
        let mut s = schema();
        assert!(apply(&mut s, "CREATE TABLE IF NOT EXISTS tags (id bigserial PRIMARY KEY, name varchar(50) NOT NULL); DROP TABLE IF EXISTS outlets", "m.php:9", false).is_empty());
        assert_eq!(names(&s, "tags"), ["id", "name"]);
        assert!(s.tables.iter().any(|t| t.name == "outlets" && t.dropped));
    }
}
