//! Screens: Nuxt file-based pages (Nuxt 2 `_id`, Nuxt 3+ `[id]`, nesting,
//! `definePageMeta`) and vue-router route records stitched together across
//! files (`routes: [adminRoutes, ...reportRoutes]`, `children`).

use std::collections::{HashMap, HashSet};

use pidx_core::envelope::Detail;
use pidx_core::{Result, Store};
use pidx_js::{RouteItem, RouteNode};
use rusqlite::params;
use serde::Serialize;
use serde_json::{Value, json};

use crate::detect::VueDetection;
use crate::paths::{Resolved, Resolver};
use crate::registry::{Module, import_source, nuxt_route_name};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PageRoute {
    pub path: String,
    pub name: Option<String>,
    pub file: Option<String>,
    pub source: &'static str,
    pub defined_in: String,
    pub parent: Option<String>,
    pub layout: Option<String>,
    pub middleware: Vec<String>,
    pub redirect: Option<String>,
    pub meta: Option<String>,
    pub note: Option<String>,
    /// Has child routes (a layout-like parent rather than a screen).
    #[serde(skip)]
    pub has_children: bool,
}

pub fn build(d: &VueDetection, modules: &[Module], resolver: &Resolver<'_>) -> Vec<PageRoute> {
    let mut out = Vec::new();
    if d.is_nuxt() {
        nuxt_pages(d, modules, &mut out);
    }
    router_pages(modules, resolver, &mut out);
    out
}

// ---------------------------------------------------------------- Nuxt

/// `orders/[id].vue` → `/orders/:id` (Nuxt 3+) or `orders/_id.vue` (Nuxt 2).
pub fn nuxt_route_path(rel: &str, nuxt2: bool) -> String {
    let stem = rel.rsplit_once('.').map_or(rel, |(s, _)| s);
    let mut parts = Vec::new();
    for seg in stem.split('/') {
        if seg == "index" || (seg.starts_with('(') && seg.ends_with(')')) {
            continue;
        }
        let p = if nuxt2 {
            if seg == "_" {
                "*".to_owned()
            } else if let Some(param) = seg.strip_prefix('_') {
                format!(":{param}")
            } else {
                seg.to_owned()
            }
        } else if let Some(inner) = seg.strip_prefix("[...").and_then(|s| s.strip_suffix(']')) {
            format!(":{inner}(.*)*")
        } else if let Some(inner) = seg.strip_prefix("[[").and_then(|s| s.strip_suffix("]]")) {
            format!(":{inner}?")
        } else {
            seg.replace('[', ":").replace(']', "")
        };
        parts.push(p);
    }
    format!("/{}", parts.join("/"))
}

fn string_list(text: &str) -> Vec<String> {
    // `['auth', 'admin']`, `'auth'`, `auth`.
    text.split(',')
        .map(|s| {
            s.trim()
                .trim_matches(['[', ']', ' '])
                .trim_matches(['\'', '"', '`'])
                .to_owned()
        })
        .filter(|s| !s.is_empty() && !s.contains(['(', '{', ' ']))
        .collect()
}

fn nuxt_pages(d: &VueDetection, modules: &[Module], out: &mut Vec<PageRoute>) {
    let prefix = format!("{}/", d.in_src("pages"));
    let nuxt2 = d.nuxt.as_ref().is_some_and(|n| n.major == Some(2));
    let layouts_dir = format!("{}/", d.in_src("layouts"));
    let has_default_layout = modules.iter().any(|m| {
        m.path.starts_with(&layouts_dir) && m.path[layouts_dir.len()..].starts_with("default.")
    });
    let page_files: HashSet<&str> = modules
        .iter()
        .filter(|m| m.path.starts_with(&prefix) && m.facts.lang == "vue")
        .map(|m| m.path.as_str())
        .collect();
    for m in modules
        .iter()
        .filter(|m| page_files.contains(m.path.as_str()))
    {
        let rel = &m.path[prefix.len()..];
        let mut path = nuxt_route_path(rel, nuxt2);
        let mut name = Some(nuxt_route_name(rel));
        let meta = &m.facts.script.setup.page_meta;
        let get = |k: &str| {
            meta.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
        };
        if let Some(p) = get("path") {
            path = p;
        }
        if let Some(n) = get("name") {
            name = Some(n);
        }
        let opts = m.facts.script.options.as_ref();
        let layout = get("layout")
            .or_else(|| opts.and_then(|o| o.layout.clone()))
            .filter(|l| l != "false")
            .or_else(|| has_default_layout.then(|| "default".to_owned()));
        let mut middleware = get("middleware")
            .map(|t| string_list(&t))
            .unwrap_or_default();
        if let Some(o) = opts {
            middleware.extend(o.middleware.iter().cloned());
        }
        // Nested: `pages/users/[id].vue` is the parent of `pages/users/[id]/*.vue`.
        let mut parent = None;
        let mut dir = rel.rsplit_once('/').map(|(d, _)| d.to_owned());
        while let Some(dd) = dir {
            if page_files.contains(format!("{prefix}{dd}.vue").as_str()) {
                parent = Some(nuxt_route_path(&format!("{dd}.vue"), nuxt2));
                break;
            }
            dir = dd.rsplit_once('/').map(|(d, _)| d.to_owned());
        }
        let other_meta: Vec<String> = meta
            .iter()
            .filter(|(k, _)| !matches!(k.as_str(), "path" | "name" | "layout" | "middleware"))
            .map(|(k, v)| format!("{k}: {v}"))
            .collect();
        out.push(PageRoute {
            path,
            name,
            file: Some(m.path.clone()),
            source: "nuxt",
            defined_in: format!("{}:1", m.path),
            parent,
            layout,
            middleware,
            redirect: get("redirect"),
            meta: (!other_meta.is_empty()).then(|| other_meta.join(", ")),
            note: None,
            has_children: false,
        });
    }
}

// ---------------------------------------------------------------- vue-router

struct Ctx<'m, 'r> {
    by_path: HashMap<&'m str, &'m Module>,
    resolver: &'m Resolver<'r>,
    referenced: HashSet<(String, String)>,
}

fn router_pages(modules: &[Module], resolver: &Resolver<'_>, out: &mut Vec<PageRoute>) {
    let mut ctx = Ctx {
        by_path: modules.iter().map(|m| (m.path.as_str(), m)).collect(),
        resolver,
        referenced: HashSet::new(),
    };
    // Which groups are reachable as references from other groups.
    for m in modules {
        for g in &m.facts.script.route_groups {
            collect_refs(&mut ctx, m, &g.items);
        }
    }
    let mut roots: Vec<(&Module, &pidx_js::RouteGroup)> = modules
        .iter()
        .flat_map(|m| m.facts.script.route_groups.iter().map(move |g| (m, g)))
        .filter(|(_, g)| g.binding == "#router")
        .collect();
    if roots.is_empty() {
        // No router call found: start from groups nobody references.
        roots = modules
            .iter()
            .flat_map(|m| m.facts.script.route_groups.iter().map(move |g| (m, g)))
            .filter(|(m, g)| {
                !ctx.referenced
                    .contains(&(m.path.clone(), g.binding.clone()))
            })
            .filter(|(_, g)| g.items.iter().any(|i| matches!(i, RouteItem::Node(_))))
            .collect();
    }
    let mut seen = HashSet::new();
    for (m, g) in roots {
        walk(&ctx, m, &g.items, "", None, out, &mut seen, 0);
    }
}

fn collect_refs(ctx: &mut Ctx<'_, '_>, m: &Module, items: &[RouteItem]) {
    for i in items {
        match i {
            RouteItem::Ref { name, .. } => {
                if let Some(target) = resolve_ref(ctx, m, name) {
                    ctx.referenced.insert(target);
                }
            }
            RouteItem::Node(n) => collect_refs(ctx, m, &n.children),
            RouteItem::Dynamic { .. } => {}
        }
    }
}

/// `(file, binding)` of the route group a name refers to.
fn resolve_ref(ctx: &Ctx<'_, '_>, m: &Module, name: &str) -> Option<(String, String)> {
    if m.facts
        .script
        .route_groups
        .iter()
        .any(|g| g.binding == name)
    {
        return Some((m.path.clone(), name.to_owned()));
    }
    let imp = m
        .facts
        .script
        .imports
        .iter()
        .find(|i| !i.dynamic && i.binds(name))?;
    let binding = if imp.default.as_deref() == Some(name) {
        "default".to_owned()
    } else {
        imp.named
            .iter()
            .find(|(_, l)| l == name)
            .map(|(i, _)| i.clone())?
    };
    match ctx.resolver.resolve(&m.path, &imp.source) {
        Resolved::File(f) => Some((f, binding)),
        _ => None,
    }
}

fn join(parent: &str, child: &str) -> String {
    if child.starts_with('/') {
        child.to_owned()
    } else if child.is_empty() {
        if parent.is_empty() {
            "/".to_owned()
        } else {
            parent.to_owned()
        }
    } else {
        format!("{}/{child}", parent.trim_end_matches('/'))
    }
}

#[allow(clippy::too_many_arguments)]
fn walk(
    ctx: &Ctx<'_, '_>,
    m: &Module,
    items: &[RouteItem],
    parent: &str,
    parent_path: Option<&str>,
    out: &mut Vec<PageRoute>,
    seen: &mut HashSet<(String, String, String)>,
    depth: u8,
) {
    if depth > 16 {
        return;
    }
    for item in items {
        match item {
            RouteItem::Node(n) => node(ctx, m, n, parent, parent_path, out, seen, depth),
            RouteItem::Ref { name, line } => match resolve_ref(ctx, m, name) {
                Some((file, binding)) => {
                    if !seen.insert((file.clone(), binding.clone(), parent.to_owned())) {
                        continue;
                    }
                    let target = ctx.by_path.get(file.as_str());
                    let group = target.and_then(|t| {
                        t.facts
                            .script
                            .route_groups
                            .iter()
                            .find(|g| g.binding == binding)
                    });
                    match (target, group) {
                        (Some(t), Some(g)) => {
                            walk(ctx, t, &g.items, parent, parent_path, out, seen, depth + 1)
                        }
                        _ => out.push(unreadable(
                            m,
                            *line,
                            parent,
                            format!("`{name}` ({file}) holds no readable route records"),
                        )),
                    }
                }
                None => out.push(unreadable(
                    m,
                    *line,
                    parent,
                    format!("`{name}` could not be traced to route records"),
                )),
            },
            RouteItem::Dynamic { text, line } => out.push(unreadable(
                m,
                *line,
                parent,
                format!("built at runtime: {text}"),
            )),
        }
    }
}

fn unreadable(m: &Module, line: u32, parent: &str, note: String) -> PageRoute {
    PageRoute {
        path: "(dynamic)".into(),
        source: "router",
        defined_in: format!("{}:{line}", m.path),
        parent: (!parent.is_empty()).then(|| parent.to_owned()),
        note: Some(note),
        ..PageRoute::default()
    }
}

#[allow(clippy::too_many_arguments)]
fn node(
    ctx: &Ctx<'_, '_>,
    m: &Module,
    n: &RouteNode,
    parent: &str,
    parent_path: Option<&str>,
    out: &mut Vec<PageRoute>,
    seen: &mut HashSet<(String, String, String)>,
    depth: u8,
) {
    let Some(p) = &n.path else {
        out.push(unreadable(
            m,
            n.line,
            parent,
            "route path is not a literal".into(),
        ));
        return;
    };
    let full = join(parent, p);
    let (file, note) = match n.component.as_deref() {
        Some(c) => {
            let spec = c.strip_prefix("import:").map(str::to_owned).or_else(|| {
                c.strip_prefix("local:")
                    .and_then(|l| import_source(&m.facts, l))
            });
            match spec.map(|s| ctx.resolver.resolve(&m.path, &s)) {
                Some(Resolved::File(f)) => (Some(f), None),
                Some(Resolved::Missing(f)) => {
                    (None, Some(format!("component file not found: {f}")))
                }
                // `RouterView` and other library components are fine.
                Some(Resolved::Package(_)) => (None, None),
                None => (
                    None,
                    Some(format!(
                        "component `{}` is not an import",
                        c.trim_start_matches("local:")
                    )),
                ),
            }
        }
        None => (None, None),
    };
    out.push(PageRoute {
        path: full.clone(),
        name: n.name.clone(),
        file,
        source: "router",
        defined_in: format!("{}:{}", m.path, n.line),
        parent: parent_path.map(str::to_owned),
        layout: None,
        middleware: Vec::new(),
        redirect: n.redirect.clone(),
        meta: n.meta.clone(),
        note,
        has_children: !n.children.is_empty(),
    });
    walk(
        ctx,
        m,
        &n.children,
        &full,
        Some(&full),
        out,
        seen,
        depth + 1,
    );
}

// ---------------------------------------------------------------- store / query

pub fn store(tx: &rusqlite::Transaction<'_>, pages: &[PageRoute]) -> Result<()> {
    tx.execute("DELETE FROM pages WHERE source IN ('nuxt', 'router')", [])?;
    let mut ins = tx.prepare(
        "INSERT INTO pages (route_path, name, file, source, defined_in, parent, layout, middleware_json, redirect, meta, note)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )?;
    for p in pages {
        let mw = (!p.middleware.is_empty())
            .then(|| serde_json::to_string(&p.middleware).unwrap_or_default());
        ins.execute(params![
            p.path,
            p.name,
            p.file,
            p.source,
            p.defined_in,
            p.parent,
            p.layout,
            mw,
            p.redirect,
            p.meta,
            p.note
        ])?;
    }
    Ok(())
}

#[derive(Debug, Default, Clone)]
pub struct PageFilter {
    pub path_contains: Option<String>,
    pub name_prefix: Option<String>,
    pub file: Option<String>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
    pub detail: Detail,
}

pub struct PageList {
    pub data: Value,
    pub next_cursor: Option<String>,
    pub truncated: bool,
}

pub fn list(store: &Store, f: &PageFilter) -> Result<PageList> {
    let mut stmt = store.conn().prepare(
        "SELECT route_path, name, file, source, defined_in, parent, layout, middleware_json, redirect, meta, note
         FROM pages ORDER BY (route_path = '(dynamic)'), route_path, name",
    )?;
    let rows: Vec<PageRoute> = stmt
        .query_map([], |r| {
            Ok(PageRoute {
                path: r.get(0)?,
                name: r.get(1)?,
                file: r.get(2)?,
                source: match r.get::<_, String>(3)?.as_str() {
                    "nuxt" => "nuxt",
                    "filament" => "filament",
                    _ => "router",
                },
                defined_in: r.get(4)?,
                parent: r.get(5)?,
                layout: r.get(6)?,
                middleware: r
                    .get::<_, Option<String>>(7)?
                    .and_then(|j| serde_json::from_str(&j).ok())
                    .unwrap_or_default(),
                redirect: r.get(8)?,
                meta: r.get(9)?,
                note: r.get(10)?,
                has_children: false,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let total_all = rows.len();
    let dynamic = rows.iter().filter(|p| p.path == "(dynamic)").count();
    let lc = |s: &str| s.to_lowercase();
    let filtered: Vec<&PageRoute> = rows
        .iter()
        .filter(|p| {
            f.path_contains
                .as_deref()
                .is_none_or(|q| lc(&p.path).contains(&lc(q)))
        })
        .filter(|p| {
            f.name_prefix
                .as_deref()
                .is_none_or(|q| p.name.as_deref().is_some_and(|n| n.starts_with(q)))
        })
        .filter(|p| {
            f.file.as_deref().is_none_or(|q| {
                p.file
                    .as_deref()
                    .is_some_and(|x| x == q || x.ends_with(&format!("/{q}")) || x.contains(q))
            })
        })
        .collect();
    let limit = f.limit.unwrap_or(20).clamp(1, 100);
    let offset = f
        .cursor
        .as_deref()
        .and_then(|c| c.strip_prefix('o'))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0usize);
    let page: Vec<Value> = filtered
        .iter()
        .skip(offset)
        .take(limit)
        .map(|p| {
            if f.detail == Detail::Full {
                json!(p)
            } else {
                let mut s = p.path.clone();
                if let Some(n) = &p.name {
                    s.push_str(&format!(" [{n}]"));
                }
                match (&p.file, &p.redirect) {
                    (Some(file), _) => s.push_str(&format!(" → {file}")),
                    (None, Some(r)) => s.push_str(&format!(" → redirect {r}")),
                    _ => {}
                }
                if let Some(l) = &p.layout {
                    s.push_str(&format!(" (layout {l})"));
                }
                if !p.middleware.is_empty() {
                    s.push_str(&format!(" (middleware {})", p.middleware.join(", ")));
                }
                if let Some(n) = &p.note {
                    s.push_str(&format!(" — {n}"));
                }
                if p.source == "filament"
                    && let Some(m) = &p.meta
                {
                    let m: String = m.chars().take(300).collect();
                    s.push_str(&format!(" — {m}"));
                }
                s.push_str(&format!("  {}", p.defined_in));
                json!(s)
            }
        })
        .collect();
    let next = (offset + limit < filtered.len()).then(|| format!("o{}", offset + limit));
    let mut data = json!({
        "found": !filtered.is_empty(),
        "total": filtered.len(),
        "pages": page,
    });
    if dynamic > 0 {
        data["unreadable_routes"] = json!(dynamic);
        data["note"] = json!(
            "`(dynamic)` entries are routes built at runtime (helper functions, loops); they exist but their path is not known statically."
        );
    }
    if total_all == 0 {
        data["hint"] = json!(
            "No Nuxt pages, vue-router routes or Filament admin pages were found in this project."
        );
    }
    Ok(PageList {
        truncated: next.is_some(),
        next_cursor: next,
        data,
    })
}

/// Routes that render the component at `file`.
pub fn routes_for_file(store: &Store, file: &str) -> Result<Vec<String>> {
    let mut stmt = store
        .conn()
        .prepare("SELECT route_path, name FROM pages WHERE file = ?1 ORDER BY route_path")?;
    let rows = stmt.query_map([file], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    })?;
    Ok(rows
        .flatten()
        .map(|(p, n)| match n {
            Some(n) => format!("{p} [{n}]"),
            None => p,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nuxt_paths() {
        assert_eq!(nuxt_route_path("index.vue", false), "/");
        assert_eq!(nuxt_route_path("orders/[id].vue", false), "/orders/:id");
        assert_eq!(
            nuxt_route_path("orders/[id]/index.vue", false),
            "/orders/:id"
        );
        assert_eq!(
            nuxt_route_path("[[lang]]/about.vue", false),
            "/:lang?/about"
        );
        assert_eq!(
            nuxt_route_path("docs/[...slug].vue", false),
            "/docs/:slug(.*)*"
        );
        assert_eq!(
            nuxt_route_path("(marketing)/pricing.vue", false),
            "/pricing"
        );
        assert_eq!(
            nuxt_route_path("shop/item-[id].vue", false),
            "/shop/item-:id"
        );
        assert_eq!(nuxt_route_path("orders/_id.vue", true), "/orders/:id");
        assert_eq!(nuxt_route_path("_.vue", true), "/*");
        assert_eq!(string_list("['auth', 'admin']"), ["auth", "admin"]);
        assert_eq!(join("/admin", "users"), "/admin/users");
        assert_eq!(join("/admin/", ""), "/admin/");
        assert_eq!(join("/admin", "/x"), "/x");
    }
}
