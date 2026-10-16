//! HTTP routes: the model, storage and the `list_routes` query. Extraction is
//! split into [`eval`] (static AST evaluation), [`resource`] (resource
//! expansion), [`sources`] (which files register routes) and [`artisan`].

pub mod artisan;
pub mod eval;
pub mod resource;
pub mod sources;

use std::collections::BTreeMap;

use codemap_core::Result;
use codemap_core::envelope::Detail;
use rusqlite::{Connection, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const GET: &[&str] = &["GET", "HEAD"];
pub const ANY: &[&str] = &["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActionKind {
    Controller,
    Closure,
    View,
    Redirect,
    Health,
    Fallback,
    /// A Next.js route handler (`app/api/x/route.ts`).
    Handler,
    #[default]
    Unresolved,
}

impl ActionKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Controller => "controller",
            Self::Closure => "closure",
            Self::View => "view",
            Self::Redirect => "redirect",
            Self::Health => "health",
            Self::Fallback => "fallback",
            Self::Handler => "handler",
            Self::Unresolved => "unresolved",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "controller" => Self::Controller,
            "closure" => Self::Closure,
            "view" => Self::View,
            "redirect" => Self::Redirect,
            "health" => Self::Health,
            "fallback" => Self::Fallback,
            "handler" => Self::Handler,
            _ => Self::Unresolved,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RouteSource {
    #[default]
    Static,
    Artisan,
    /// Next.js route handlers, written by the frontend adapter.
    Next,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteExtra {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub wheres: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub without_middleware: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Registered inside an `if`: may not exist at runtime.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub conditional: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub defaults: BTreeMap<String, String>,
}

impl RouteExtra {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteDef {
    pub methods: Vec<String>,
    pub uri: String,
    pub name: Option<String>,
    pub action_kind: ActionKind,
    pub action_raw: Option<String>,
    pub controller_fqn: Option<String>,
    pub controller_method: Option<String>,
    pub middleware: Vec<String>,
    pub prefix: Option<String>,
    pub domain: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub source: RouteSource,
    pub unresolved_reason: Option<String>,
    pub extra: RouteExtra,
}

impl RouteDef {
    pub fn methods_str(&self) -> String {
        self.methods.join("|")
    }

    /// `App\Http\Controllers\OrderController@index`, `Closure`, `view:welcome`, ...
    pub fn action_label(&self) -> String {
        match (&self.controller_fqn, &self.controller_method) {
            (Some(c), Some(m)) => format!("{c}@{m}"),
            _ => match self.action_kind {
                ActionKind::Closure | ActionKind::Health => "Closure".into(),
                ActionKind::View => format!("view:{}", self.extra.view.as_deref().unwrap_or("?")),
                ActionKind::Redirect => {
                    format!(
                        "redirect:{}",
                        self.extra.redirect_to.as_deref().unwrap_or("?")
                    )
                }
                _ => self.action_raw.clone().unwrap_or_else(|| "?".into()),
            },
        }
    }

    pub fn location(&self) -> Option<String> {
        Some(format!("{}:{}", self.file.as_deref()?, self.line?))
    }

    /// One line for snapshots and `doctor`.
    pub fn table_line(&self) -> String {
        let mut line = format!(
            "{:<10} {:<36} {:<24} {:<56} [{}] {} {}",
            self.methods_str(),
            self.uri,
            self.name.as_deref().unwrap_or("-"),
            self.action_label(),
            self.middleware.join(","),
            self.location().unwrap_or_else(|| "-".into()),
            match self.source {
                RouteSource::Static => "static",
                RouteSource::Artisan => "artisan",
                RouteSource::Next => "next",
            },
        );
        if let Some(reason) = &self.unresolved_reason {
            line.push_str(&format!("  UNRESOLVED: {reason}"));
        }
        line.trim_end().to_owned()
    }

    pub fn to_json(&self, detail: Detail) -> Value {
        let mut v = json!({
            "methods": self.methods_str(),
            "uri": self.uri,
            "name": self.name,
            "action": self.action_label(),
            "middleware": self.middleware,
            "file": self.location(),
            "source": self.source,
        });
        let obj = v.as_object_mut().expect("object");
        if let Some(reason) = &self.unresolved_reason {
            obj.insert("unresolved_reason".into(), json!(reason));
        }
        if detail == Detail::Full {
            obj.insert("action_kind".into(), json!(self.action_kind));
            obj.insert("controller_fqn".into(), json!(self.controller_fqn));
            obj.insert("controller_method".into(), json!(self.controller_method));
            if let Some(d) = &self.domain {
                obj.insert("domain".into(), json!(d));
            }
            if let Some(p) = &self.prefix {
                obj.insert("prefix".into(), json!(p));
            }
            if !self.extra.is_empty() {
                obj.insert("extra".into(), json!(self.extra));
            }
        }
        v
    }
}

/// Joins a group prefix and a URI the way Laravel does; the root is `/`.
pub fn join_uri(prefix: &str, uri: &str) -> String {
    let joined = format!("{}/{}", prefix.trim_matches('/'), uri.trim_matches('/'));
    let joined = joined.trim_matches('/');
    if joined.is_empty() {
        "/".into()
    } else {
        joined.to_owned()
    }
}

// ---------------------------------------------------------------- storage

pub fn replace_all(conn: &Connection, routes: &[RouteDef]) -> Result<()> {
    conn.execute("DELETE FROM routes WHERE source != 'next'", [])?;
    let mut stmt = conn.prepare_cached(
        "INSERT INTO routes (seq, methods, uri, name, action_kind, action_raw, controller_fqn,
                             controller_method, middleware_json, prefix, domain, file, line,
                             source, unresolved_reason, extra_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
    )?;
    for (seq, r) in routes.iter().enumerate() {
        let extra = if r.extra.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&r.extra)?)
        };
        stmt.execute(params![
            seq as i64,
            r.methods_str(),
            r.uri,
            r.name,
            r.action_kind.as_str(),
            r.action_raw,
            r.controller_fqn,
            r.controller_method,
            serde_json::to_string(&r.middleware)?,
            r.prefix,
            r.domain,
            r.file,
            r.line,
            match r.source {
                RouteSource::Static => "static",
                RouteSource::Artisan => "artisan",
                RouteSource::Next => "next",
            },
            r.unresolved_reason,
            extra,
        ])?;
    }
    Ok(())
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<RouteDef> {
    let methods: String = row.get(0)?;
    let middleware: String = row.get(7)?;
    let source: String = row.get(12)?;
    let extra: Option<String> = row.get(14)?;
    Ok(RouteDef {
        methods: methods.split('|').map(str::to_owned).collect(),
        uri: row.get(1)?,
        name: row.get(2)?,
        action_kind: ActionKind::parse(&row.get::<_, String>(3)?),
        action_raw: row.get(4)?,
        controller_fqn: row.get(5)?,
        controller_method: row.get(6)?,
        middleware: serde_json::from_str(&middleware).unwrap_or_default(),
        prefix: row.get(8)?,
        domain: row.get(9)?,
        file: row.get(10)?,
        line: row.get(11)?,
        source: match source.as_str() {
            "artisan" => RouteSource::Artisan,
            "next" => RouteSource::Next,
            _ => RouteSource::Static,
        },
        unresolved_reason: row.get(13)?,
        extra: extra
            .and_then(|e| serde_json::from_str(&e).ok())
            .unwrap_or_default(),
    })
}

/// All routes in registration order.
pub fn all(conn: &Connection) -> Result<Vec<RouteDef>> {
    let mut stmt = conn.prepare_cached(
        "SELECT methods, uri, name, action_kind, action_raw, controller_fqn, controller_method,
                middleware_json, prefix, domain, file, line, source, unresolved_reason, extra_json
         FROM routes ORDER BY seq",
    )?;
    let rows = stmt.query_map([], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

// ---------------------------------------------------------------- list_routes

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListInput {
    #[serde(default)]
    pub uri_contains: Option<String>,
    #[serde(default)]
    pub name_prefix: Option<String>,
    #[serde(default)]
    pub method: Option<String>,
    /// Controller FQN or short name (substring, case-insensitive).
    #[serde(default)]
    pub controller: Option<String>,
    #[serde(default)]
    pub middleware: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub detail: Detail,
}

pub struct ListResult {
    pub data: Value,
    pub next_cursor: Option<String>,
    pub truncated: bool,
}

fn matches(r: &RouteDef, f: &ListInput) -> bool {
    let lc = |s: &str| s.to_lowercase();
    if let Some(u) = &f.uri_contains {
        let u = lc(u.trim_matches('/'));
        if !lc(&r.uri).contains(&u) {
            return false;
        }
    }
    if let Some(p) = &f.name_prefix
        && !r.name.as_deref().is_some_and(|n| n.starts_with(p.as_str()))
    {
        return false;
    }
    if let Some(m) = &f.method
        && !r.methods.iter().any(|x| x.eq_ignore_ascii_case(m))
    {
        return false;
    }
    if let Some(c) = &f.controller {
        let c = lc(c.trim_start_matches('\\'));
        if !r
            .controller_fqn
            .as_deref()
            .is_some_and(|fqn| lc(fqn).contains(&c))
        {
            return false;
        }
    }
    if let Some(m) = &f.middleware {
        let hit = r
            .middleware
            .iter()
            .any(|x| x == m || x.starts_with(&format!("{m}:")));
        if !hit {
            return false;
        }
    }
    true
}

pub fn list(conn: &Connection, input: &ListInput) -> Result<ListResult> {
    list_with(conn, input, |_, _| {})
}

/// [`list`], letting the caller add to each route's entry (e.g. permissions).
pub fn list_with(
    conn: &Connection,
    input: &ListInput,
    extra: impl Fn(&RouteDef, &mut Value),
) -> Result<ListResult> {
    let limit = input
        .limit
        .unwrap_or(codemap_core::symbols::DEFAULT_LIMIT)
        .clamp(1, codemap_core::symbols::MAX_LIMIT);
    let offset = input
        .cursor
        .as_deref()
        .and_then(|c| c.strip_prefix('o')?.parse().ok())
        .unwrap_or(0usize);
    let routes = all(conn)?;
    let hits: Vec<&RouteDef> = routes.iter().filter(|r| matches(r, input)).collect();

    if hits.is_empty() {
        return Ok(ListResult {
            data: not_found(&routes, input),
            next_cursor: None,
            truncated: false,
        });
    }
    let page: Vec<Value> = hits
        .iter()
        .skip(offset)
        .take(limit)
        .map(|r| {
            let mut v = r.to_json(input.detail);
            extra(r, &mut v);
            v
        })
        .collect();
    let next = offset + page.len();
    let more = next < hits.len();
    Ok(ListResult {
        data: json!({ "found": true, "total": hits.len(), "routes": page }),
        next_cursor: more.then(|| format!("o{next}")),
        truncated: more,
    })
}

/// `found: false` with the closest route names / URIs to what was asked for.
fn not_found(routes: &[RouteDef], f: &ListInput) -> Value {
    let query = json!({
        "uri_contains": f.uri_contains,
        "name_prefix": f.name_prefix,
        "method": f.method,
        "controller": f.controller,
        "middleware": f.middleware,
    });
    let needle = f
        .name_prefix
        .as_deref()
        .or(f.uri_contains.as_deref())
        .or(f.controller.as_deref())
        .unwrap_or("")
        .to_lowercase();
    let mut suggestions: Vec<(f64, &RouteDef)> = Vec::new();
    if !needle.is_empty() {
        for r in routes {
            let candidates = [
                r.name.clone().unwrap_or_default(),
                r.uri.clone(),
                r.controller_fqn
                    .as_deref()
                    .map(|c| c.rsplit('\\').next().unwrap_or(c).to_owned())
                    .unwrap_or_default(),
            ];
            let best = candidates
                .iter()
                .filter(|c| !c.is_empty())
                .map(|c| strsim::jaro_winkler(&needle, &c.to_lowercase()))
                .fold(0.0, f64::max);
            if best >= 0.75 {
                suggestions.push((best, r));
            }
        }
    }
    suggestions.sort_by(|a, b| b.0.total_cmp(&a.0));
    suggestions.truncate(5);
    let list: Vec<Value> = suggestions
        .iter()
        .map(|(score, r)| {
            json!({
                "methods": r.methods_str(),
                "uri": r.uri,
                "name": r.name,
                "file": r.location(),
                "score": (score * 100.0).round() / 100.0,
            })
        })
        .collect();
    let hint = if routes.is_empty() {
        "This project has no indexed routes.".to_owned()
    } else if let Some((_, r)) = suggestions.first() {
        format!(
            "No route matches; closest is {} {}",
            r.methods_str(),
            r.name.as_deref().unwrap_or(&r.uri)
        )
    } else {
        "No route matches these filters.".to_owned()
    };
    json!({ "found": false, "query": query, "suggestions": list, "hint": hint })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_joining() {
        assert_eq!(join_uri("", "/"), "/");
        assert_eq!(join_uri("api", "/orders/"), "api/orders");
        assert_eq!(
            join_uri("/api/v1/", "orders/{order}"),
            "api/v1/orders/{order}"
        );
        assert_eq!(join_uri("admin", ""), "admin");
    }
}
