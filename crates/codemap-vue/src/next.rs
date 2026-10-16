//! Next.js routing from the file system: app router pages and layouts
//! (`app/(site)/[lang]/rooms/[slug]/page.tsx` → `/:lang/rooms/:slug`), pages
//! router pages (`pages/rooms/[id].tsx`), and API route handlers
//! (`app/api/availability/[slug]/route.ts` exporting `GET` →
//! `GET api/availability/{slug}`; `pages/api/**`). Route groups `(x)` are
//! left out of URLs; private `_folders`, parallel `@slots` and intercepting
//! `(.)` routes are not URLs of their own.

use crate::detect::VueDetection;
use crate::pages::PageRoute;
use crate::registry::Module;

const PAGE_EXT: &[&str] = &["tsx", "jsx", "ts", "js", "mdx"];
const METHODS: &[&str] = &["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

/// `dir/name.ext` under `base/`: (dir segments, file stem).
fn split<'p>(path: &'p str, base: &str) -> Option<(Vec<&'p str>, &'p str)> {
    let rest = path.strip_prefix(base)?.strip_prefix('/')?;
    let (dir, file) = rest.rsplit_once('/').unwrap_or(("", rest));
    let (stem, ext) = file.rsplit_once('.')?;
    if !PAGE_EXT.contains(&ext) {
        return None;
    }
    let segs = dir.split('/').filter(|s| !s.is_empty()).collect();
    Some((segs, stem))
}

/// One folder name as a URL segment: `None` = not part of the URL (a group),
/// `Err` = the folder is not routable.
fn segment(seg: &str, laravel_style: bool) -> Result<Option<String>, ()> {
    if seg.starts_with('(') && seg.ends_with(')') && !seg.starts_with("(.") {
        return Ok(None); // route group
    }
    if seg.starts_with('_') || seg.starts_with('@') || seg.starts_with("(.") {
        return Err(()); // private folder, parallel slot, intercepting route
    }
    let param = |name: &str, kind: &str| -> String {
        if laravel_style {
            if kind == "opt" {
                format!("{{{name}?}}")
            } else {
                format!("{{{name}}}")
            }
        } else {
            match kind {
                "opt" => format!(":{name}(.*)*"),
                "all" => format!(":{name}(.*)+"),
                _ => format!(":{name}"),
            }
        }
    };
    if let Some(inner) = seg.strip_prefix("[[...").and_then(|s| s.strip_suffix("]]")) {
        return Ok(Some(param(inner, "opt")));
    }
    if let Some(inner) = seg.strip_prefix("[...").and_then(|s| s.strip_suffix(']')) {
        return Ok(Some(param(inner, "all")));
    }
    if let Some(inner) = seg.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        return Ok(Some(param(inner, "one")));
    }
    Ok(Some(seg.to_owned()))
}

fn url(segs: &[&str], laravel_style: bool) -> Option<String> {
    let mut out = Vec::new();
    for s in segs {
        match segment(s, laravel_style) {
            Ok(Some(x)) => out.push(x),
            Ok(None) => {}
            Err(()) => return None,
        }
    }
    Some(out.join("/"))
}

/// `/:lang/rooms/:slug` → `lang-rooms-slug` (root: `index`).
pub fn page_name(path: &str) -> String {
    route_name(path)
}

fn route_name(path: &str) -> String {
    let name: Vec<String> = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.trim_start_matches(':')
                .split('(')
                .next()
                .unwrap_or(s)
                .to_owned()
        })
        .collect();
    if name.is_empty() {
        "index".into()
    } else {
        name.join("-")
    }
}

/// The app router screen a file is (`page.tsx`), as a URL.
pub fn app_page_url(app_dir: &str, path: &str) -> Option<String> {
    let (segs, stem) = split(path, app_dir)?;
    (stem == "page")
        .then(|| url(&segs, false))
        .flatten()
        .map(|u| format!("/{u}"))
}

/// A pages router screen (`pages/rooms/[id].tsx`), as a URL; API files are not screens.
pub fn pages_router_url(pages_dir: &str, path: &str) -> Option<String> {
    let (mut segs, stem) = split(path, pages_dir)?;
    if segs.first() == Some(&"api") || stem.starts_with('_') {
        return None;
    }
    if stem != "index" {
        segs.push(stem);
    }
    url(&segs, false).map(|u| format!("/{u}"))
}

/// Is this file a Next.js page (either router)?
pub fn page_url(d: &VueDetection, path: &str) -> Option<String> {
    let n = d.next.as_ref()?;
    n.app_dir
        .as_deref()
        .and_then(|a| app_page_url(a, path))
        .or_else(|| {
            n.pages_dir
                .as_deref()
                .and_then(|p| pages_router_url(p, path))
        })
}

/// Is this file an app router layout? Returns the URL it wraps.
pub fn layout_url(d: &VueDetection, path: &str) -> Option<String> {
    let app = d.next.as_ref()?.app_dir.as_deref()?;
    let (segs, stem) = split(path, app)?;
    (stem == "layout")
        .then(|| url(&segs, false))
        .flatten()
        .map(|u| format!("/{u}"))
}

/// A route handler: `(methods, uri in Laravel style, file)`.
#[derive(Debug, Clone)]
pub struct Handler {
    pub methods: Vec<String>,
    pub uri: String,
    pub file: String,
    pub line: u32,
}

/// Route handlers among `modules`: app router `route.ts` exports and
/// pages router `pages/api/**` (any method).
pub fn handlers(d: &VueDetection, modules: &[Module]) -> Vec<Handler> {
    let Some(n) = &d.next else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for m in modules {
        if let Some(app) = n.app_dir.as_deref()
            && let Some((segs, stem)) = split(&m.path, app)
            && stem == "route"
            && let Some(uri) = url(&segs, true)
        {
            let mut methods: Vec<String> = Vec::new();
            let mut line = 1;
            for decl in &m.facts.script.decls {
                if decl.exported && METHODS.contains(&decl.name.as_str()) {
                    methods.push(decl.name.clone());
                    if line == 1 {
                        line = decl.line;
                    }
                }
            }
            for r in &m.facts.script.reexports {
                if METHODS.contains(&r.exported.as_str()) && !methods.contains(&r.exported) {
                    methods.push(r.exported.clone());
                }
            }
            if methods.contains(&"GET".to_owned()) && !methods.contains(&"HEAD".to_owned()) {
                methods.insert(
                    methods.iter().position(|x| x == "GET").map_or(0, |i| i + 1),
                    "HEAD".into(),
                );
            }
            if methods.is_empty() {
                continue;
            }
            out.push(Handler {
                methods,
                uri: if uri.is_empty() { "/".into() } else { uri },
                file: m.path.clone(),
                line,
            });
            continue;
        }
        if let Some(pages) = n.pages_dir.as_deref()
            && let Some((segs, stem)) = split(&m.path, pages)
            && segs.first() == Some(&"api")
        {
            let mut segs = segs.clone();
            if stem != "index" {
                segs.push(stem);
            }
            if let Some(uri) = url(&segs, true) {
                out.push(Handler {
                    methods: vec!["ANY".into()],
                    uri,
                    file: m.path.clone(),
                    line: 1,
                });
            }
        }
    }
    out.sort_by(|a, b| a.uri.cmp(&b.uri));
    out
}

/// Screens of a Next.js app, with the nearest layout of app router pages.
pub fn pages(d: &VueDetection, modules: &[Module]) -> Vec<PageRoute> {
    let Some(n) = &d.next else {
        return Vec::new();
    };
    let layouts: Vec<(&str, String)> = modules
        .iter()
        .filter_map(|m| layout_url(d, &m.path).map(|_| m.path.as_str()))
        .map(|p| {
            (
                p,
                p.rsplit_once('/')
                    .map_or(String::new(), |(dir, _)| dir.to_owned()),
            )
        })
        .collect();
    let mut out = Vec::new();
    for m in modules {
        let Some(path) = page_url(d, &m.path) else {
            continue;
        };
        // The deepest layout whose folder contains the page.
        let dir = m.path.rsplit_once('/').map_or("", |(dir, _)| dir);
        let layout = n.app_dir.as_ref().and_then(|_| {
            layouts
                .iter()
                .filter(|(_, ldir)| dir == ldir.as_str() || dir.starts_with(&format!("{ldir}/")))
                .max_by_key(|(_, ldir)| ldir.len())
                .map(|(lp, _)| (*lp).to_owned())
        });
        out.push(PageRoute {
            name: Some(route_name(&path)),
            path,
            file: Some(m.path.clone()),
            source: "next",
            defined_in: format!("{}:1", m.path),
            layout,
            ..PageRoute::default()
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_router_urls() {
        assert_eq!(app_page_url("app", "app/page.tsx").as_deref(), Some("/"));
        assert_eq!(
            app_page_url("app", "app/(site)/[lang]/rooms/[slug]/page.tsx").as_deref(),
            Some("/:lang/rooms/:slug")
        );
        assert_eq!(
            app_page_url("app", "app/docs/[...slug]/page.tsx").as_deref(),
            Some("/docs/:slug(.*)+")
        );
        assert_eq!(
            app_page_url("app", "app/shop/[[...tags]]/page.tsx").as_deref(),
            Some("/shop/:tags(.*)*")
        );
        assert_eq!(app_page_url("app", "app/_lib/page.tsx"), None);
        assert_eq!(app_page_url("app", "app/@modal/photo/page.tsx"), None);
        assert_eq!(app_page_url("app", "app/(.)photo/[id]/page.tsx"), None);
        assert_eq!(app_page_url("app", "app/rooms/layout.tsx"), None);
        assert_eq!(route_name("/:lang/rooms/:slug"), "lang-rooms-slug");
    }

    #[test]
    fn pages_router_urls() {
        assert_eq!(
            pages_router_url("pages", "pages/index.tsx").as_deref(),
            Some("/")
        );
        assert_eq!(
            pages_router_url("pages", "pages/rooms/[id].tsx").as_deref(),
            Some("/rooms/:id")
        );
        assert_eq!(pages_router_url("pages", "pages/_app.tsx"), None);
        assert_eq!(pages_router_url("pages", "pages/api/rooms.ts"), None);
    }
}
