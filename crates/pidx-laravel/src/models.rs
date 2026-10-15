//! Eloquent models: classes extending Model / Auth\User / Pivot (transitively,
//! within the project). Properties, casts, relations, accessors and scopes are
//! read from literals in the source; anything else is noted, not guessed.

use std::collections::BTreeMap;
use std::path::Path;

use pidx_core::Result;
use pidx_core::symbols::{self, ClassRef};
use pidx_php::Scope;
use pidx_php::ast::{find_class, named_children, start_line, text};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use tree_sitter::Node;

use crate::inflector;
use crate::literal::{self, Lit, snake};

pub const ELOQUENT_MODEL: &str = "Illuminate\\Database\\Eloquent\\Model";
pub const AUTH_USER: &str = "Illuminate\\Foundation\\Auth\\User";
pub const PIVOT: &str = "Illuminate\\Database\\Eloquent\\Relations\\Pivot";
pub const MORPH_PIVOT: &str = "Illuminate\\Database\\Eloquent\\Relations\\MorphPivot";
const ATTRIBUTE_CAST: &str = "Illuminate\\Database\\Eloquent\\Casts\\Attribute";
const SCOPE_ATTRIBUTE: &str = "Illuminate\\Database\\Eloquent\\Attributes\\Scope";
const SOFT_DELETES: &str = "Illuminate\\Database\\Eloquent\\SoftDeletes";
const RELATIONS_NS: &str = "Illuminate\\Database\\Eloquent\\Relations\\";

pub const RELATION_TYPES: &[&str] = &[
    "hasOne",
    "hasMany",
    "belongsTo",
    "belongsToMany",
    "hasOneThrough",
    "hasManyThrough",
    "morphTo",
    "morphOne",
    "morphMany",
    "morphToMany",
    "morphedByMany",
];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Relation {
    pub method: String,
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub related: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreign_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pivot_table: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub through: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub morph_name: Option<String>,
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unresolved_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accessor {
    pub attribute: String,
    pub method: String,
    pub style: String,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeDef {
    pub name: String,
    pub method: String,
    pub line: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub fqn: String,
    /// `model`, `pivot` or `user`.
    pub kind: String,
    pub parent_fqn: Option<String>,
    pub table: String,
    pub table_inferred: bool,
    pub connection: Option<String>,
    pub primary_key: Option<String>,
    pub fillable: Option<Vec<String>>,
    pub guarded: Option<Vec<String>>,
    pub hidden: Option<Vec<String>>,
    pub casts: Option<Vec<(String, String)>>,
    pub appends: Option<Vec<String>>,
    pub traits: Vec<String>,
    pub relations: Vec<Relation>,
    pub accessors: Vec<Accessor>,
    pub scopes: Vec<ScopeDef>,
    pub file: String,
    pub line: u32,
    pub notes: Vec<String>,
}

impl ModelInfo {
    pub fn uses_trait(&self, short: &str) -> bool {
        self.traits
            .iter()
            .any(|t| t.rsplit('\\').next() == Some(short))
    }
}

/// Properties declared on one class (before inheritance is applied).
#[derive(Debug, Clone, Default)]
struct Declared {
    table: Option<Lit>,
    connection: Option<Lit>,
    primary_key: Option<Lit>,
    fillable: Option<Lit>,
    guarded: Option<Lit>,
    hidden: Option<Lit>,
    casts: Option<Lit>,
    appends: Option<Lit>,
    casts_method: Option<Lit>,
    attrs: ClassAttrs,
}

/// Laravel 13+ class attributes: `#[Fillable([...])]`, `#[Hidden]`, `#[Table('x')]`, ...
#[derive(Debug, Clone, Default)]
struct ClassAttrs {
    fillable: Option<Vec<String>>,
    hidden: Option<Vec<String>>,
    appends: Option<Vec<String>>,
    guarded: Option<Vec<String>>,
    unguarded: bool,
    table: Option<String>,
    key: Option<String>,
    connection: Option<String>,
}

const ATTRS_NS: &str = "Illuminate\\Database\\Eloquent\\Attributes\\";

fn class_attrs(node: Node<'_>, src: &[u8], scope: &Scope) -> ClassAttrs {
    let mut out = ClassAttrs::default();
    let Some(list) = node.child_by_field_name("attributes") else {
        return out;
    };
    let mut stack = vec![list];
    while let Some(n) = stack.pop() {
        if n.kind() != "attribute" {
            stack.extend(named_children(n));
            continue;
        }
        let parts = named_children(n);
        let Some(name) = parts
            .iter()
            .find(|c| matches!(c.kind(), "name" | "qualified_name"))
            .and_then(|c| scope.resolve_class(text(*c, src)))
        else {
            continue;
        };
        let Some(short) = name.strip_prefix(ATTRS_NS) else {
            continue;
        };
        // (named?, value) for each argument
        let args: Vec<(Option<String>, Lit)> = parts
            .iter()
            .find(|c| c.kind() == "arguments")
            .map(|a| {
                named_children(*a)
                    .into_iter()
                    .filter(|x| x.kind() == "argument")
                    .filter_map(|x| {
                        let arg_name = x
                            .child_by_field_name("name")
                            .map(|nn| text(nn, src).to_owned());
                        let value = named_children(x).into_iter().last()?;
                        Some((arg_name, literal::eval(value, src, scope)))
                    })
                    .collect()
            })
            .unwrap_or_default();
        // array|string ...$columns
        let columns = || -> Vec<String> {
            args.iter()
                .flat_map(|(_, v)| v.strings().unwrap_or_default())
                .collect()
        };
        let named = |key: &str, pos: usize| -> Option<String> {
            args.iter()
                .find(|(k, _)| k.as_deref() == Some(key))
                .or_else(|| args.iter().filter(|(k, _)| k.is_none()).nth(pos))
                .and_then(|(_, v)| v.as_str().map(str::to_owned))
        };
        match short {
            "Fillable" => out.fillable = Some(columns()),
            "Hidden" => out.hidden = Some(columns()),
            "Appends" => out.appends = Some(columns()),
            "Guarded" => out.guarded = Some(columns()),
            "Unguarded" => out.unguarded = true,
            "Table" => {
                out.table = named("name", 0);
                out.key = named("key", 1);
            }
            "Connection" => out.connection = named("name", 0),
            _ => {}
        }
    }
    out
}

/// Property list + attribute list, in that order, without duplicates.
fn merged(property: Option<Vec<String>>, attribute: Option<Vec<String>>) -> Option<Vec<String>> {
    if property.is_none() && attribute.is_none() {
        return None;
    }
    let mut out = property.unwrap_or_default();
    for c in attribute.unwrap_or_default() {
        if !out.contains(&c) {
            out.push(c);
        }
    }
    Some(out)
}

pub fn extract(conn: &Connection, root: &Path) -> Result<Vec<ModelInfo>> {
    let classes = symbols::descendants(conn, &[ELOQUENT_MODEL, AUTH_USER, PIVOT, MORPH_PIVOT])?;
    let mut declared: BTreeMap<String, Declared> = BTreeMap::new();
    let mut models = Vec::new();

    for class in &classes {
        let record = symbols::by_fqn(conn, &class.fqn)?.into_iter().next();
        let parent = record
            .as_ref()
            .and_then(|r| r.meta.extends.first().cloned());
        let mut info = ModelInfo {
            fqn: class.fqn.clone(),
            kind: model_kind(conn, &class.fqn)?,
            parent_fqn: parent,
            traits: inherited_traits(conn, &class.fqn)?,
            file: class.file.clone(),
            line: class.line,
            ..ModelInfo::default()
        };
        let mut decl = Declared::default();
        match std::fs::read(root.join(&class.file)) {
            Ok(src) => read_class(&src, class, &mut info, &mut decl, conn)?,
            Err(e) => info
                .notes
                .push(format!("could not read {}: {e}", class.file)),
        }
        declared.insert(class.fqn.clone(), decl);
        models.push(info);
    }

    // Apply PHP property inheritance through project parent classes.
    let parents: BTreeMap<String, Option<String>> = models
        .iter()
        .map(|m| (m.fqn.clone(), m.parent_fqn.clone()))
        .collect();
    for m in &mut models {
        let chain = ancestry(&m.fqn, &parents);
        let pick = |get: &dyn Fn(&Declared) -> Option<Lit>| -> Option<Lit> {
            chain.iter().find_map(|c| declared.get(c).and_then(get))
        };
        let table = pick(&|d| d.table.clone());
        let list = |lit: Option<Lit>, what: &str, notes: &mut Vec<String>| -> Option<Vec<String>> {
            let lit = lit?;
            let strings = lit.strings();
            if strings.is_none() {
                notes.push(format!("${what} is not a literal list: {}", lit.display()));
            }
            strings
        };
        let mut notes = std::mem::take(&mut m.notes);
        // Laravel 13 attributes: Fillable/Hidden/Appends merge with the property;
        // Guarded/Unguarded apply only when $guarded is not declared.
        let attr = |get: &dyn Fn(&ClassAttrs) -> Option<Vec<String>>| -> Option<Vec<String>> {
            chain
                .iter()
                .find_map(|c| declared.get(c).and_then(|d| get(&d.attrs)))
        };
        m.fillable = merged(
            list(pick(&|d| d.fillable.clone()), "fillable", &mut notes),
            attr(&|a| a.fillable.clone()),
        );
        m.hidden = merged(
            list(pick(&|d| d.hidden.clone()), "hidden", &mut notes),
            attr(&|a| a.hidden.clone()),
        );
        m.appends = merged(
            list(pick(&|d| d.appends.clone()), "appends", &mut notes),
            attr(&|a| a.appends.clone()),
        );
        m.guarded = match list(pick(&|d| d.guarded.clone()), "guarded", &mut notes) {
            Some(g) => Some(g),
            None if chain
                .iter()
                .any(|c| declared.get(c).is_some_and(|d| d.attrs.unguarded)) =>
            {
                Some(Vec::new())
            }
            None => attr(&|a| a.guarded.clone()),
        };
        let attr_str = |get: &dyn Fn(&ClassAttrs) -> Option<String>| -> Option<String> {
            chain
                .iter()
                .find_map(|c| declared.get(c).and_then(|d| get(&d.attrs)))
        };
        m.connection = pick(&|d| d.connection.clone())
            .and_then(|l| l.as_str().map(str::to_owned))
            .or_else(|| attr_str(&|a| a.connection.clone()));
        m.primary_key = pick(&|d| d.primary_key.clone())
            .and_then(|l| l.as_str().map(str::to_owned))
            .or_else(|| attr_str(&|a| a.key.clone()));
        let table = table.or_else(|| attr_str(&|a| a.table.clone()).map(Lit::Str));

        // $casts and casts() are merged; the method wins on conflicts.
        let mut casts: Vec<(String, String)> = Vec::new();
        for lit in [
            pick(&|d| d.casts.clone()),
            pick(&|d| d.casts_method.clone()),
        ]
        .into_iter()
        .flatten()
        {
            match lit {
                Lit::Arr(items) => {
                    for (k, v) in items {
                        if let Some(k) = k {
                            casts.retain(|(x, _)| x != &k);
                            casts.push((k, v.plain()));
                        }
                    }
                }
                other => notes.push(format!(
                    "casts are not a literal array: {}",
                    other.display()
                )),
            }
        }
        m.casts = (!casts.is_empty()).then_some(casts);

        match table.as_ref().and_then(Lit::as_str) {
            Some(t) => m.table = t.to_owned(),
            None => {
                if let Some(t) = &table {
                    notes.push(format!("$table is not a literal: {}", t.display()));
                }
                let base = m.fqn.rsplit('\\').next().unwrap_or(&m.fqn);
                m.table = if m.kind == "pivot" {
                    snake(&inflector::singular(base))
                } else {
                    inflector::plural(&snake(base))
                };
                m.table_inferred = true;
            }
        }
        if m.traits.iter().any(|t| t == SOFT_DELETES) {
            notes.push("SoftDeletes: expects a nullable deleted_at column".into());
        }
        m.notes = notes;
    }
    Ok(models)
}

/// The class and its project ancestors, nearest first.
fn ancestry(fqn: &str, parents: &BTreeMap<String, Option<String>>) -> Vec<String> {
    let mut chain = vec![fqn.to_owned()];
    let mut current = fqn.to_owned();
    while let Some(Some(parent)) = parents.get(&current) {
        if chain.contains(parent) {
            break;
        }
        chain.push(parent.clone());
        current = parent.clone();
    }
    chain
}

fn model_kind(conn: &Connection, fqn: &str) -> Result<String> {
    let mut current = fqn.to_owned();
    for _ in 0..32 {
        let Some(parent) = symbols::by_fqn(conn, &current)?
            .into_iter()
            .next()
            .and_then(|r| r.meta.extends.into_iter().next())
        else {
            break;
        };
        match parent.as_str() {
            PIVOT | MORPH_PIVOT => return Ok("pivot".into()),
            AUTH_USER => return Ok("user".into()),
            ELOQUENT_MODEL => return Ok("model".into()),
            _ => current = parent,
        }
    }
    Ok("model".into())
}

/// Traits of the class and its project ancestors (and traits used by traits).
fn inherited_traits(conn: &Connection, fqn: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut queue = vec![fqn.to_owned()];
    let mut seen = Vec::new();
    while let Some(c) = queue.pop() {
        if seen.contains(&c) {
            continue;
        }
        seen.push(c.clone());
        if let Some(r) = symbols::by_fqn(conn, &c)?.into_iter().next() {
            for t in &r.meta.traits {
                if !out.contains(t) {
                    out.push(t.clone());
                }
                queue.push(t.clone());
            }
            queue.extend(r.meta.extends.iter().cloned());
        }
    }
    Ok(out)
}

fn read_class(
    src: &[u8],
    class: &ClassRef,
    info: &mut ModelInfo,
    decl: &mut Declared,
    conn: &Connection,
) -> Result<()> {
    let Some(tree) = pidx_php::parse::parse(src) else {
        return Ok(());
    };
    let Some((node, scope)) = find_class(tree.root_node(), src, &class.fqn) else {
        info.notes
            .push("class declaration not found in file".into());
        return Ok(());
    };
    decl.attrs = class_attrs(node, src, &scope);
    let Some(body) = node.child_by_field_name("body") else {
        return Ok(());
    };
    let methods: BTreeMap<String, symbols::SymbolRecord> = symbols::members(conn, &class.fqn)?
        .into_iter()
        .filter(|m| m.kind == symbols::SymbolKind::Method)
        .map(|m| (m.name.to_lowercase(), m))
        .collect();

    for member in named_children(body) {
        match member.kind() {
            "property_declaration" => {
                for el in named_children(member) {
                    if el.kind() != "property_element" {
                        continue;
                    }
                    let name = el
                        .child_by_field_name("name")
                        .map(|n| text(n, src))
                        .unwrap_or("");
                    let Some(value) = el.child_by_field_name("default_value") else {
                        continue;
                    };
                    let lit = literal::eval(value, src, &scope);
                    match name {
                        "$table" => decl.table = Some(lit),
                        "$connection" => decl.connection = Some(lit),
                        "$primaryKey" => decl.primary_key = Some(lit),
                        "$fillable" => decl.fillable = Some(lit),
                        "$guarded" => decl.guarded = Some(lit),
                        "$hidden" => decl.hidden = Some(lit),
                        "$casts" => decl.casts = Some(lit),
                        "$appends" => decl.appends = Some(lit),
                        _ => {}
                    }
                }
            }
            "method_declaration" => {
                let Some(name) = member.child_by_field_name("name").map(|n| text(n, src)) else {
                    continue;
                };
                let line = start_line(member);
                let record = methods.get(&name.to_lowercase());
                let body = member.child_by_field_name("body");
                let returned = body.and_then(|b| return_expression(b));

                if name == "casts" {
                    decl.casts_method = returned.map(|r| literal::eval(r, src, &scope));
                    continue;
                }
                if let Some(rel) = returned.and_then(|r| relation(r, src, &scope, name, line)) {
                    info.relations.push(rel);
                } else if let Some(r) = record
                    && let Some(rel_type) = r
                        .meta
                        .return_fqns
                        .iter()
                        .find_map(|f| f.strip_prefix(RELATIONS_NS))
                {
                    info.relations.push(Relation {
                        method: name.to_owned(),
                        r#type: lcfirst(rel_type),
                        line,
                        unresolved_reason: Some(
                            "relation is not a direct `return $this->relation(...)`; type from return type only".into(),
                        ),
                        ..Relation::default()
                    });
                }

                if let Some(attr) = legacy_accessor(name, "get") {
                    info.accessors.push(Accessor {
                        attribute: attr,
                        method: name.into(),
                        style: "legacy_get".into(),
                        line,
                    });
                } else if let Some(attr) = legacy_accessor(name, "set") {
                    info.accessors.push(Accessor {
                        attribute: attr,
                        method: name.into(),
                        style: "legacy_set".into(),
                        line,
                    });
                } else if record
                    .is_some_and(|r| r.meta.return_fqns.iter().any(|f| f == ATTRIBUTE_CAST))
                    || body.is_some_and(|b| {
                        let t = text(b, src);
                        t.contains("Attribute::make(") || t.contains("new Attribute(")
                    })
                {
                    info.accessors.push(Accessor {
                        attribute: snake(name),
                        method: name.into(),
                        style: "attribute_make".into(),
                        line,
                    });
                }

                if let Some(scope_name) = name.strip_prefix("scope").filter(|s| !s.is_empty()) {
                    info.scopes.push(ScopeDef {
                        name: lcfirst(scope_name),
                        method: name.into(),
                        line,
                    });
                } else if record
                    .is_some_and(|r| r.meta.attributes.iter().any(|a| a == SCOPE_ATTRIBUTE))
                {
                    info.scopes.push(ScopeDef {
                        name: name.into(),
                        method: name.into(),
                        line,
                    });
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn lcfirst(s: &str) -> String {
    let mut chars = s.chars();
    chars
        .next()
        .map(|c| c.to_lowercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

/// `getTotalFormattedAttribute` → `total_formatted`.
fn legacy_accessor(method: &str, prefix: &str) -> Option<String> {
    let middle = method.strip_prefix(prefix)?.strip_suffix("Attribute")?;
    (!middle.is_empty() && middle.starts_with(char::is_uppercase)).then(|| snake(middle))
}

/// The expression of the first top-level `return` in a method body.
pub fn return_expression(body: Node<'_>) -> Option<Node<'_>> {
    named_children(body)
        .into_iter()
        .find(|s| s.kind() == "return_statement")
        .and_then(|r| named_children(r).into_iter().next())
}

/// `return $this->hasMany(Item::class, 'order_id')->latest();`
fn relation(
    expr: Node<'_>,
    src: &[u8],
    scope: &Scope,
    method: &str,
    line: u32,
) -> Option<Relation> {
    let mut node = expr;
    loop {
        if node.kind() != "member_call_expression" {
            return None;
        }
        let object = node.child_by_field_name("object")?;
        let name = text(node.child_by_field_name("name")?, src);
        if object.kind() == "variable_name"
            && text(object, src) == "$this"
            && RELATION_TYPES.contains(&name)
        {
            let args: Vec<Lit> = node
                .child_by_field_name("arguments")
                .map(|a| {
                    named_children(a)
                        .into_iter()
                        .filter(|x| x.kind() == "argument")
                        .filter_map(|x| named_children(x).into_iter().last())
                        .map(|v| literal::eval(v, src, scope))
                        .collect()
                })
                .unwrap_or_default();
            let s = |i: usize| args.get(i).and_then(Lit::as_str).map(str::to_owned);
            let mut rel = Relation {
                method: method.to_owned(),
                r#type: name.to_owned(),
                line,
                ..Relation::default()
            };
            if name != "morphTo" {
                rel.related = s(0);
                if rel.related.is_none() {
                    rel.unresolved_reason = Some(format!(
                        "related model is not a literal: {}",
                        args.first().map_or("missing".into(), Lit::display)
                    ));
                }
            }
            match name {
                "hasOne" | "hasMany" | "belongsTo" => {
                    rel.foreign_key = s(1);
                    rel.owner_key = s(2);
                }
                "belongsToMany" => {
                    rel.pivot_table = s(1);
                    rel.foreign_key = s(2);
                    rel.owner_key = s(3);
                }
                "hasOneThrough" | "hasManyThrough" => {
                    rel.through = s(1);
                    rel.foreign_key = s(2);
                    rel.owner_key = s(3);
                }
                "morphTo" => {
                    rel.morph_name = s(0).or_else(|| Some(snake(method)));
                }
                "morphOne" | "morphMany" => rel.morph_name = s(1),
                "morphToMany" | "morphedByMany" => {
                    rel.morph_name = s(1);
                    rel.pivot_table = s(2);
                }
                _ => {}
            }
            return Some(rel);
        }
        node = object;
    }
}

pub fn store(conn: &Connection, models: &[ModelInfo]) -> Result<()> {
    for sql in [
        "DELETE FROM models",
        "DELETE FROM model_relations",
        "DELETE FROM model_accessors",
        "DELETE FROM model_scopes",
    ] {
        conn.execute(sql, [])?;
    }
    let json = |v: &Option<Vec<String>>| {
        v.as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_default())
    };
    for m in models {
        conn.execute(
            "INSERT INTO models (fqn, kind, parent_fqn, table_name, table_inferred, connection,
                                 primary_key, fillable_json, guarded_json, hidden_json, casts_json,
                                 appends_json, traits_json, file, line, notes_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                m.fqn,
                m.kind,
                m.parent_fqn,
                m.table,
                m.table_inferred,
                m.connection,
                m.primary_key,
                json(&m.fillable),
                json(&m.guarded),
                json(&m.hidden),
                m.casts
                    .as_ref()
                    .map(|c| serde_json::to_string(c).unwrap_or_default()),
                json(&m.appends),
                serde_json::to_string(&m.traits)?,
                m.file,
                m.line,
                (!m.notes.is_empty()).then(|| serde_json::to_string(&m.notes).unwrap_or_default()),
            ],
        )?;
        for r in &m.relations {
            conn.execute(
                "INSERT INTO model_relations (model_fqn, method, type, related_fqn, foreign_key,
                     owner_key, pivot_table, through_fqn, morph_name, line, unresolved_reason)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    m.fqn,
                    r.method,
                    r.r#type,
                    r.related,
                    r.foreign_key,
                    r.owner_key,
                    r.pivot_table,
                    r.through,
                    r.morph_name,
                    r.line,
                    r.unresolved_reason
                ],
            )?;
        }
        for a in &m.accessors {
            conn.execute(
                "INSERT INTO model_accessors (model_fqn, attribute, method, style, line) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![m.fqn, a.attribute, a.method, a.style, a.line],
            )?;
        }
        for s in &m.scopes {
            conn.execute(
                "INSERT INTO model_scopes (model_fqn, scope_name, method, line) VALUES (?1, ?2, ?3, ?4)",
                params![m.fqn, s.name, s.method, s.line],
            )?;
        }
    }
    Ok(())
}

/// All stored models with their relations, accessors and scopes.
pub fn load(conn: &Connection) -> Result<Vec<ModelInfo>> {
    let parse = |s: Option<String>| -> Option<Vec<String>> {
        s.and_then(|j| serde_json::from_str(&j).ok())
    };
    let mut stmt = conn.prepare(
        "SELECT fqn, kind, parent_fqn, table_name, table_inferred, connection, primary_key,
                fillable_json, guarded_json, hidden_json, casts_json, appends_json, traits_json,
                file, line, notes_json FROM models ORDER BY fqn",
    )?;
    let mut models: Vec<ModelInfo> = stmt
        .query_map([], |r| {
            Ok(ModelInfo {
                fqn: r.get(0)?,
                kind: r.get(1)?,
                parent_fqn: r.get(2)?,
                table: r.get(3)?,
                table_inferred: r.get(4)?,
                connection: r.get(5)?,
                primary_key: r.get(6)?,
                fillable: parse(r.get(7)?),
                guarded: parse(r.get(8)?),
                hidden: parse(r.get(9)?),
                casts: r
                    .get::<_, Option<String>>(10)?
                    .and_then(|j| serde_json::from_str(&j).ok()),
                appends: parse(r.get(11)?),
                traits: parse(r.get(12)?).unwrap_or_default(),
                file: r.get(13)?,
                line: r.get(14)?,
                notes: parse(r.get(15)?).unwrap_or_default(),
                ..ModelInfo::default()
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    for m in &mut models {
        let mut s = conn.prepare_cached(
            "SELECT method, type, related_fqn, foreign_key, owner_key, pivot_table, through_fqn,
                    morph_name, line, unresolved_reason FROM model_relations WHERE model_fqn = ?1 ORDER BY line",
        )?;
        m.relations = s
            .query_map([&m.fqn], |r| {
                Ok(Relation {
                    method: r.get(0)?,
                    r#type: r.get(1)?,
                    related: r.get(2)?,
                    foreign_key: r.get(3)?,
                    owner_key: r.get(4)?,
                    pivot_table: r.get(5)?,
                    through: r.get(6)?,
                    morph_name: r.get(7)?,
                    line: r.get(8)?,
                    unresolved_reason: r.get(9)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut s = conn.prepare_cached(
            "SELECT attribute, method, style, line FROM model_accessors WHERE model_fqn = ?1 ORDER BY line",
        )?;
        m.accessors = s
            .query_map([&m.fqn], |r| {
                Ok(Accessor {
                    attribute: r.get(0)?,
                    method: r.get(1)?,
                    style: r.get(2)?,
                    line: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut s = conn.prepare_cached(
            "SELECT scope_name, method, line FROM model_scopes WHERE model_fqn = ?1 ORDER BY line",
        )?;
        m.scopes = s
            .query_map([&m.fqn], |r| {
                Ok(ScopeDef {
                    name: r.get(0)?,
                    method: r.get(1)?,
                    line: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessor_names() {
        assert_eq!(
            legacy_accessor("getTotalFormattedAttribute", "get").as_deref(),
            Some("total_formatted")
        );
        assert_eq!(legacy_accessor("getAttribute", "get"), None);
        assert_eq!(
            legacy_accessor("setStatusAttribute", "set").as_deref(),
            Some("status")
        );
        assert_eq!(legacy_accessor("getter", "get"), None);
        assert_eq!(lcfirst("Paid"), "paid");
    }
}
