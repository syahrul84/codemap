//! The Vue / Nuxt part of `codemap doctor`: how the frontend was understood and
//! what could not be resolved.

use std::fmt::Write as _;

use codemap_core::{Result, Store};
use serde::Serialize;

use crate::refresh::{ADAPTER, stored_detection};

#[derive(Debug, Clone, Serialize)]
pub struct TagIssue {
    pub tag: String,
    pub uses: usize,
    pub example: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VueDoctor {
    pub framework: String,
    pub src_dir: String,
    pub reasons: Vec<String>,
    pub warnings: Vec<String>,
    pub components: usize,
    pub pages: usize,
    /// Tags that resolve to no project file, registration or known library.
    pub unresolved_tags: Vec<TagIssue>,
    pub unresolved_tags_total: usize,
    /// `file:line message` for JS/TS/Vue code the parser could not read.
    pub parse_errors: Vec<String>,
    pub parse_errors_total: usize,
    pub routes: usize,
    /// Routes whose component import points at nothing (`file:line note`).
    pub broken_routes: Vec<String>,
    /// Routes built at runtime, which can't be listed statically.
    pub dynamic_routes: Vec<String>,
}

const LIMIT: usize = 40;

pub fn build(store: &Store) -> Result<Option<VueDoctor>> {
    let Some(d) = stored_detection(store)? else {
        return Ok(None);
    };
    if !d.vue {
        return Ok(None);
    }
    let conn = store.conn();
    let framework = match (&d.nuxt, &d.vue_package) {
        _ if d.next.is_some() => format!(
            "Next.js {}",
            d.next
                .as_ref()
                .map(|n| n.package.display())
                .unwrap_or_default()
        ),
        _ if d.react.is_some() => format!(
            "React {}",
            d.react.as_ref().map(|r| r.display()).unwrap_or_default()
        ),
        (Some(n), _) => format!("Nuxt {}", n.package.display()),
        (None, Some(v)) => format!("Vue {}", v.display()),
        _ => "Vue".into(),
    };
    let count = |sql: &str| -> Result<usize> { Ok(conn.query_row(sql, [], |r| r.get(0))?) };
    // One example per tag; its note (e.g. the missing import) belongs to that same use.
    let mut stmt = conn.prepare(
        "SELECT tag, n, from_path || ':' || line, package FROM (
             SELECT tag, from_path, line, package, COUNT(*) OVER (PARTITION BY tag) AS n,
                    ROW_NUMBER() OVER (PARTITION BY tag ORDER BY package IS NULL, from_path, line) AS rn
             FROM component_uses WHERE via = 'unresolved')
         WHERE rn = 1 ORDER BY n DESC, tag",
    )?;
    let all: Vec<TagIssue> = stmt
        .query_map([], |r| {
            Ok(TagIssue {
                tag: r.get(0)?,
                uses: r.get(1)?,
                example: r.get(2)?,
                note: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let total = all.len();
    let mut errs = conn.prepare(
        "SELECT COALESCE(path, '?') || ':' || COALESCE(line, 0) || ' ' || message
         FROM warnings WHERE adapter = 'js' ORDER BY path, line",
    )?;
    let errors: Vec<String> = errs
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut w = conn.prepare("SELECT message FROM warnings WHERE adapter = ?1")?;
    let warnings: Vec<String> = w
        .query_map([ADAPTER], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut q = conn.prepare(
        "SELECT defined_in || '  ' || route_path || '  ' || note FROM pages
         WHERE note IS NOT NULL AND route_path <> '(dynamic)' ORDER BY defined_in",
    )?;
    let broken: Vec<String> = q
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut q = conn.prepare(
        "SELECT defined_in || '  ' || note FROM pages WHERE route_path = '(dynamic)' ORDER BY defined_in",
    )?;
    let dynamic: Vec<String> = q
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some(VueDoctor {
        routes: count("SELECT COUNT(*) FROM pages WHERE route_path <> '(dynamic)'")?,
        broken_routes: broken,
        dynamic_routes: dynamic,
        framework,
        src_dir: if d.src_dir.is_empty() {
            ".".into()
        } else {
            d.src_dir.clone()
        },
        reasons: d.reasons.clone(),
        warnings,
        components: count("SELECT COUNT(*) FROM js_modules WHERE kind IN ('component', 'layout')")?,
        pages: count("SELECT COUNT(*) FROM js_modules WHERE kind = 'page'")?,
        unresolved_tags: all.into_iter().take(LIMIT).collect(),
        unresolved_tags_total: total,
        parse_errors_total: errors.len(),
        parse_errors: errors.into_iter().take(LIMIT).collect(),
    }))
}

pub fn render_text(r: &VueDoctor) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "\nfrontend     {} (src: {})", r.framework, r.src_dir);
    for reason in &r.reasons {
        let _ = writeln!(s, "  - {reason}");
    }
    let _ = writeln!(s, "components   {} ({} pages)", r.components, r.pages);
    for w in &r.warnings {
        let _ = writeln!(s, "  ! {w}");
    }
    let _ = writeln!(s, "pages/routes {}", r.routes);
    if !r.broken_routes.is_empty() {
        let _ = writeln!(
            s,
            "\nroutes whose component is missing ({}):",
            r.broken_routes.len()
        );
        for b in r.broken_routes.iter().take(LIMIT) {
            let _ = writeln!(s, "  {b}");
        }
    }
    if !r.dynamic_routes.is_empty() {
        let _ = writeln!(
            s,
            "\nroutes built at runtime, not listed ({}):",
            r.dynamic_routes.len()
        );
        for b in r.dynamic_routes.iter().take(LIMIT) {
            let _ = writeln!(s, "  {b}");
        }
    }
    if r.unresolved_tags_total > 0 {
        let _ = writeln!(
            s,
            "\nunresolved component tags ({}): not a project component, registration or known library",
            r.unresolved_tags_total
        );
        for t in &r.unresolved_tags {
            let _ = writeln!(
                s,
                "  <{}> ×{}  e.g. {}{}",
                t.tag,
                t.uses,
                t.example,
                t.note
                    .as_ref()
                    .map(|n| format!("  ({n})"))
                    .unwrap_or_default()
            );
        }
        if r.unresolved_tags_total > r.unresolved_tags.len() {
            let _ = writeln!(
                s,
                "  … {} more",
                r.unresolved_tags_total - r.unresolved_tags.len()
            );
        }
    }
    if r.parse_errors_total > 0 {
        let _ = writeln!(
            s,
            "\nJS/TS/Vue code the parser could not read ({}):",
            r.parse_errors_total
        );
        for e in &r.parse_errors {
            let _ = writeln!(s, "  {e}");
        }
    }
    s
}
