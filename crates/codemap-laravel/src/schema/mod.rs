//! Database schema reconstructed by replaying migrations (`up()` only) in
//! filename order, on top of a squashed schema dump when one exists.

pub mod blueprint;
pub mod dump;
pub mod live;
pub mod raw;
pub mod supabase;

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use codemap_core::Result;
use codemap_core::envelope::Detail;
use codemap_php::Scope;
use codemap_php::ast::{apply_use_declaration, named_children, start_line, text};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tree_sitter::Node;

use crate::config_php::ConfigReader;
use crate::literal::{self, Lit};
use crate::models::ModelInfo;
use crate::routes::eval::Warning;
use blueprint::Blueprint;

const SCHEMA_FACADE: &str = "Illuminate\\Support\\Facades\\Schema";
const DB_FACADE: &str = "Illuminate\\Support\\Facades\\DB";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    #[default]
    High,
    /// Applied inside a condition, or derived by inference.
    Low,
}

impl Confidence {
    fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Low => "low",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    pub r#type: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub unsigned: bool,
    pub unique: bool,
    pub indexed: bool,
    pub primary: bool,
    pub auto_increment: bool,
    pub fk_table: Option<String>,
    pub fk_column: Option<String>,
    pub created_in: Option<String>,
    pub last_changed_in: Option<String>,
    pub confidence: Confidence,
    /// `live:local`, `dump:structure`; `None`: migrations / the schema dump.
    pub source: Option<String>,
    /// How the other source differs (`not in database live:local`).
    pub drift: Option<String>,
}

/// An index over several columns. Single-column ones are flags on the column.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableIndex {
    /// As Laravel names it: `labels_shelf_id_slug_unique`, or the name given.
    pub name: String,
    /// primary | unique | index | fullText | spatialIndex
    pub kind: String,
    pub columns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defined_in: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Table {
    pub name: String,
    pub columns: Vec<Column>,
    pub created_in: Option<String>,
    pub last_changed_in: Option<String>,
    pub dropped: bool,
    pub renamed_from: Option<String>,
    pub confidence: Confidence,
    pub source: Option<String>,
    pub drift: Option<String>,
    /// Indexes over several columns.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<TableIndex>,
}

impl Table {
    pub fn column_mut(&mut self, name: &str) -> Option<&mut Column> {
        self.columns.iter_mut().find(|c| c.name == name)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Schema {
    pub tables: Vec<Table>,
    pub warnings: Vec<Warning>,
    /// Migrations that were replayed / already contained in the dump.
    pub replayed: Vec<String>,
    pub from_dump: Vec<String>,
    pub dump_file: Option<String>,
}

impl Schema {
    pub fn table_mut(&mut self, name: &str) -> Option<&mut Table> {
        self.tables
            .iter_mut()
            .find(|t| t.name == name && !t.dropped)
    }

    fn warn(&mut self, file: &str, line: Option<u32>, message: impl Into<String>) {
        self.warnings.push(Warning {
            file: Some(file.to_owned()),
            line,
            message: message.into(),
        });
    }
}

/// Facts the replay needs from elsewhere in the index.
pub struct ReplayContext<'a> {
    pub models: &'a [ModelInfo],
    /// Laravel major version: `change()` semantics differ before 11.
    pub laravel_major: Option<u32>,
}

pub fn replay(root: &Path, ctx: &ReplayContext<'_>) -> Schema {
    let mut schema = Schema::default();

    // 1. Squashed schema dump.
    let mut dumped = BTreeSet::new();
    let dumps = list_files(root, "database/schema", "-schema.sql");
    if let Some(dump_rel) = dumps.first() {
        if dumps.len() > 1 {
            schema.warn(
                dump_rel,
                None,
                format!(
                    "several schema dumps ({}); using {dump_rel}",
                    dumps.join(", ")
                ),
            );
        }
        if let Ok(sql) = std::fs::read_to_string(root.join(dump_rel)) {
            let parsed = dump::parse(&sql, dump_rel);
            dumped = parsed.migrations.iter().cloned().collect();
            schema.tables = parsed.tables;
            schema.warnings.extend(parsed.warnings);
            schema.dump_file = Some(dump_rel.clone());
        }
    }

    // 2. Migrations not already in the dump, in filename order.
    for rel in list_files(root, "database/migrations", ".php") {
        let name = rel
            .rsplit('/')
            .next()
            .unwrap_or(&rel)
            .trim_end_matches(".php")
            .to_owned();
        if dumped.contains(&name) {
            schema.from_dump.push(name);
            continue;
        }
        let Ok(src) = std::fs::read(root.join(&rel)) else {
            continue;
        };
        replay_migration(&mut schema, root, &rel, &src, ctx);
        schema.replayed.push(name);
    }
    schema
}

/// Relative paths of files in `dir` ending with `suffix`, sorted by name.
fn list_files(root: &Path, dir: &str, suffix: &str) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(root.join(dir))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(suffix))
        .map(|n| format!("{dir}/{n}"))
        .collect();
    out.sort();
    out
}

struct MigrationCtx<'a, 'c> {
    rel: &'a str,
    src: &'a [u8],
    scope: Scope,
    replay: &'a ReplayContext<'c>,
    config: &'a ConfigReader,
    /// `$var = <static value>` assignments seen so far in `up()`; the flag
    /// marks values that came from an `env()` default.
    vars: RefCell<HashMap<String, (Lit, bool)>>,
    /// Literal constants and properties of the migration class
    /// (`self::TABLES`, `$this->indexes`).
    members: HashMap<String, Lit>,
    /// Body of the migration class, for `$this->helper()` methods.
    class_body: Option<Node<'a>>,
    depth: std::cell::Cell<u8>,
    /// A `return` on a path that runs was reached: the rest of `up()` does not run.
    returned: std::cell::Cell<bool>,
}

impl MigrationCtx<'_, '_> {
    /// `$this->tables()` → the literal its `return` produces; `(new Model)->getTable()`
    /// → that model's table.
    fn eval_call(&self, n: Node<'_>) -> Option<(Lit, bool)> {
        let object = n.child_by_field_name("object")?;
        let name = text(n.child_by_field_name("name")?, self.src);
        let has_args = n
            .child_by_field_name("arguments")
            .is_some_and(|a| named_children(a).iter().any(|x| x.kind() == "argument"));
        if object.kind() == "variable_name" && text(object, self.src) == "$this" && !has_args {
            if self.depth.get() > 4 {
                return None;
            }
            let method = named_children(self.class_body?).into_iter().find(|m| {
                m.kind() == "method_declaration"
                    && m.child_by_field_name("name").map(|x| text(x, self.src)) == Some(name)
            })?;
            let ret = crate::models::return_expression(method.child_by_field_name("body")?)?;
            self.depth.set(self.depth.get() + 1);
            let value = self.eval(ret);
            self.depth.set(self.depth.get() - 1);
            return (!matches!(value.0, Lit::Raw(_))).then_some(value);
        }
        if name == "getTable" {
            let created = if object.kind() == "parenthesized_expression" {
                named_children(object).into_iter().next()?
            } else {
                object
            };
            if created.kind() != "object_creation_expression" {
                return None;
            }
            let class = named_children(created)
                .into_iter()
                .find(|c| matches!(c.kind(), "name" | "qualified_name"))?;
            let fqn = self.scope.resolve_class(text(class, self.src))?;
            let model = self
                .replay
                .models
                .iter()
                .find(|m| m.fqn.eq_ignore_ascii_case(&fqn))?;
            return Some((Lit::Str(model.table.clone()), model.table_inferred));
        }
        None
    }
    /// Literal value of an expression, also resolving `config('a.b')`,
    /// tracked variables, `$var['key']` and `a ?? b`. The flag is true when
    /// the value depends on an `env()` default.
    fn eval(&self, n: Node<'_>) -> (Lit, bool) {
        match n.kind() {
            "function_call_expression" => {
                let name = n
                    .child_by_field_name("function")
                    .map(|f| text(f, self.src))
                    .unwrap_or("");
                if name.trim_start_matches('\\') != "config" {
                    return (literal::eval(n, self.src, &self.scope), false);
                }
                let args: Vec<Node<'_>> = n
                    .child_by_field_name("arguments")
                    .map(|a| {
                        named_children(a)
                            .into_iter()
                            .filter(|x| x.kind() == "argument")
                            .filter_map(|x| named_children(x).into_iter().last())
                            .collect()
                    })
                    .unwrap_or_default();
                let key = args
                    .first()
                    .map(|k| literal::eval(*k, self.src, &self.scope));
                match key
                    .as_ref()
                    .and_then(Lit::as_str)
                    .and_then(|k| self.config.get(k))
                {
                    Some(v) => (v.value, v.from_env_default),
                    None => match args.get(1) {
                        Some(default) => (literal::eval(*default, self.src, &self.scope), true),
                        // config('file.missing') is null when the file is known.
                        None if key
                            .as_ref()
                            .and_then(Lit::as_str)
                            .is_some_and(|k| self.config.has_file(k)) =>
                        {
                            (Lit::Null, false)
                        }
                        None => (literal::eval(n, self.src, &self.scope), false),
                    },
                }
            }
            "variable_name" => self
                .vars
                .borrow()
                .get(text(n, self.src))
                .cloned()
                .unwrap_or_else(|| (Lit::Raw(text(n, self.src).to_owned()), false)),
            "subscript_expression" => {
                let parts = named_children(n);
                let (Some(base), Some(index)) = (parts.first(), parts.get(1)) else {
                    return (literal::eval(n, self.src, &self.scope), false);
                };
                let (base, low) = self.eval(*base);
                let key = literal::eval(*index, self.src, &self.scope);
                match (base, key.as_str()) {
                    (Lit::Arr(items), Some(k)) => items
                        .into_iter()
                        .find(|(ik, _)| ik.as_deref() == Some(k))
                        .map_or_else(
                            || (Lit::Raw(text(n, self.src).to_owned()), false),
                            |(_, v)| (v, low),
                        ),
                    _ => (Lit::Raw(text(n, self.src).to_owned()), false),
                }
            }
            "binary_expression"
                if n.child_by_field_name("operator").map(|o| text(o, self.src)) == Some("??") =>
            {
                let left = n.child_by_field_name("left").map(|l| self.eval(l));
                match left {
                    Some((v, low)) if !matches!(v, Lit::Raw(_) | Lit::Null) => (v, low),
                    _ => n
                        .child_by_field_name("right")
                        .map_or((Lit::Null, false), |r| self.eval(r)),
                }
            }
            "parenthesized_expression" => named_children(n)
                .first()
                .map_or((Lit::Null, false), |c| self.eval(*c)),
            "member_call_expression" => self
                .eval_call(n)
                .unwrap_or_else(|| (literal::eval(n, self.src, &self.scope), false)),
            "unary_op_expression" if text(n, self.src).trim_start().starts_with('!') => {
                match named_children(n).first().map(|c| self.eval(*c)) {
                    Some((v, low)) => match literal::truthy(&v) {
                        Some(b) => (Lit::Bool(!b), low),
                        None => (Lit::Raw(text(n, self.src).to_owned()), false),
                    },
                    None => (Lit::Raw(text(n, self.src).to_owned()), false),
                }
            }
            "binary_expression"
                if matches!(
                    n.child_by_field_name("operator")
                        .map(|o| text(o, self.src).to_ascii_lowercase())
                        .as_deref(),
                    Some("&&" | "||" | "and" | "or")
                ) =>
            {
                let op = n
                    .child_by_field_name("operator")
                    .map(|o| text(o, self.src).to_ascii_lowercase());
                let is_and = matches!(op.as_deref(), Some("&&" | "and"));
                let side = |f: &str| {
                    n.child_by_field_name(f)
                        .map(|x| self.eval(x))
                        .and_then(|(v, _)| literal::truthy(&v))
                };
                let (l, r) = (side("left"), side("right"));
                let known = if is_and {
                    match (l, r) {
                        (Some(false), _) | (_, Some(false)) => Some(false),
                        (Some(true), Some(true)) => Some(true),
                        _ => None,
                    }
                } else {
                    match (l, r) {
                        (Some(true), _) | (_, Some(true)) => Some(true),
                        (Some(false), Some(false)) => Some(false),
                        _ => None,
                    }
                };
                known.map_or_else(
                    || (Lit::Raw(text(n, self.src).to_owned()), false),
                    |b| (Lit::Bool(b), false),
                )
            }
            // Elements may use tracked variables: ["{$field}_en", $other].
            "array_creation_expression" => {
                let mut items = Vec::new();
                let mut low = false;
                for el in named_children(n) {
                    if el.kind() != "array_element_initializer" {
                        continue;
                    }
                    match named_children(el).as_slice() {
                        [value] if value.kind() != "variadic_unpacking" => {
                            let (v, l) = self.eval(*value);
                            low |= l;
                            items.push((None, v));
                        }
                        [key, value] => {
                            let key = match self.eval(*key).0 {
                                Lit::Str(k) | Lit::Class(k) => k,
                                Lit::Int(i) => i.to_string(),
                                other => other.display(),
                            };
                            let (v, l) = self.eval(*value);
                            low |= l;
                            items.push((Some(key), v));
                        }
                        _ => return (literal::eval(n, self.src, &self.scope), false),
                    }
                }
                (Lit::Arr(items), low)
            }
            "class_constant_access_expression" => {
                let parts = named_children(n);
                let scope = parts
                    .first()
                    .map(|p| text(*p, self.src).to_ascii_lowercase());
                let name = parts.last().map(|p| text(*p, self.src));
                match (scope.as_deref(), name) {
                    (Some("self" | "static"), Some(name)) if self.members.contains_key(name) => {
                        (self.members[name].clone(), false)
                    }
                    _ => (literal::eval(n, self.src, &self.scope), false),
                }
            }
            "member_access_expression" => {
                let object = n.child_by_field_name("object").map(|o| text(o, self.src));
                let name = n.child_by_field_name("name").map(|o| text(o, self.src));
                match (object, name) {
                    (Some("$this"), Some(name))
                        if self.members.contains_key(&format!("${name}")) =>
                    {
                        (self.members[&format!("${name}")].clone(), false)
                    }
                    _ => (literal::eval(n, self.src, &self.scope), false),
                }
            }
            // "campaign_{$ref}" with a tracked variable.
            "encapsed_string" => {
                let mut out = String::new();
                let mut low = false;
                for c in named_children(n) {
                    match c.kind() {
                        "string_content" => out.push_str(text(c, self.src)),
                        "escape_sequence" => out.push_str(&literal::unquote_double(&format!(
                            "\"{}\"",
                            text(c, self.src)
                        ))),
                        "variable_name" => match self.eval(c) {
                            (Lit::Str(v), l) => {
                                out.push_str(&v);
                                low |= l;
                            }
                            (Lit::Int(i), l) => {
                                out.push_str(&i.to_string());
                                low |= l;
                            }
                            _ => return (literal::eval(n, self.src, &self.scope), false),
                        },
                        _ => return (literal::eval(n, self.src, &self.scope), false),
                    }
                }
                (Lit::Str(out), low)
            }
            _ => (literal::eval(n, self.src, &self.scope), false),
        }
    }

    /// Records `$var = <expr>` when the value is statically known.
    fn track_assignment(&self, e: Node<'_>) {
        if e.kind() != "assignment_expression" {
            return;
        }
        let (Some(left), Some(right)) = (
            e.child_by_field_name("left"),
            e.child_by_field_name("right"),
        ) else {
            return;
        };
        if left.kind() != "variable_name" {
            return;
        }
        let (value, low) = self.eval(right);
        if !matches!(value, Lit::Raw(_)) {
            self.vars
                .borrow_mut()
                .insert(text(left, self.src).to_owned(), (value, low));
        }
    }
}

fn replay_migration(
    schema: &mut Schema,
    project_root: &Path,
    rel: &str,
    src: &[u8],
    ctx: &ReplayContext<'_>,
) {
    let Some(tree) = codemap_php::parse::parse(src) else {
        return;
    };
    let root = tree.root_node();
    let mut scope = Scope::default();
    for child in named_children(root) {
        match child.kind() {
            "namespace_use_declaration" => apply_use_declaration(child, src, &mut scope),
            "namespace_definition" => {
                let name = child
                    .child_by_field_name("name")
                    .map(|n| text(n, src))
                    .unwrap_or("");
                scope.set_namespace(name);
            }
            _ => {}
        }
    }
    let Some(up) = find_method(root, src, "up") else {
        schema.warn(rel, None, "no up() method found");
        return;
    };
    let Some(body) = up.child_by_field_name("body") else {
        return;
    };
    let config = ConfigReader::new(project_root);
    let members = class_members(up, src, &scope);
    let class_body = up.parent();
    let m = MigrationCtx {
        rel,
        src,
        scope,
        replay: ctx,
        config: &config,
        vars: RefCell::new(HashMap::new()),
        members,
        class_body,
        depth: std::cell::Cell::new(0),
        returned: std::cell::Cell::new(false),
    };
    statements(schema, &m, body, false);
}

/// Literal constants (`NAME`) and properties (`$name`) of the class that
/// declares `method`.
fn class_members(method: Node<'_>, src: &[u8], scope: &Scope) -> HashMap<String, Lit> {
    let mut out = HashMap::new();
    let Some(body) = method.parent() else {
        return out;
    };
    for member in named_children(body) {
        match member.kind() {
            "const_declaration" => {
                for el in named_children(member)
                    .into_iter()
                    .filter(|e| e.kind() == "const_element")
                {
                    let parts = named_children(el);
                    if let (Some(name), Some(value)) = (parts.first(), parts.get(1)) {
                        out.insert(
                            text(*name, src).to_owned(),
                            literal::eval(*value, src, scope),
                        );
                    }
                }
            }
            "property_declaration" => {
                for el in named_children(member)
                    .into_iter()
                    .filter(|e| e.kind() == "property_element")
                {
                    if let (Some(name), Some(value)) = (
                        el.child_by_field_name("name"),
                        el.child_by_field_name("default_value"),
                    ) {
                        out.insert(text(name, src).to_owned(), literal::eval(value, src, scope));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn find_method<'t>(node: Node<'t>, src: &[u8], name: &str) -> Option<Node<'t>> {
    if node.kind() == "method_declaration"
        && node.child_by_field_name("name").map(|n| text(n, src)) == Some(name)
    {
        return Some(node);
    }
    named_children(node)
        .into_iter()
        .find_map(|c| find_method(c, src, name))
}

fn statements(schema: &mut Schema, m: &MigrationCtx<'_, '_>, node: Node<'_>, conditional: bool) {
    for s in named_children(node) {
        if m.returned.get() {
            return;
        }
        match s.kind() {
            // `if (Schema::hasTable('items')) { return; }` with the table there:
            // nothing after it runs.
            "return_statement" if !conditional => {
                m.returned.set(true);
                return;
            }
            "expression_statement" => {
                if let Some(e) = named_children(s).into_iter().next() {
                    m.track_assignment(e);
                    expression(schema, m, e, conditional);
                }
            }
            "compound_statement" | "colon_block" => statements(schema, m, s, conditional),
            // `foreach (['orders', 'reservations'] as $table)` over a literal
            // array runs the body once per value, like PHP would.
            "foreach_statement" if foreach_literal(schema, m, s, conditional) => {}
            // A condition with a known value (e.g. `if ($teams)` from config) is followed.
            "if_statement" if if_known(schema, m, s, conditional) => {}
            "if_statement" | "else_clause" | "else_if_clause" | "foreach_statement"
            | "for_statement" | "while_statement" | "try_statement" | "catch_clause" => {
                for c in named_children(s) {
                    if !matches!(c.kind(), "parenthesized_expression") {
                        statements_or_one(schema, m, c, true);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Follows an `if` / `elseif` / `else` chain whose conditions are statically
/// known. Returns false (caller treats every branch as conditional) otherwise.
fn if_known(schema: &mut Schema, m: &MigrationCtx<'_, '_>, s: Node<'_>, conditional: bool) -> bool {
    let truth = |node: Node<'_>| -> Option<bool> {
        let c = node.child_by_field_name("condition")?;
        schema_truth(schema, m, c).or_else(|| literal::truthy(&m.eval(c).0))
    };
    let mut branch = Some(s);
    let mut alternatives = {
        let mut cursor = s.walk();
        s.children_by_field_name("alternative", &mut cursor)
            .collect::<Vec<_>>()
            .into_iter()
    };
    while let Some(node) = branch {
        let take = if node.kind() == "else_clause" {
            Some(true)
        } else {
            truth(node)
        };
        match take {
            Some(true) => {
                if let Some(body) = node.child_by_field_name("body") {
                    statements_or_one(schema, m, body, conditional);
                }
                return true;
            }
            Some(false) => branch = alternatives.next(),
            None => {
                return node.id() != s.id() && {
                    // An unknown `elseif` after known-false ones: the rest is conditional.
                    for rest in std::iter::once(node).chain(alternatives) {
                        if let Some(body) = rest.child_by_field_name("body") {
                            statements_or_one(schema, m, body, true);
                        }
                    }
                    true
                };
            }
        }
    }
    true
}

/// `Schema::hasTable('items')`, `! Schema::hasColumn('items', 'data')`:
/// known from the migrations replayed so far.
fn schema_truth(schema: &Schema, m: &MigrationCtx<'_, '_>, c: Node<'_>) -> Option<bool> {
    match c.kind() {
        "parenthesized_expression" => {
            schema_truth(schema, m, named_children(c).into_iter().next()?)
        }
        "unary_op_expression" if text(c, m.src).trim_start().starts_with('!') => Some(
            !schema_truth(schema, m, named_children(c).into_iter().last()?)?,
        ),
        "scoped_call_expression" => {
            let class = c.child_by_field_name("scope").map(|n| text(n, m.src))?;
            if !class.trim_start_matches('\\').ends_with("Schema") {
                return None;
            }
            let method = c.child_by_field_name("name").map(|n| text(n, m.src))?;
            let args: Vec<String> = c
                .child_by_field_name("arguments")
                .map(|a| {
                    named_children(a)
                        .into_iter()
                        .filter_map(|arg| named_children(arg).into_iter().last())
                        .filter_map(|v| m.eval(v).0.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let table = |name: &str| schema.tables.iter().find(|t| t.name == name && !t.dropped);
            match method {
                "hasTable" => Some(table(args.first()?).is_some()),
                "hasColumn" => {
                    let t = table(args.first()?)?;
                    Some(t.columns.iter().any(|col| Some(&col.name) == args.get(1)))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Expands a `foreach` over a statically known array. Returns false (so the
/// caller falls back to "conditional") when the array or variables are dynamic.
fn foreach_literal(
    schema: &mut Schema,
    m: &MigrationCtx<'_, '_>,
    s: Node<'_>,
    conditional: bool,
) -> bool {
    let kids = named_children(s);
    let (Some(iterated), Some(target), Some(body)) =
        (kids.first(), kids.get(1), s.child_by_field_name("body"))
    else {
        return false;
    };
    let (key_var, value_var) = match target.kind() {
        "variable_name" => (None, text(*target, m.src).to_owned()),
        "pair" => {
            let parts = named_children(*target);
            match (parts.first(), parts.get(1)) {
                (Some(k), Some(v))
                    if k.kind() == "variable_name" && v.kind() == "variable_name" =>
                {
                    (Some(text(*k, m.src).to_owned()), text(*v, m.src).to_owned())
                }
                _ => return false,
            }
        }
        _ => return false,
    };
    let (Lit::Arr(items), low) = m.eval(*iterated) else {
        return false;
    };
    for (i, (key, value)) in items.into_iter().enumerate() {
        if let Some(k) = &key_var {
            let key = key.map_or_else(|| Lit::Int(i as i64), Lit::Str);
            m.vars.borrow_mut().insert(k.clone(), (key, low));
        }
        m.vars.borrow_mut().insert(value_var.clone(), (value, low));
        statements_or_one(schema, m, body, conditional);
    }
    true
}

fn statements_or_one(
    schema: &mut Schema,
    m: &MigrationCtx<'_, '_>,
    node: Node<'_>,
    conditional: bool,
) {
    match node.kind() {
        "compound_statement" | "colon_block" => statements(schema, m, node, conditional),
        "expression_statement" => {
            if let Some(e) = named_children(node).into_iter().next() {
                expression(schema, m, e, conditional);
            }
        }
        _ => statements(schema, m, node, conditional),
    }
}

struct Call<'t> {
    name: String,
    args: Vec<Node<'t>>,
}

/// Flattens `Facade::a(...)->b(...)` into calls when the facade resolves to `facade`.
fn facade_chain<'t>(
    node: Node<'t>,
    src: &[u8],
    scope: &Scope,
    facade: &str,
) -> Option<Vec<Call<'t>>> {
    match node.kind() {
        "member_call_expression" => {
            let mut calls = facade_chain(node.child_by_field_name("object")?, src, scope, facade)?;
            calls.push(call(node, src)?);
            Some(calls)
        }
        "scoped_call_expression" => {
            let raw = text(node.child_by_field_name("scope")?, src);
            let short = facade.rsplit('\\').next().unwrap_or(facade);
            let ok = raw.trim_start_matches('\\') == short
                || scope.resolve_class(raw).as_deref() == Some(facade);
            ok.then(|| call(node, src).map(|c| vec![c]))?
        }
        _ => None,
    }
}

fn call<'t>(node: Node<'t>, src: &[u8]) -> Option<Call<'t>> {
    Some(Call {
        name: text(node.child_by_field_name("name")?, src).to_owned(),
        args: node
            .child_by_field_name("arguments")
            .map(|a| {
                named_children(a)
                    .into_iter()
                    .filter(|x| x.kind() == "argument")
                    .filter_map(|x| named_children(x).into_iter().last())
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn expression(schema: &mut Schema, m: &MigrationCtx<'_, '_>, e: Node<'_>, conditional: bool) {
    let line = start_line(e);
    let at = format!("{}:{line}", m.rel);
    if let Some(calls) = facade_chain(e, m.src, &m.scope, DB_FACADE) {
        for c in calls.iter().filter(|c| {
            matches!(
                c.name.as_str(),
                "statement" | "unprepared" | "affectingStatement"
            )
        }) {
            match c.args.first().map(|a| m.eval(*a).0) {
                Some(Lit::Str(sql)) => {
                    let at = format!("{}:{line}", m.rel);
                    for stmt in raw::apply(schema, &sql, &at, conditional) {
                        schema.warn(
                            m.rel,
                            Some(line),
                            format!("raw SQL not understood; schema may be incomplete: {stmt}"),
                        );
                    }
                }
                _ => schema.warn(
                    m.rel,
                    Some(line),
                    "raw SQL (DB::statement) is built dynamically; schema may be incomplete",
                ),
            }
        }
        return;
    }
    let Some(calls) = facade_chain(e, m.src, &m.scope, SCHEMA_FACADE) else {
        return;
    };
    let lit = |n: Node<'_>| m.eval(n).0;
    let Some(op) = calls.iter().rev().find(|c| c.name != "connection") else {
        return;
    };
    let from_env = op.args.first().is_some_and(|n| m.eval(*n).1);
    let confidence = if conditional || from_env {
        Confidence::Low
    } else {
        Confidence::High
    };
    let table_arg = op.args.first().map(|n| lit(*n));
    let table_name = table_arg.as_ref().and_then(Lit::as_str).map(str::to_owned);
    match (op.name.as_str(), table_name) {
        ("create" | "table", Some(name)) => {
            let creating = op.name == "create";
            if creating {
                if schema.table_mut(&name).is_some() {
                    schema.warn(m.rel, Some(line), format!("table {name} is created twice"));
                }
                schema.tables.push(Table {
                    name: name.clone(),
                    created_in: Some(at.clone()),
                    last_changed_in: Some(at.clone()),
                    confidence,
                    ..Table::default()
                });
            } else if schema.table_mut(&name).is_none() {
                schema.warn(
                    m.rel,
                    Some(line),
                    format!("Schema::table('{name}') on a table no migration created; columns recorded with low confidence"),
                );
                schema.tables.push(Table {
                    name: name.clone(),
                    confidence: Confidence::Low,
                    ..Table::default()
                });
            }
            if let Some(t) = schema.table_mut(&name) {
                t.last_changed_in = Some(at.clone());
            }
            if let Some(closure) = op.args.get(1) {
                blueprint_closure(schema, m, &name, *closure, conditional);
            }
        }
        ("rename", Some(from)) => {
            let to = op.args.get(1).map(|n| lit(*n));
            match to.as_ref().and_then(Lit::as_str) {
                Some(to) => rename_table(schema, &from, to, &at),
                None => schema.warn(m.rel, Some(line), "Schema::rename target is not a literal"),
            }
        }
        ("drop" | "dropIfExists", Some(name)) => {
            if let Some(t) = schema.table_mut(&name) {
                t.dropped = true;
                t.last_changed_in = Some(at);
            } else if op.name == "drop" {
                schema.warn(m.rel, Some(line), format!("drops unknown table {name}"));
            }
        }
        ("dropColumns", Some(name)) => {
            let cols = op
                .args
                .get(1)
                .map(|n| lit(*n))
                .and_then(|l| l.strings())
                .unwrap_or_default();
            if let Some(t) = schema.table_mut(&name) {
                t.columns.retain(|c| !cols.contains(&c.name));
                t.last_changed_in = Some(at);
            }
        }
        (
            "hasTable"
            | "hasColumn"
            | "hasColumns"
            | "hasIndex"
            | "getColumnListing"
            | "disableForeignKeyConstraints"
            | "enableForeignKeyConstraints"
            | "withoutForeignKeyConstraints"
            | "defaultStringLength",
            _,
        ) => {}
        (other, None)
            if matches!(
                other,
                "create" | "table" | "drop" | "dropIfExists" | "rename"
            ) =>
        {
            schema.warn(
                m.rel,
                Some(line),
                format!(
                    "Schema::{other} table name is not a literal ({})",
                    table_arg.map_or("missing".into(), |l| l.display())
                ),
            );
        }
        (other, _) => schema.warn(
            m.rel,
            Some(line),
            format!("Schema::{other}() is not interpreted"),
        ),
    }
}

fn rename_table(schema: &mut Schema, from: &str, to: &str, at: &str) {
    if let Some(t) = schema.table_mut(from) {
        let mut renamed = t.clone();
        t.dropped = true;
        t.last_changed_in = Some(at.to_owned());
        renamed.name = to.to_owned();
        renamed.renamed_from = Some(from.to_owned());
        renamed.last_changed_in = Some(at.to_owned());
        schema.tables.push(renamed);
    }
}

fn blueprint_closure(
    schema: &mut Schema,
    m: &MigrationCtx<'_, '_>,
    table: &str,
    closure: Node<'_>,
    conditional: bool,
) {
    if !matches!(closure.kind(), "anonymous_function" | "arrow_function") {
        schema.warn(
            m.rel,
            Some(start_line(closure)),
            "Blueprint callback is not a closure",
        );
        return;
    }
    let var = closure
        .child_by_field_name("parameters")
        .and_then(|p| named_children(p).into_iter().next())
        .and_then(|p| p.child_by_field_name("name"))
        .map(|n| text(n, m.src).to_owned())
        .unwrap_or_else(|| "$table".into());
    let Some(body) = closure.child_by_field_name("body") else {
        return;
    };
    let mut bp = Blueprint::new(schema, m.rel, table, m.replay, conditional);
    let eval = |n: Node<'_>| m.eval(n).0;
    if closure.kind() == "arrow_function" {
        bp.statement_expr(body, m.src, &var, conditional, &eval);
    } else {
        bp.statements(body, m.src, &var, conditional, &eval);
    }
}

// ---------------------------------------------------------------- storage

pub fn store(conn: &Connection, schema: &Schema) -> Result<()> {
    conn.execute("DELETE FROM db_tables", [])?;
    conn.execute("DELETE FROM db_columns", [])?;
    for t in &schema.tables {
        // A name can be reused after a drop; the live table wins.
        let live_exists =
            !t.dropped || schema.tables.iter().any(|o| o.name == t.name && !o.dropped);
        if t.dropped && live_exists {
            continue;
        }
        let indexes = (!t.indexes.is_empty())
            .then(|| serde_json::to_string(&t.indexes))
            .transpose()?;
        conn.execute(
            "INSERT OR REPLACE INTO db_tables (name, created_in, last_changed_in, dropped, renamed_from, confidence, source, drift, indexes_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![t.name, t.created_in, t.last_changed_in, t.dropped, t.renamed_from, t.confidence.as_str(), t.source, t.drift, indexes],
        )?;
        if t.dropped {
            continue;
        }
        for (pos, c) in t.columns.iter().enumerate() {
            conn.execute(
                "INSERT OR REPLACE INTO db_columns (table_name, name, position, type, nullable, default_raw,
                     unsigned, is_unique, indexed, is_primary, auto_increment, fk_table, fk_column,
                     created_in, last_changed_in, confidence, source, drift)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
                params![
                    t.name, c.name, pos as i64, c.r#type, c.nullable, c.default, c.unsigned, c.unique,
                    c.indexed, c.primary, c.auto_increment, c.fk_table, c.fk_column, c.created_in,
                    c.last_changed_in, c.confidence.as_str(), c.source, c.drift
                ],
            )?;
        }
    }
    Ok(())
}

pub fn load_columns(conn: &Connection, table: &str) -> Result<Vec<Column>> {
    let mut stmt = conn.prepare_cached(
        "SELECT name, type, nullable, default_raw, unsigned, is_unique, indexed, is_primary,
                auto_increment, fk_table, fk_column, created_in, last_changed_in, confidence, source, drift
         FROM db_columns WHERE table_name = ?1 ORDER BY position",
    )?;
    let rows = stmt.query_map([table], |r| {
        Ok(Column {
            name: r.get(0)?,
            r#type: r.get(1)?,
            nullable: r.get(2)?,
            default: r.get(3)?,
            unsigned: r.get(4)?,
            unique: r.get(5)?,
            indexed: r.get(6)?,
            primary: r.get(7)?,
            auto_increment: r.get(8)?,
            fk_table: r.get(9)?,
            fk_column: r.get(10)?,
            created_in: r.get(11)?,
            last_changed_in: r.get(12)?,
            confidence: if r.get::<_, String>(13)? == "low" {
                Confidence::Low
            } else {
                Confidence::High
            },
            source: r.get(14)?,
            drift: r.get(15)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// (name, dropped, renamed_from, created_in, last_changed_in, confidence, (source, drift))
type TableRow = (
    String,
    bool,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    (Option<String>, Option<String>),
);

fn tables(conn: &Connection) -> Result<Vec<TableRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT name, dropped, renamed_from, created_in, last_changed_in, confidence, source, drift FROM db_tables ORDER BY name",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            (r.get(6)?, r.get(7)?),
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn models_for(conn: &Connection, table: &str) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare_cached(
        "SELECT fqn, table_inferred, file, line FROM models WHERE table_name = ?1 ORDER BY fqn",
    )?;
    let rows = stmt.query_map([table], |r| {
        let inferred: bool = r.get(1)?;
        let file: String = r.get(2)?;
        let line: u32 = r.get(3)?;
        Ok(json!({ "fqn": r.get::<_, String>(0)?, "file": format!("{file}:{line}"), "table_inferred": inferred }))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn column_json(c: &Column, detail: Detail) -> Value {
    let mut v = json!({ "name": c.name, "type": c.r#type, "nullable": c.nullable });
    let o = v.as_object_mut().expect("object");
    if let Some(d) = &c.default {
        o.insert("default".into(), json!(d));
    }
    let keys: Vec<&str> = [
        (c.primary, "primary"),
        (c.auto_increment, "auto_increment"),
        (c.unique, "unique"),
        (c.indexed && !c.unique && !c.primary, "index"),
        (c.unsigned, "unsigned"),
    ]
    .into_iter()
    .filter_map(|(on, k)| on.then_some(k))
    .collect();
    if !keys.is_empty() {
        o.insert("keys".into(), json!(keys));
    }
    if let Some(t) = &c.fk_table {
        o.insert(
            "references".into(),
            json!(format!("{t}.{}", c.fk_column.as_deref().unwrap_or("id"))),
        );
    }
    if c.confidence == Confidence::Low {
        o.insert("confidence".into(), json!("low"));
    }
    if let Some(s) = &c.source {
        o.insert("source".into(), json!(s));
    }
    if let Some(d) = &c.drift {
        o.insert("drift".into(), json!(d));
    }
    if detail == Detail::Full {
        o.insert("created_in".into(), json!(c.created_in));
        o.insert("last_changed_in".into(), json!(c.last_changed_in));
    }
    v
}

/// `get_schema`: all tables, or one table's columns.
pub fn describe(conn: &Connection, table: Option<&str>, detail: Detail) -> Result<Value> {
    let all = tables(conn)?;
    let Some(name) = table.map(str::trim).filter(|t| !t.is_empty()) else {
        let mut list = Vec::new();
        for (name, dropped, _, _, _, _, (source, drift)) in all.iter().filter(|t| !t.1) {
            let _ = dropped;
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM db_columns WHERE table_name = ?1",
                [name],
                |r| r.get(0),
            )?;
            let models: Vec<String> = models_for(conn, name)?
                .iter()
                .filter_map(|m| m["fqn"].as_str().map(str::to_owned))
                .collect();
            let mut row = json!({ "table": name, "columns": count, "models": models });
            if let Some(s) = source {
                row["source"] = json!(s);
            }
            if let Some(d) = drift {
                row["drift"] = json!(d);
            }
            list.push(row);
        }
        if list.is_empty() {
            return Ok(
                json!({ "found": false, "query": null, "suggestions": [], "hint": "No tables: no migrations or schema dump were found." }),
            );
        }
        return Ok(json!({ "found": true, "total": list.len(), "tables": list }));
    };

    let live = all.iter().find(|t| t.0 == name && !t.1);
    let Some((tname, _, renamed_from, created_in, changed_in, confidence, (source, drift))) = live
    else {
        let mut hint = String::new();
        if let Some(t) = all.iter().find(|t| t.0 == name && t.1) {
            hint = format!(
                "Table {name} was dropped ({}).",
                t.4.as_deref().unwrap_or("?")
            );
        }
        if let Some(t) = all.iter().find(|t| t.2.as_deref() == Some(name) && !t.1) {
            hint = format!(
                "Table {name} was renamed to {} ({}).",
                t.0,
                t.4.as_deref().unwrap_or("?")
            );
        }
        let mut suggestions: Vec<(f64, &str)> = all
            .iter()
            .filter(|t| !t.1)
            .map(|t| {
                (
                    strsim::jaro_winkler(&name.to_lowercase(), &t.0.to_lowercase()),
                    t.0.as_str(),
                )
            })
            .filter(|(s, _)| *s >= 0.8)
            .collect();
        suggestions.sort_by(|a, b| b.0.total_cmp(&a.0));
        suggestions.truncate(5);
        if hint.is_empty() {
            hint = match suggestions.first() {
                Some((_, s)) => format!("Did you mean {s}?"),
                None => "No such table in migrations or the schema dump.".into(),
            };
        }
        let suggestions: Vec<Value> = suggestions
            .iter()
            .map(|(score, t)| json!({ "table": t, "score": (score * 100.0).round() / 100.0 }))
            .collect();
        return Ok(
            json!({ "found": false, "query": name, "suggestions": suggestions, "hint": hint }),
        );
    };

    let columns: Vec<Value> = load_columns(conn, tname)?
        .iter()
        .map(|c| column_json(c, detail))
        .collect();
    let mut v = json!({
        "found": true,
        "table": tname,
        "columns": columns,
        "models": models_for(conn, tname)?,
        "created_in": created_in,
        "last_changed_in": changed_in,
    });
    let o = v.as_object_mut().expect("object");
    if let Some(r) = renamed_from {
        o.insert("renamed_from".into(), json!(r));
    }
    if confidence == "low" {
        o.insert("confidence".into(), json!("low"));
    }
    if let Some(s) = source {
        o.insert("source".into(), json!(s));
    }
    if let Some(d) = drift {
        o.insert("drift".into(), json!(d));
    }
    let indexes: Option<String> = conn
        .query_row(
            "SELECT indexes_json FROM db_tables WHERE name = ?1 AND dropped = 0",
            [tname],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    if let Some(list) = indexes.and_then(|j| serde_json::from_str::<Vec<TableIndex>>(&j).ok()) {
        let list: Vec<Value> = list
            .iter()
            .map(|i| {
                json!({
                    "name": i.name,
                    "columns": i.columns,
                    "unique": i.kind == "unique" || i.kind == "primary",
                    "kind": i.kind,
                    "defined_in": i.defined_in,
                })
            })
            .collect();
        o.insert("indexes".into(), json!(list));
    }
    Ok(v)
}
