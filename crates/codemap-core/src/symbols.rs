//! Code symbols: the stored model, inserts, ranked search and "did you mean"
//! suggestions. Extractors (e.g. `codemap-php`) fill [`NewSymbol`]s.

use std::fmt;

use rusqlite::{Connection, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Result;
use crate::envelope::Detail;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Class,
    Interface,
    Trait,
    Enum,
    Method,
    Function,
    Property,
    Const,
    EnumCase,
    /// A Vue component (`.vue` file or registered options object).
    Component,
    /// A JS/TS function named `use*`.
    Composable,
    /// A Pinia store or Vuex module.
    Store,
}

impl SymbolKind {
    pub const ALL: [SymbolKind; 12] = [
        Self::Class,
        Self::Interface,
        Self::Trait,
        Self::Enum,
        Self::Method,
        Self::Function,
        Self::Property,
        Self::Const,
        Self::EnumCase,
        Self::Component,
        Self::Composable,
        Self::Store,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Class => "class",
            Self::Interface => "interface",
            Self::Trait => "trait",
            Self::Enum => "enum",
            Self::Method => "method",
            Self::Function => "function",
            Self::Property => "property",
            Self::Const => "const",
            Self::EnumCase => "enum_case",
            Self::Component => "component",
            Self::Composable => "composable",
            Self::Store => "store",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    pub fn is_class_like(self) -> bool {
        matches!(
            self,
            Self::Class | Self::Interface | Self::Trait | Self::Enum
        )
    }

    /// Ordering among equally scored results: types first, then functions, then members.
    fn rank(self) -> u8 {
        match self {
            k if k.is_class_like() || k == Self::Component => 0,
            Self::Function | Self::Composable | Self::Store => 1,
            Self::Method => 2,
            _ => 3,
        }
    }
}

impl fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    /// Class names in the type, resolved to FQNs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub type_fqns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub variadic: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub by_ref: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub promoted: bool,
}

/// Kind-specific details, stored as JSON. Empty fields are omitted.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SymbolMeta {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extends: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub implements: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub traits: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attributes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backed_type: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<Param>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_type: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub return_fqns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    /// Literal default of a property or value of a const / enum case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub readonly: bool,
}

impl SymbolMeta {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NewSymbol {
    pub kind: SymbolKind,
    pub name: String,
    pub fqn: String,
    pub parent_fqn: Option<String>,
    pub signature: String,
    pub visibility: Option<String>,
    pub is_static: bool,
    pub is_abstract: bool,
    pub is_final: bool,
    pub start_line: u32,
    pub end_line: u32,
    pub doc_summary: Option<String>,
    pub meta: SymbolMeta,
}

/// Per-row FTS maintenance (same SQL as migration 0002).
const FTS_INSERT_TRIGGER: &str =
    "CREATE TRIGGER IF NOT EXISTS symbols_fts_insert AFTER INSERT ON symbols BEGIN
    INSERT INTO symbols_fts (rowid, name, fqn, doc_summary)
    VALUES (new.id, new.name, new.fqn, new.doc_summary);
END;";

/// Batches at least this big skip per-row FTS updates and rebuild the index once.
pub const BULK_FTS_THRESHOLD: usize = 2_000;

/// Starts a bulk insert of `symbols`: per-row trigram indexing is slow, so the
/// insert trigger is dropped and [`bulk_insert_end`] rebuilds the FTS index once.
/// Run both inside the same transaction.
pub fn bulk_insert_begin(conn: &Connection) -> Result<()> {
    conn.execute_batch("DROP TRIGGER IF EXISTS symbols_fts_insert;")?;
    Ok(())
}

pub fn bulk_insert_end(conn: &Connection) -> Result<()> {
    conn.execute_batch(&format!(
        "INSERT INTO symbols_fts (symbols_fts) VALUES ('rebuild');\n{FTS_INSERT_TRIGGER}"
    ))?;
    Ok(())
}

/// Inserts one symbol and its extends/implements/trait references; returns its id.
pub fn insert(conn: &Connection, file_id: i64, s: &NewSymbol) -> Result<i64> {
    let meta_json = if s.meta.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&s.meta)?)
    };
    conn.prepare_cached(
        "INSERT INTO symbols (file_id, kind, name, fqn, parent_fqn, signature, visibility,
                              is_static, is_abstract, is_final, start_line, end_line,
                              doc_summary, meta_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
    )?
    .execute(params![
        file_id,
        s.kind.as_str(),
        s.name,
        s.fqn,
        s.parent_fqn,
        s.signature,
        s.visibility,
        s.is_static,
        s.is_abstract,
        s.is_final,
        s.start_line,
        s.end_line,
        s.doc_summary,
        meta_json,
    ])?;
    let id = conn.last_insert_rowid();
    let refs = [
        ("extends", &s.meta.extends),
        ("implements", &s.meta.implements),
        ("uses_trait", &s.meta.traits),
    ];
    let mut stmt = conn.prepare_cached(
        "INSERT INTO symbol_refs (symbol_id, relation, target_fqn) VALUES (?1, ?2, ?3)",
    )?;
    for (relation, targets) in refs {
        for target in targets {
            stmt.execute(params![id, relation, target])?;
        }
    }
    Ok(id)
}

/// A stored symbol with its file location.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SymbolRecord {
    pub kind: SymbolKind,
    pub name: String,
    pub fqn: String,
    pub parent_fqn: Option<String>,
    pub signature: String,
    pub visibility: Option<String>,
    pub is_static: bool,
    pub is_abstract: bool,
    pub is_final: bool,
    pub file: String,
    pub line: u32,
    pub end_line: u32,
    pub doc_summary: Option<String>,
    pub meta: SymbolMeta,
}

const RECORD_COLUMNS: &str = "s.kind, s.name, s.fqn, s.parent_fqn, s.signature, s.visibility,
     s.is_static, s.is_abstract, s.is_final, f.path, s.start_line, s.end_line,
     s.doc_summary, s.meta_json";

impl SymbolRecord {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let kind: String = row.get(0)?;
        let meta_json: Option<String> = row.get(13)?;
        Ok(Self {
            kind: SymbolKind::parse(&kind).unwrap_or(SymbolKind::Class),
            name: row.get(1)?,
            fqn: row.get(2)?,
            parent_fqn: row.get(3)?,
            signature: row.get(4)?,
            visibility: row.get(5)?,
            is_static: row.get(6)?,
            is_abstract: row.get(7)?,
            is_final: row.get(8)?,
            file: row.get(9)?,
            line: row.get(10)?,
            end_line: row.get(11)?,
            doc_summary: row.get(12)?,
            meta: meta_json
                .and_then(|j| serde_json::from_str(&j).ok())
                .unwrap_or_default(),
        })
    }

    /// JSON for a response. `brief` keeps only what is needed to pick and locate a symbol.
    pub fn to_json(&self, detail: Detail, score: Option<f64>) -> Value {
        let mut v = json!({
            "name": self.name,
            "fqn": self.fqn,
            "kind": self.kind,
            "signature": self.signature,
            "file": self.file,
            "line": self.line,
        });
        let obj = v.as_object_mut().expect("object");
        if let Some(doc) = &self.doc_summary {
            obj.insert("doc_summary".into(), json!(doc));
        }
        if detail == Detail::Full {
            obj.insert("end_line".into(), json!(self.end_line));
            if let Some(p) = &self.parent_fqn {
                obj.insert("parent_fqn".into(), json!(p));
            }
            if let Some(vis) = &self.visibility {
                obj.insert("visibility".into(), json!(vis));
            }
            let flags: Vec<&str> = [
                (self.is_static, "static"),
                (self.is_abstract, "abstract"),
                (self.is_final, "final"),
            ]
            .into_iter()
            .filter_map(|(on, name)| on.then_some(name))
            .collect();
            if !flags.is_empty() {
                obj.insert("flags".into(), json!(flags));
            }
            if !self.meta.is_empty() {
                obj.insert("meta".into(), json!(self.meta));
            }
            if let Some(score) = score {
                obj.insert("score".into(), json!(round2(score)));
            }
        }
        v
    }
}

/// Every symbol, sorted by file, line and fqn. For snapshots and debugging.
pub fn all(conn: &Connection) -> Result<Vec<SymbolRecord>> {
    let sql = format!(
        "SELECT {RECORD_COLUMNS} FROM symbols s JOIN files f ON f.id = s.file_id
         ORDER BY f.path, s.start_line, s.fqn"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], SymbolRecord::from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Symbols whose FQN matches exactly (case-insensitive, leading `\\` ignored).
pub fn by_fqn(conn: &Connection, fqn: &str) -> Result<Vec<SymbolRecord>> {
    let sql = format!(
        "SELECT {RECORD_COLUMNS} FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE s.fqn = ?1 COLLATE NOCASE ORDER BY f.path, s.start_line"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map([fqn.trim_start_matches('\\')], SymbolRecord::from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Symbols with this short name (case-insensitive).
pub fn all_named(conn: &Connection, name: &str) -> Result<Vec<SymbolRecord>> {
    let sql = format!(
        "SELECT {RECORD_COLUMNS} FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE s.name = ?1 COLLATE NOCASE ORDER BY s.fqn"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map([name], SymbolRecord::from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Direct members (methods, properties, constants, cases) of a class-like.
pub fn members(conn: &Connection, parent_fqn: &str) -> Result<Vec<SymbolRecord>> {
    let sql = format!(
        "SELECT {RECORD_COLUMNS} FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE s.parent_fqn = ?1 COLLATE NOCASE ORDER BY s.start_line"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map([parent_fqn], SymbolRecord::from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// A project class and where it is declared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassRef {
    pub fqn: String,
    pub file: String,
    pub line: u32,
}

/// Project classes extending any of `bases`, directly or through other project classes.
pub fn descendants(conn: &Connection, bases: &[&str]) -> Result<Vec<ClassRef>> {
    let mut stmt = conn.prepare_cached(
        "SELECT s.fqn, f.path, s.start_line FROM symbol_refs r
         JOIN symbols s ON s.id = r.symbol_id JOIN files f ON f.id = s.file_id
         WHERE r.relation = 'extends' AND r.target_fqn = ?1 COLLATE NOCASE AND s.kind = 'class'",
    )?;
    let mut out: Vec<ClassRef> = Vec::new();
    let mut frontier: Vec<String> = bases
        .iter()
        .map(|b| b.trim_start_matches('\\').to_owned())
        .collect();
    while let Some(base) = frontier.pop() {
        let rows = stmt.query_map([&base], |r| {
            Ok(ClassRef {
                fqn: r.get(0)?,
                file: r.get(1)?,
                line: r.get(2)?,
            })
        })?;
        for row in rows {
            let class = row?;
            if !out.iter().any(|c| c.fqn.eq_ignore_ascii_case(&class.fqn)) {
                frontier.push(class.fqn.clone());
                out.push(class);
            }
        }
    }
    out.sort_by(|a, b| a.fqn.cmp(&b.fqn));
    Ok(out)
}

/// Result of looking a member up on a class and what it inherits in the project.
#[derive(Debug, Clone, PartialEq)]
pub enum MemberLookup {
    Found(Box<SymbolRecord>),
    /// The class itself is not in the project index.
    ClassMissing,
    /// Not on the class, its project parents or traits, and every ancestor is indexed.
    MemberMissing,
    /// Not found in project code, but ancestors/traits outside the project
    /// (e.g. vendor classes) may define it. Lists those outside classes.
    Unverifiable(Vec<String>),
}

/// Looks up `member` (`markPaid`, `$fillable`, `STATUS`) on `class_fqn`,
/// following project parents and traits.
pub fn find_member(conn: &Connection, class_fqn: &str, member: &str) -> Result<MemberLookup> {
    let mut queue =
        std::collections::VecDeque::from([class_fqn.trim_start_matches('\\').to_owned()]);
    let mut seen: Vec<String> = Vec::new();
    let mut external: Vec<String> = Vec::new();
    while let Some(class) = queue.pop_front() {
        if seen.iter().any(|s| s.eq_ignore_ascii_case(&class)) {
            continue;
        }
        seen.push(class.clone());
        let classes: Vec<SymbolRecord> = by_fqn(conn, &class)?
            .into_iter()
            .filter(|s| s.kind.is_class_like())
            .collect();
        let Some(found) = classes.first() else {
            if seen.len() == 1 {
                return Ok(MemberLookup::ClassMissing);
            }
            external.push(class);
            continue;
        };
        if let Some(hit) = by_fqn(conn, &format!("{}::{member}", found.fqn))?
            .into_iter()
            .next()
        {
            return Ok(MemberLookup::Found(Box::new(hit)));
        }
        // Parents before traits, so "may be inherited from X" names the base class.
        for c in &classes {
            queue.extend(c.meta.extends.iter().cloned());
            queue.extend(c.meta.traits.iter().cloned());
        }
    }
    Ok(if external.is_empty() {
        MemberLookup::MemberMissing
    } else {
        MemberLookup::Unverifiable(external)
    })
}

// ---------------------------------------------------------------- search

pub const DEFAULT_LIMIT: usize = 20;
pub const MAX_LIMIT: usize = 100;
const MAX_CANDIDATES: usize = 500;
const MAX_SUGGESTIONS: usize = 5;
const SUGGESTION_THRESHOLD: f64 = 0.8;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchInput {
    pub query: String,
    #[serde(default)]
    pub kind: Option<SymbolKind>,
    #[serde(default)]
    pub namespace_prefix: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub detail: Detail,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Suggestion {
    pub name: String,
    pub fqn: String,
    pub kind: SymbolKind,
    pub file: String,
    pub line: u32,
    pub score: f64,
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    /// The `data` part of the response.
    pub data: Value,
    pub next_cursor: Option<String>,
    pub truncated: bool,
}

/// Ranked symbol search. Returns `found: false` with suggestions when nothing matches.
pub fn search(conn: &Connection, input: &SearchInput) -> Result<SearchResult> {
    let query = normalize_query(&input.query);
    let limit = input.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let offset = input.cursor.as_deref().and_then(decode_cursor).unwrap_or(0);

    if query.is_empty() {
        return Ok(not_found(&input.query, Vec::new(), "Empty query."));
    }

    let mut scored: Vec<(f64, SymbolRecord)> = candidates(conn, &query, input)?
        .into_iter()
        .filter_map(|r| score(&query, &r).map(|s| (s, r)))
        .collect();
    // A kind filter that rules everything out: show the other kinds instead.
    let mut kind_note = None;
    if scored.is_empty()
        && let Some(kind) = input.kind
    {
        let any = SearchInput {
            kind: None,
            ..input.clone()
        };
        scored = candidates(conn, &query, &any)?
            .into_iter()
            .filter_map(|r| score(&query, &r).map(|s| (s, r)))
            .collect();
        if !scored.is_empty() {
            kind_note = Some(format!(
                "No {} matches; showing other kinds.",
                kind.as_str()
            ));
        }
    }
    if scored.is_empty() {
        let mut suggestions = suggest(conn, &query, &input.query, input.kind)?;
        if suggestions.is_empty() && input.kind.is_some() {
            suggestions = suggest(conn, &query, &input.query, None)?;
        }
        let hint = match suggestions.first() {
            Some(s) => format!("Did you mean {}?", s.name),
            None => "No symbol matches; it probably does not exist in this project.".into(),
        };
        return Ok(not_found(&input.query, suggestions, &hint));
    }
    scored.sort_by(|(sa, a), (sb, b)| {
        sb.total_cmp(sa)
            .then(a.kind.rank().cmp(&b.kind.rank()))
            .then(a.fqn.len().cmp(&b.fqn.len()))
            .then_with(|| a.fqn.cmp(&b.fqn))
    });

    let total = scored.len();
    let page: Vec<Value> = scored
        .iter()
        .skip(offset)
        .take(limit)
        .map(|(s, r)| r.to_json(input.detail, Some(*s)))
        .collect();
    let next = offset + page.len();
    let has_more = next < total;
    let mut data = json!({ "found": true, "total": total, "matches": page });
    if let Some(n) = kind_note {
        data["note"] = json!(n);
    }
    Ok(SearchResult {
        data,
        next_cursor: has_more.then(|| encode_cursor(next)),
        truncated: has_more,
    })
}

fn not_found(query: &str, suggestions: Vec<Suggestion>, hint: &str) -> SearchResult {
    SearchResult {
        data: json!({
            "found": false,
            "query": query,
            "suggestions": suggestions,
            "hint": hint,
        }),
        next_cursor: None,
        truncated: false,
    }
}

/// Lowercased, trimmed, without a leading `\`.
fn normalize_query(q: &str) -> String {
    q.trim().trim_start_matches('\\').to_lowercase()
}

fn candidates(conn: &Connection, query: &str, input: &SearchInput) -> Result<Vec<SymbolRecord>> {
    let mut filters = String::new();
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(kind) = input.kind {
        filters.push_str(" AND s.kind = ?");
        args.push(Box::new(kind.as_str()));
    }
    if let Some(prefix) = input.namespace_prefix.as_deref() {
        let prefix = prefix.trim().trim_matches('\\');
        if !prefix.is_empty() {
            filters.push_str(" AND (s.fqn LIKE ? ESCAPE '!' OR s.fqn = ? COLLATE NOCASE)");
            args.push(Box::new(format!("{}\\%", escape_like(prefix))));
            args.push(Box::new(prefix.to_owned()));
        }
    }

    // Trigram FTS needs at least 3 characters; shorter queries scan with LIKE.
    let sql = if query.chars().count() >= 3 {
        args.insert(0, Box::new(fts_phrase(query)));
        format!(
            "SELECT {RECORD_COLUMNS} FROM symbols_fts
             JOIN symbols s ON s.id = symbols_fts.rowid
             JOIN files f ON f.id = s.file_id
             WHERE symbols_fts MATCH ?{filters}
             LIMIT {MAX_CANDIDATES}"
        )
    } else {
        let pattern = format!("%{}%", escape_like(query));
        args.insert(0, Box::new(pattern.clone()));
        args.insert(1, Box::new(pattern));
        format!(
            "SELECT {RECORD_COLUMNS} FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE (s.name LIKE ? ESCAPE '!' OR s.fqn LIKE ? ESCAPE '!'){filters}
             LIMIT {MAX_CANDIDATES}"
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let params = rusqlite::params_from_iter(args.iter().map(|a| a.as_ref()));
    let rows = stmt.query_map(params, SymbolRecord::from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// An FTS5 string literal: the whole query as one phrase.
fn fts_phrase(query: &str) -> String {
    format!("\"{}\"", query.replace('"', "\"\""))
}

fn escape_like(s: &str) -> String {
    s.replace('!', "!!").replace('%', "!%").replace('_', "!_")
}

/// Relevance in 0..=1, or `None` when the record does not actually match.
fn score(query: &str, r: &SymbolRecord) -> Option<f64> {
    let name = r.name.to_lowercase();
    let fqn = r.fqn.to_lowercase();
    let base = if fqn == query || name == query {
        1.0
    } else if fqn.ends_with(&format!("\\{query}")) || (query.contains("::") && fqn.ends_with(query))
    {
        0.95
    } else if name.starts_with(query) {
        0.9
    } else if name.contains(query) {
        0.75
    } else if fqn.contains(query) {
        0.6
    } else if r
        .doc_summary
        .as_deref()
        .is_some_and(|d| d.to_lowercase().contains(query))
    {
        0.4
    } else {
        return None;
    };
    let boost = if r.kind.is_class_like()
        || matches!(
            r.kind,
            SymbolKind::Function
                | SymbolKind::Component
                | SymbolKind::Composable
                | SymbolKind::Store
        ) {
        0.05
    } else {
        0.0
    };
    Some(round2(f64::min(base + boost, 1.0)))
}

/// Close names by Jaro-Winkler similarity, best first.
fn suggest(
    conn: &Connection,
    query: &str,
    original: &str,
    kind: Option<SymbolKind>,
) -> Result<Vec<Suggestion>> {
    // Compare like with like: the member part of `Class::member`, the last segment of an FQN.
    let needle = query
        .rsplit("::")
        .next()
        .unwrap_or(query)
        .rsplit('\\')
        .next()
        .unwrap_or(query)
        .trim_start_matches('$');
    let mut stmt = conn.prepare(
        "SELECT s.name, s.fqn, s.kind, f.path, s.start_line
         FROM symbols s JOIN files f ON f.id = s.file_id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, u32>(4)?,
        ))
    })?;
    // Distinctive words of the query (`useCartItems` → cart, items): a name
    // sharing them is a likely match even when spelled differently.
    let last = original
        .rsplit("::")
        .next()
        .unwrap_or(original)
        .rsplit('\\')
        .next()
        .unwrap_or(original);
    let words: Vec<String> = words_of(last)
        .into_iter()
        .filter(|w| w.len() >= 3 && !GENERIC_WORDS.contains(&w.as_str()))
        .collect();
    let mut out: Vec<Suggestion> = Vec::new();
    for row in rows {
        let (name, fqn, kind_str, file, line) = row?;
        let Some(sym_kind) = SymbolKind::parse(&kind_str) else {
            continue;
        };
        if kind.is_some_and(|k| k != sym_kind) {
            continue;
        }
        let lower = name.to_lowercase();
        let mut score = strsim::jaro_winkler(needle, &lower);
        if !words.is_empty() {
            let name_words = words_of(&name);
            let hit = words
                .iter()
                .filter(|w| name_words.iter().any(|n| n == *w))
                .count();
            if hit * 2 >= words.len() && hit > 0 {
                score = score.max(0.8 + 0.15 * hit as f64 / words.len() as f64);
            }
        }
        if score >= SUGGESTION_THRESHOLD {
            out.push(Suggestion {
                name,
                fqn,
                kind: sym_kind,
                file,
                line,
                score: round2(score),
            });
        }
    }
    out.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.kind.rank().cmp(&b.kind.rank()))
            .then_with(|| a.fqn.cmp(&b.fqn))
    });
    out.dedup_by(|a, b| a.fqn == b.fqn);
    out.truncate(MAX_SUGGESTIONS);
    Ok(out)
}

/// Words too common in names to say anything (`use`, `get`, `Controller`…).
const GENERIC_WORDS: &[&str] = &[
    "use",
    "get",
    "set",
    "the",
    "has",
    "can",
    "for",
    "new",
    "app",
    "controller",
    "service",
    "resource",
    "request",
    "model",
    "component",
    "store",
    "page",
    "test",
    "index",
    "base",
];

/// Lowercased words of a camelCase / snake_case / kebab-case name.
fn words_of(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = name.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        let boundary = c.is_uppercase()
            && i > 0
            && (chars[i - 1].is_lowercase()
                || chars.get(i + 1).is_some_and(|n| n.is_lowercase())
                    && chars[i - 1].is_uppercase());
        if boundary && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn encode_cursor(offset: usize) -> String {
    format!("o{offset}")
}

fn decode_cursor(cursor: &str) -> Option<usize> {
    cursor.strip_prefix('o')?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(kind: SymbolKind, name: &str, fqn: &str) -> SymbolRecord {
        SymbolRecord {
            kind,
            name: name.into(),
            fqn: fqn.into(),
            parent_fqn: None,
            signature: String::new(),
            visibility: None,
            is_static: false,
            is_abstract: false,
            is_final: false,
            file: "f.php".into(),
            line: 1,
            end_line: 1,
            doc_summary: Some("Creates orders for customers.".into()),
            meta: SymbolMeta::default(),
        }
    }

    #[test]
    fn scoring_order() {
        let class = record(
            SymbolKind::Class,
            "OrderService",
            "App\\Services\\OrderService",
        );
        let method = record(
            SymbolKind::Method,
            "markPaid",
            "App\\Models\\Order::markPaid",
        );
        assert_eq!(score("orderservice", &class), Some(1.0));
        assert_eq!(score("app\\services\\orderservice", &class), Some(1.0));
        assert_eq!(score("services\\orderservice", &class), Some(1.0));
        assert_eq!(score("order::markpaid", &method), Some(0.95));
        assert_eq!(score("orderserv", &class), Some(0.95));
        assert_eq!(score("service", &class), Some(0.8));
        assert_eq!(score("services", &class), Some(0.65));
        assert_eq!(score("customers", &class), Some(0.45));
        assert_eq!(score("nothing", &class), None);
    }

    #[test]
    fn cursor_roundtrip() {
        assert_eq!(decode_cursor(&encode_cursor(40)), Some(40));
        assert_eq!(decode_cursor("garbage"), None);
    }

    #[test]
    fn query_normalization() {
        assert_eq!(
            normalize_query("  \\App\\Models\\User "),
            "app\\models\\user"
        );
        assert_eq!(fts_phrase("a\"b"), "\"a\"\"b\"");
        assert_eq!(escape_like("a_b%c!"), "a!_b!%c!!");
    }

    #[test]
    fn kind_names_roundtrip() {
        for kind in SymbolKind::ALL {
            assert_eq!(SymbolKind::parse(kind.as_str()), Some(kind));
            assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
        }
    }
}
