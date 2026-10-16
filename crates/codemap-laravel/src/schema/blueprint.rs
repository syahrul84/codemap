//! `Blueprint` calls inside `Schema::create/table` closures: column types,
//! modifiers (`nullable`, `default`, `change`, `constrained`, ...) and commands
//! (`dropColumn`, `renameColumn`, `unique`, `foreign`, ...).

use codemap_php::ast::{named_children, start_line, text};
use tree_sitter::Node;

use super::{Column, Confidence, ReplayContext, Schema, Table, rename_table};
use crate::inflector;
use crate::literal::{Lit, snake};

/// Evaluates an argument (literals, config(), tracked variables).
pub type Eval<'a> = dyn Fn(Node<'_>) -> Lit + 'a;

/// Column-defining methods, to tell "unknown method" from "name is not a literal".
const COLUMN_METHODS: &[&str] = &[
    "id",
    "increments",
    "integerIncrements",
    "tinyIncrements",
    "smallIncrements",
    "mediumIncrements",
    "bigIncrements",
    "string",
    "char",
    "text",
    "tinyText",
    "mediumText",
    "longText",
    "integer",
    "tinyInteger",
    "smallInteger",
    "mediumInteger",
    "bigInteger",
    "unsignedInteger",
    "unsignedTinyInteger",
    "unsignedSmallInteger",
    "unsignedMediumInteger",
    "unsignedBigInteger",
    "boolean",
    "decimal",
    "unsignedDecimal",
    "float",
    "double",
    "date",
    "dateTime",
    "dateTimeTz",
    "time",
    "timeTz",
    "timestamp",
    "timestampTz",
    "year",
    "json",
    "jsonb",
    "binary",
    "uuid",
    "ulid",
    "geometry",
    "geography",
    "point",
    "vector",
    "ipAddress",
    "macAddress",
    "enum",
    "set",
    "foreignId",
    "foreignUuid",
    "foreignUlid",
    "foreignIdFor",
    "morphs",
    "nullableMorphs",
    "uuidMorphs",
    "nullableUuidMorphs",
    "ulidMorphs",
    "nullableUlidMorphs",
];

struct Call<'t> {
    name: String,
    args: Vec<Node<'t>>,
}

struct ColumnDef {
    col: Column,
    /// FK target from `foreignIdFor` (used only with `->constrained()`).
    fk_hint: Option<(String, String)>,
}

#[derive(Default)]
struct Mods {
    nullable: Option<bool>,
    default: Option<String>,
    unsigned: Option<bool>,
    unique: bool,
    index: bool,
    primary: bool,
    auto_increment: bool,
    after: Option<String>,
    first: bool,
    change: bool,
    constrained: Option<(Option<String>, Option<String>)>,
    references: Option<String>,
    on: Option<String>,
    unknown: Vec<String>,
}

pub struct Blueprint<'s, 'c> {
    schema: &'s mut Schema,
    rel: &'s str,
    table: String,
    ctx: &'s ReplayContext<'c>,
    conditional: bool,
}

fn col(name: &str, ty: &str) -> Column {
    Column {
        name: name.to_owned(),
        r#type: ty.to_owned(),
        ..Column::default()
    }
}

impl<'s, 'c> Blueprint<'s, 'c> {
    pub fn new(
        schema: &'s mut Schema,
        rel: &'s str,
        table: &str,
        ctx: &'s ReplayContext<'c>,
        conditional: bool,
    ) -> Self {
        Self {
            schema,
            rel,
            table: table.to_owned(),
            ctx,
            conditional,
        }
    }

    pub fn statements(
        &mut self,
        body: Node<'_>,
        src: &[u8],
        var: &str,
        conditional: bool,
        eval: &Eval<'_>,
    ) {
        for s in named_children(body) {
            match s.kind() {
                "expression_statement" => {
                    if let Some(e) = named_children(s).into_iter().next() {
                        self.statement_expr(e, src, var, conditional, eval);
                    }
                }
                "compound_statement" | "colon_block" => {
                    self.statements(s, src, var, conditional, eval)
                }
                // `if ($teams)` with a known value: follow the branch that runs.
                "if_statement"
                    if s.child_by_field_name("condition")
                        .and_then(|c| crate::literal::truthy(&eval(c)))
                        .is_some() =>
                {
                    let run = s
                        .child_by_field_name("condition")
                        .and_then(|c| crate::literal::truthy(&eval(c)))
                        .unwrap_or(false);
                    let mut cursor = s.walk();
                    let target = if run {
                        s.child_by_field_name("body")
                    } else {
                        s.children_by_field_name("alternative", &mut cursor)
                            .find(|a| a.kind() == "else_clause")
                            .and_then(|a| a.child_by_field_name("body"))
                    };
                    if let Some(body) = target {
                        if body.kind() == "expression_statement" {
                            if let Some(e) = named_children(body).into_iter().next() {
                                self.statement_expr(e, src, var, conditional, eval);
                            }
                        } else {
                            self.statements(body, src, var, conditional, eval);
                        }
                    }
                }
                "if_statement" | "else_clause" | "else_if_clause" | "foreach_statement"
                | "for_statement" | "while_statement" => {
                    for c in named_children(s) {
                        if c.kind() != "parenthesized_expression" {
                            if c.kind() == "expression_statement" {
                                if let Some(e) = named_children(c).into_iter().next() {
                                    self.statement_expr(e, src, var, true, eval);
                                }
                            } else {
                                self.statements(c, src, var, true, eval);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    pub fn statement_expr(
        &mut self,
        e: Node<'_>,
        src: &[u8],
        var: &str,
        conditional: bool,
        eval: &Eval<'_>,
    ) {
        let Some(calls) = chain(e, src, var) else {
            return;
        };
        let line = start_line(e);
        let lits: Vec<Vec<Lit>> = calls
            .iter()
            .map(|c| c.args.iter().map(|a| eval(*a)).collect())
            .collect();
        self.conditional_flag(conditional);
        self.apply(&calls, &lits, line);
    }

    fn conditional_flag(&mut self, conditional: bool) {
        self.conditional = self.conditional || conditional;
    }

    fn at(&self, line: u32) -> String {
        format!("{}:{line}", self.rel)
    }

    fn warn(&mut self, line: u32, msg: impl Into<String>) {
        let rel = self.rel;
        self.schema.warn(rel, Some(line), msg);
    }

    fn table(&mut self) -> Option<&mut Table> {
        let name = self.table.clone();
        self.schema.table_mut(&name)
    }

    fn apply(&mut self, calls: &[Call<'_>], lits: &[Vec<Lit>], line: u32) {
        let first = &calls[0];
        let args = &lits[0];
        let mods = parse_mods(&calls[1..], &lits[1..]);
        if !mods.unknown.is_empty() {
            let list: Vec<String> = mods.unknown.iter().map(|m| format!("->{m}()")).collect();
            self.warn(line, format!("unknown column modifier {}", list.join(", ")));
        }
        match self.column_defs(&first.name, args) {
            Some(defs) => {
                for def in defs {
                    self.add_or_change(def, &mods, line);
                }
            }
            None if COLUMN_METHODS.contains(&first.name.as_str()) => {
                let arg = args
                    .first()
                    .map_or_else(|| "missing".to_owned(), Lit::display);
                self.warn(
                    line,
                    format!(
                        "->{}() column name is not a literal ({arg}); column not recorded",
                        first.name
                    ),
                );
            }
            None => self.command(&first.name, args, &mods, line),
        }
    }

    fn column_defs(&self, method: &str, a: &[Lit]) -> Option<Vec<ColumnDef>> {
        let s = |i: usize| a.get(i).and_then(Lit::as_str).map(str::to_owned);
        let n = |i: usize, d: i64| match a.get(i) {
            Some(Lit::Int(v)) => *v,
            _ => d,
        };
        let name = || s(0);
        let one = |c: Column| {
            Some(vec![ColumnDef {
                col: c,
                fk_hint: None,
            }])
        };
        let auto_pk = |name: String, ty: &str| {
            let mut c = col(&name, ty);
            c.unsigned = true;
            c.auto_increment = true;
            c.primary = true;
            c
        };
        let int = |ty: &str, unsigned: bool| -> Option<Vec<ColumnDef>> {
            let mut c = col(&name()?, ty);
            c.unsigned = unsigned || matches!(a.get(2), Some(Lit::Bool(true)));
            if matches!(a.get(1), Some(Lit::Bool(true))) {
                c.auto_increment = true;
                c.primary = true;
            }
            one(c)
        };
        match method {
            "id" => one(auto_pk(s(0).unwrap_or_else(|| "id".into()), "bigint")),
            "increments" | "integerIncrements" => one(auto_pk(name()?, "int")),
            "tinyIncrements" => one(auto_pk(name()?, "tinyint")),
            "smallIncrements" => one(auto_pk(name()?, "smallint")),
            "mediumIncrements" => one(auto_pk(name()?, "mediumint")),
            "bigIncrements" => one(auto_pk(name()?, "bigint")),
            "string" => one(col(&name()?, &format!("varchar({})", n(1, 255)))),
            "char" => one(col(&name()?, &format!("char({})", n(1, 255)))),
            "text" | "tinyText" | "mediumText" | "longText" => {
                one(col(&name()?, &method.to_lowercase()))
            }
            "integer" => int("int", false),
            "tinyInteger" => int("tinyint", false),
            "smallInteger" => int("smallint", false),
            "mediumInteger" => int("mediumint", false),
            "bigInteger" => int("bigint", false),
            "unsignedInteger" => int("int", true),
            "unsignedTinyInteger" => int("tinyint", true),
            "unsignedSmallInteger" => int("smallint", true),
            "unsignedMediumInteger" => int("mediumint", true),
            "unsignedBigInteger" => int("bigint", true),
            "boolean" => one(col(&name()?, "boolean")),
            "decimal" | "unsignedDecimal" => {
                let mut c = col(&name()?, &format!("decimal({},{})", n(1, 8), n(2, 2)));
                c.unsigned = method == "unsignedDecimal";
                one(c)
            }
            "float" => one(col(&name()?, "float")),
            "double" => one(col(&name()?, "double")),
            "date" => one(col(&name()?, "date")),
            "dateTime" => one(col(&name()?, "datetime")),
            "dateTimeTz" => one(col(&name()?, "datetimetz")),
            "time" => one(col(&name()?, "time")),
            "timeTz" => one(col(&name()?, "timetz")),
            "timestamp" => one(col(&name()?, "timestamp")),
            "timestampTz" => one(col(&name()?, "timestamptz")),
            "year" => one(col(&name()?, "year")),
            "json" | "jsonb" | "binary" | "uuid" | "ulid" | "geometry" | "geography" | "point"
            | "vector" => one(col(&name()?, &method.to_lowercase())),
            "ipAddress" => one(col(
                &s(0).unwrap_or_else(|| "ip_address".into()),
                "ipaddress",
            )),
            "macAddress" => one(col(
                &s(0).unwrap_or_else(|| "mac_address".into()),
                "macaddress",
            )),
            "enum" | "set" => {
                let values = a.get(1).and_then(Lit::strings).unwrap_or_default();
                let list: Vec<String> = values.iter().map(|v| format!("'{v}'")).collect();
                one(col(&name()?, &format!("{method}({})", list.join(","))))
            }
            "foreignId" => {
                let mut c = col(&name()?, "bigint");
                c.unsigned = true;
                one(c)
            }
            "foreignUuid" => one(col(&name()?, "uuid")),
            "foreignUlid" => one(col(&name()?, "ulid")),
            "foreignIdFor" if a.first().and_then(Lit::as_str).is_none() => {
                // Model given by an expression (e.g. Passkeys::userModel()): only an
                // explicit column name can be recorded, with the type assumed.
                let mut c = col(&s(1)?, "bigint");
                c.unsigned = true;
                c.confidence = Confidence::Low;
                Some(vec![ColumnDef {
                    col: c,
                    fk_hint: None,
                }])
            }
            "foreignIdFor" => {
                let class = a.first().and_then(Lit::as_str)?.to_owned();
                let model = self
                    .ctx
                    .models
                    .iter()
                    .find(|m| m.fqn.eq_ignore_ascii_case(&class));
                let base = class.rsplit('\\').next().unwrap_or(&class);
                let key = model
                    .and_then(|m| m.primary_key.clone())
                    .unwrap_or_else(|| "id".into());
                let column = s(1).unwrap_or_else(|| format!("{}_{key}", snake(base)));
                let table = model
                    .map(|m| m.table.clone())
                    .unwrap_or_else(|| inflector::plural(&snake(base)));
                let ty = match model {
                    Some(m) if m.uses_trait("HasUuids") => "uuid",
                    Some(m) if m.uses_trait("HasUlids") => "ulid",
                    _ => "bigint",
                };
                let mut c = col(&column, ty);
                c.unsigned = ty == "bigint";
                if model.is_none() {
                    c.confidence = Confidence::Low;
                }
                Some(vec![ColumnDef {
                    col: c,
                    fk_hint: Some((table, key)),
                }])
            }
            "morphs" | "nullableMorphs" | "uuidMorphs" | "nullableUuidMorphs" | "ulidMorphs"
            | "nullableUlidMorphs" => {
                let base = name()?;
                let nullable = method.starts_with("nullable");
                let id_type = if method.contains("Uuid") {
                    "uuid"
                } else if method.contains("Ulid") {
                    "ulid"
                } else {
                    "bigint"
                };
                let mut ty = col(&format!("{base}_type"), "varchar(255)");
                let mut id = col(&format!("{base}_id"), id_type);
                id.unsigned = id_type == "bigint";
                for c in [&mut ty, &mut id] {
                    c.nullable = nullable;
                    c.indexed = true;
                }
                Some(vec![
                    ColumnDef {
                        col: ty,
                        fk_hint: None,
                    },
                    ColumnDef {
                        col: id,
                        fk_hint: None,
                    },
                ])
            }
            "timestamps" | "nullableTimestamps" | "timestampsTz" | "datetimes" => {
                let ty = match method {
                    "timestampsTz" => "timestamptz",
                    "datetimes" => "datetime",
                    _ => "timestamp",
                };
                Some(
                    ["created_at", "updated_at"]
                        .iter()
                        .map(|n| {
                            let mut c = col(n, ty);
                            c.nullable = true;
                            ColumnDef {
                                col: c,
                                fk_hint: None,
                            }
                        })
                        .collect(),
                )
            }
            "softDeletes" | "softDeletesTz" | "softDeletesDatetime" => {
                let mut c = col(
                    &s(0).unwrap_or_else(|| "deleted_at".into()),
                    match method {
                        "softDeletesTz" => "timestamptz",
                        "softDeletesDatetime" => "datetime",
                        _ => "timestamp",
                    },
                );
                c.nullable = true;
                one(c)
            }
            "rememberToken" => {
                let mut c = col("remember_token", "varchar(100)");
                c.nullable = true;
                one(c)
            }
            _ => None,
        }
    }

    fn add_or_change(&mut self, def: ColumnDef, mods: &Mods, line: u32) {
        let at = self.at(line);
        let conditional = self.conditional;
        let major = self.ctx.laravel_major;
        let ColumnDef { mut col, fk_hint } = def;
        apply_mods(&mut col, mods, fk_hint);
        if conditional {
            col.confidence = Confidence::Low;
        }
        col.last_changed_in = Some(at.clone());
        let Some(table) = self.table() else {
            return;
        };
        table.last_changed_in = Some(at.clone());
        if mods.change {
            if let Some(existing) = table.column_mut(&col.name) {
                existing.r#type = col.r#type;
                if major.is_some_and(|m| m < 11) {
                    // Before Laravel 11, change() kept attributes that were not restated.
                    if let Some(n) = mods.nullable {
                        existing.nullable = n;
                    }
                    if mods.default.is_some() {
                        existing.default = col.default;
                    }
                    if mods.unsigned.is_some() {
                        existing.unsigned = col.unsigned;
                    }
                } else {
                    // Laravel 11+: the column is redefined; unstated modifiers are dropped.
                    existing.nullable = col.nullable;
                    existing.default = col.default;
                    existing.unsigned = col.unsigned;
                }
                existing.unique |= col.unique;
                existing.indexed |= col.indexed;
                existing.last_changed_in = Some(at);
                if conditional {
                    existing.confidence = Confidence::Low;
                }
                return;
            }
            let name = col.name.clone();
            self.warn(
                line,
                format!("->change() on unknown column {name}; added instead"),
            );
            let Some(table) = self.table() else { return };
            col.created_in = Some(at);
            table.columns.push(col);
            return;
        }
        col.created_in = Some(at);
        if let Some(pos) = table.columns.iter().position(|c| c.name == col.name) {
            table.columns.remove(pos);
        }
        let position = if mods.first {
            0
        } else if let Some(after) = &mods.after {
            table
                .columns
                .iter()
                .position(|c| &c.name == after)
                .map_or(table.columns.len(), |p| p + 1)
        } else {
            table.columns.len()
        };
        table.columns.insert(position, col);
    }

    fn command(&mut self, method: &str, a: &[Lit], mods: &Mods, line: u32) {
        let at = self.at(line);
        let strings = |i: usize| a.get(i).and_then(Lit::strings).unwrap_or_default();
        let all_strings = || -> Vec<String> {
            a.iter()
                .flat_map(|l| l.strings().unwrap_or_default())
                .collect()
        };
        let table_name = self.table.clone();
        let Some(table) = self.table() else {
            return;
        };
        table.last_changed_in = Some(at.clone());
        let touch = |c: &mut Column| c.last_changed_in = Some(at.clone());
        match method {
            "primary" | "unique" | "index" | "fullText" | "spatialIndex" => {
                let cols = strings(0);
                let single = cols.len() == 1;
                for name in &cols {
                    if let Some(c) = table.column_mut(name) {
                        match method {
                            "primary" => c.primary = true,
                            "unique" if single => c.unique = true,
                            _ => c.indexed = true,
                        }
                        touch(c);
                    }
                }
            }
            "foreign" => {
                for name in strings(0) {
                    if let Some(c) = table.column_mut(&name) {
                        c.fk_column = mods.references.clone().or(Some("id".into()));
                        c.fk_table = mods.on.clone();
                        touch(c);
                    }
                }
            }
            "dropColumn" => {
                let cols = all_strings();
                table.columns.retain(|c| !cols.contains(&c.name));
            }
            "renameColumn" => {
                if let (Some(from), Some(to)) = (
                    a.first().and_then(Lit::as_str),
                    a.get(1).and_then(Lit::as_str),
                ) {
                    match table.column_mut(from) {
                        Some(c) => {
                            c.name = to.to_owned();
                            touch(c);
                        }
                        None => {
                            let msg = format!("renameColumn: unknown column {table_name}.{from}");
                            self.warn(line, msg);
                        }
                    }
                }
            }
            "dropForeign" | "dropIndex" | "dropUnique" | "dropPrimary" => {
                let suffix = match method {
                    "dropForeign" => "_foreign",
                    "dropIndex" => "_index",
                    "dropUnique" => "_unique",
                    _ => "_primary",
                };
                let cols: Vec<String> = match a.first() {
                    Some(Lit::Arr(_)) => strings(0),
                    Some(Lit::Str(index)) => index
                        .strip_prefix(&format!("{table_name}_"))
                        .and_then(|r| r.strip_suffix(suffix))
                        .map(|c| vec![c.to_owned()])
                        .unwrap_or_default(),
                    _ => table.columns.iter().map(|c| c.name.clone()).collect(),
                };
                for name in cols {
                    if let Some(c) = table.column_mut(&name) {
                        match method {
                            "dropForeign" => {
                                c.fk_table = None;
                                c.fk_column = None;
                            }
                            "dropIndex" => c.indexed = false,
                            "dropUnique" => c.unique = false,
                            _ => c.primary = false,
                        }
                        touch(c);
                    }
                }
            }
            "dropTimestamps" | "dropTimestampsTz" => {
                table
                    .columns
                    .retain(|c| c.name != "created_at" && c.name != "updated_at");
            }
            "dropSoftDeletes" | "dropSoftDeletesTz" => {
                let name = a
                    .first()
                    .and_then(Lit::as_str)
                    .unwrap_or("deleted_at")
                    .to_owned();
                table.columns.retain(|c| c.name != name);
            }
            "dropRememberToken" => table.columns.retain(|c| c.name != "remember_token"),
            "dropMorphs" => {
                if let Some(base) = a.first().and_then(Lit::as_str) {
                    let (t, i) = (format!("{base}_type"), format!("{base}_id"));
                    table.columns.retain(|c| c.name != t && c.name != i);
                }
            }
            "dropConstrainedForeignId" => {
                let name = a
                    .first()
                    .and_then(Lit::as_str)
                    .unwrap_or_default()
                    .to_owned();
                table.columns.retain(|c| c.name != name);
            }
            "rename" => {
                if let Some(to) = a.first().and_then(Lit::as_str) {
                    let to = to.to_owned();
                    rename_table(self.schema, &table_name, &to, &at);
                    self.table = to;
                }
            }
            "engine" | "charset" | "collation" | "comment" | "temporary" | "create" | "drop"
            | "dropIfExists" => {}
            other => {
                // An unknown method with a column name is still recorded as a column.
                if let Some(name) = a.first().and_then(Lit::as_str) {
                    let mut c = col(name, &format!("unknown({other})"));
                    c.created_in = Some(at.clone());
                    c.last_changed_in = Some(at);
                    c.confidence = Confidence::Low;
                    table.columns.push(c);
                }
                self.warn(line, format!("unknown Blueprint method ->{other}()"));
            }
        }
    }
}

fn parse_mods(calls: &[Call<'_>], lits: &[Vec<Lit>]) -> Mods {
    let mut m = Mods::default();
    for (c, a) in calls.iter().zip(lits) {
        let s = |i: usize| a.get(i).and_then(Lit::as_str).map(str::to_owned);
        match c.name.as_str() {
            "nullable" => m.nullable = Some(!matches!(a.first(), Some(Lit::Bool(false)))),
            "default" => m.default = a.first().map(Lit::display),
            "unsigned" => m.unsigned = Some(true),
            "unique" => m.unique = true,
            "index" => m.index = true,
            "primary" => m.primary = true,
            "autoIncrement" => m.auto_increment = true,
            "after" => m.after = s(0),
            "first" => m.first = true,
            "change" => m.change = true,
            "useCurrent" => m.default = Some("CURRENT_TIMESTAMP".into()),
            "constrained" => m.constrained = Some((s(0), s(1))),
            "references" => m.references = s(0),
            "on" => m.on = s(0),
            "comment"
            | "charset"
            | "collation"
            | "invisible"
            | "storedAs"
            | "virtualAs"
            | "generatedAs"
            | "always"
            | "from"
            | "startingValue"
            | "useCurrentOnUpdate"
            | "onUpdate"
            | "onDelete"
            | "cascadeOnDelete"
            | "cascadeOnUpdate"
            | "restrictOnDelete"
            | "restrictOnUpdate"
            | "nullOnDelete"
            | "nullOnUpdate"
            | "noActionOnDelete"
            | "noActionOnUpdate"
            | "persisted"
            | "type"
            | "lock"
            | "algorithm"
            | "deferrable"
            | "initiallyImmediate"
            | "notInitiallyImmediate" => {}
            other => m.unknown.push(other.to_owned()),
        }
    }
    m
}

fn apply_mods(c: &mut Column, m: &Mods, fk_hint: Option<(String, String)>) {
    if let Some(n) = m.nullable {
        c.nullable = n;
    }
    if m.default.is_some() {
        c.default = m.default.clone();
    }
    if let Some(u) = m.unsigned {
        c.unsigned = u;
    }
    c.unique |= m.unique;
    c.indexed |= m.index;
    c.primary |= m.primary;
    c.auto_increment |= m.auto_increment;
    if let Some((table, column)) = &m.constrained {
        let (hint_table, hint_col) = fk_hint.unzip();
        let column = column.clone().or(hint_col).unwrap_or_else(|| "id".into());
        let table = table.clone().or(hint_table).unwrap_or_else(|| {
            // Laravel: Str::of($name)->beforeLast('_'.$column)->plural()
            let base = c
                .name
                .strip_suffix(&format!("_{column}"))
                .unwrap_or(&c.name);
            inflector::plural(base)
        });
        c.fk_table = Some(table);
        c.fk_column = Some(column);
        c.indexed = true;
    }
    if m.references.is_some() || m.on.is_some() {
        c.fk_column = m.references.clone().or(Some("id".into()));
        c.fk_table = m.on.clone();
    }
}

/// `$table->a(...)->b(...)` rooted at the closure variable.
fn chain<'t>(node: Node<'t>, src: &[u8], var: &str) -> Option<Vec<Call<'t>>> {
    if node.kind() != "member_call_expression" {
        return None;
    }
    let object = node.child_by_field_name("object")?;
    let name = text(node.child_by_field_name("name")?, src).to_owned();
    let args = node
        .child_by_field_name("arguments")
        .map(|a| {
            named_children(a)
                .into_iter()
                .filter(|x| x.kind() == "argument")
                .filter_map(|x| named_children(x).into_iter().last())
                .collect()
        })
        .unwrap_or_default();
    let mut calls = if object.kind() == "variable_name" {
        if text(object, src) != var {
            return None;
        }
        Vec::new()
    } else {
        chain(object, src, var)?
    };
    calls.push(Call { name, args });
    Some(calls)
}
