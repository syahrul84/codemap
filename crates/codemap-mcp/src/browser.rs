//! Browser tests (Playwright, Cypress) tied to the screens they open, so a
//! change can name the browser tests worth running. Tests are read as text:
//! the URLs they visit (string literals starting with `/`, also in the page
//! objects they import) and the test ids they look for (`getByTestId('x')`,
//! `[data-testid="x"]`, `[data-cy=x]`).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

use codemap_core::Store;
use codemap_core::symbols;

use crate::tools::{Project, ToolError};
use crate::usages::is_test_path;

type R<T> = Result<T, ToolError>;

pub fn is_cypress(path: &str, src: &str) -> bool {
    path.contains("cypress/") || src.contains("cy.visit(") || src.contains("cy.get(")
}

/// A test that drives a browser (Playwright or Cypress).
pub fn is_browser_test(path: &str, src: &str) -> bool {
    // Runners only pick spec files (`*.spec.*`, `*.test.*`, Cypress `*.cy.*`);
    // page objects and helpers next to them are not tests.
    let name = path.rsplit('/').next().unwrap_or(path);
    let spec_file = name.contains(".spec.") || name.contains(".test.") || name.contains(".cy.");
    spec_file && is_test_path(path) && (src.contains("@playwright/test") || is_cypress(path, src))
}

/// String and template literals of a JS/TS source (`${…}` becomes `{}`),
/// outside comments.
fn literals(src: &str) -> Vec<String> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            q @ (b'\'' | b'"' | b'`') => {
                let mut s = Vec::new();
                i += 1;
                while i < b.len() && b[i] != q {
                    if b[i] == b'\\' && i + 1 < b.len() {
                        s.push(b[i + 1]);
                        i += 2;
                        continue;
                    }
                    if q == b'`' && b[i] == b'$' && b.get(i + 1) == Some(&b'{') {
                        let mut depth = 0;
                        while i < b.len() {
                            match b[i] {
                                b'{' => depth += 1,
                                b'}' => {
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                }
                                _ => {}
                            }
                            i += 1;
                        }
                        s.extend_from_slice(b"{}");
                        i += 1;
                        continue;
                    }
                    if q != b'`' && b[i] == b'\n' {
                        break;
                    }
                    s.push(b[i]);
                    i += 1;
                }
                i += 1;
                out.push(String::from_utf8_lossy(&s).into_owned());
            }
            _ => i += 1,
        }
    }
    out
}

/// `/malls/1?x=2` → `/malls/1`; only app paths (not API calls or assets).
fn as_visit(lit: &str) -> Option<String> {
    let l = lit.trim();
    if !l.starts_with('/') || l.starts_with("//") || l.len() > 150 {
        return None;
    }
    if l.contains(char::is_whitespace) || l.contains(['*', '\\', '<', '>', '(', ')']) {
        return None;
    }
    let path = l.split(['?', '#']).next().unwrap_or(l);
    let first = path.trim_start_matches('/').split('/').next().unwrap_or("");
    if first == "api" || first.starts_with('_') {
        return None;
    }
    let last = path.rsplit('/').next().unwrap_or("");
    if last.contains('.') && !last.contains("{}") {
        return None; // an asset or a file, not a screen
    }
    Some(path.to_owned())
}

/// Test ids a test looks for. A dynamic id keeps its fixed start plus `{}`
/// (`mall-{}`).
fn spec_test_ids(src: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let norm = |v: &str| -> Option<String> {
        let v = v.trim().trim_matches(['"', '\'']);
        let v = match v.split_once("{}") {
            Some((pre, _)) => format!("{pre}{{}}"),
            None => v.to_owned(),
        };
        (v.len() >= 2 && !v.contains(char::is_whitespace)).then_some(v)
    };
    let mut rest = src;
    while let Some(at) = rest.find("getByTestId(") {
        rest = &rest[at + "getByTestId(".len()..];
        if let Some(lit) = literals(rest.split(')').next().unwrap_or(""))
            .into_iter()
            .next()
            && let Some(id) = norm(&lit)
        {
            out.insert(id);
        }
    }
    for lit in literals(src) {
        for attr in ["data-testid=", "data-test=", "data-cy="] {
            let mut r = lit.as_str();
            while let Some(at) = r.find(attr) {
                r = &r[at + attr.len()..];
                let v: String = r
                    .chars()
                    .take_while(|c| !matches!(c, ']' | ' ' | ','))
                    .collect();
                if let Some(id) = norm(&v) {
                    out.insert(id);
                }
            }
        }
    }
    out
}

/// Test ids set in a component's template: `data-testid="x"`, and bound ones
/// with their fixed start (`:data-testid="`mall-${mall.id}`"` → `mall-{}`).
fn component_test_ids(src: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for attr in ["data-testid=", "data-test=", "data-cy="] {
        let mut rest = src;
        while let Some(at) = rest.find(attr) {
            let bound = rest[..at].ends_with(':') || rest[..at].ends_with("v-bind:");
            rest = &rest[at + attr.len()..];
            let Some(q) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
                continue;
            };
            let value: String = rest[1..].chars().take_while(|c| *c != q).collect();
            let id = if bound {
                let v = value.trim();
                match v.chars().next() {
                    Some('`') => match v[1..].split_once("${") {
                        Some((pre, _)) => format!("{pre}{{}}"),
                        None => v[1..].trim_end_matches('`').to_owned(),
                    },
                    Some('\'') | Some('"') => {
                        let quote = v.chars().next().unwrap_or('\'');
                        let lit = v[1..].split(quote).next().unwrap_or("");
                        // `'mall-' + mall.id`: a fixed start and a variable end.
                        if v[1 + lit.len()..]
                            .trim_start_matches(quote)
                            .trim_start()
                            .starts_with('+')
                        {
                            format!("{lit}{{}}")
                        } else {
                            lit.to_owned()
                        }
                    }
                    _ => String::new(),
                }
            } else {
                value
            };
            if id.len() >= 2 && !id.contains(char::is_whitespace) {
                out.insert(id);
            }
        }
    }
    out
}

/// Two test ids name the same element: equal, or a variable id (`mall-{}`)
/// against a fixed one whose rest is a number (`mall-12`) or also variable.
fn ids_match(a: &str, b: &str) -> bool {
    let var = |x: &str| x.split_once("{}").map(|(pre, _)| pre.to_owned());
    match (var(a), var(b)) {
        (None, None) => a == b,
        (Some(pa), Some(pb)) => pa == pb && !pa.is_empty(),
        (Some(p), None) => fixed_with_number(b, &p),
        (None, Some(p)) => fixed_with_number(a, &p),
    }
}

fn fixed_with_number(id: &str, prefix: &str) -> bool {
    !prefix.is_empty()
        && id
            .strip_prefix(prefix)
            .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

/// A route path (`/orders/:id`, `/:lang?/about`, `/docs/:slug(.*)*`, `/*`)
/// matches a visited path (`/orders/12`, `/orders/{}`).
fn route_matches(route: &str, url: &str) -> bool {
    fn go(r: &[&str], u: &[&str]) -> bool {
        match (r.split_first(), u.split_first()) {
            (None, None) => true,
            (Some((rs, rrest)), _) if *rs == "*" || rs.ends_with(")*") || rs.ends_with(")+") => {
                let _ = rrest;
                true
            }
            (Some((rs, rrest)), None) => rs.starts_with(':') && rs.ends_with('?') && go(rrest, u),
            (None, Some(_)) => false,
            (Some((rs, rrest)), Some((us, urest))) => {
                if rs.starts_with(':') {
                    let here = go(rrest, urest);
                    if rs.ends_with('?') {
                        here || go(rrest, u)
                    } else {
                        here
                    }
                } else if let Some((pre, _)) = rs.split_once(':') {
                    (*us == "{}" || us.starts_with(pre)) && go(rrest, urest)
                } else {
                    (*us == "{}" || us == rs) && go(rrest, urest)
                }
            }
        }
    }
    let r: Vec<&str> = route.split('/').filter(|s| !s.is_empty()).collect();
    let u: Vec<&str> = url.split('/').filter(|s| !s.is_empty()).collect();
    go(&r, &u)
}

/// What a browser test does: URLs it opens and test ids it looks for,
/// including those of the page objects it imports.
struct Spec {
    path: String,
    visits: BTreeSet<String>,
    ids: BTreeSet<String>,
}

/// A relative import that looks like a page object (`../pages/OutletPage`).
fn page_object_imports(dir: &str, src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in src.lines() {
        let l = line.trim();
        if !l.starts_with("import ") {
            continue;
        }
        let Some(spec) = literals(l).into_iter().last() else {
            continue;
        };
        if !spec.starts_with('.') {
            continue;
        }
        let base = spec.rsplit('/').next().unwrap_or(&spec);
        let looks = base.ends_with("Page")
            || base.ends_with("Screen")
            || base.ends_with(".page")
            || spec.contains("/pages/")
            || spec.contains("page-objects")
            || spec.contains("pageObjects");
        if looks {
            out.push(normalize_join(dir, &spec));
        }
    }
    out
}

fn normalize_join(dir: &str, spec: &str) -> String {
    let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in spec.split('/') {
        match seg {
            "." | "" => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn read_spec(root: &Path, files: &HashSet<String>, path: &str, src: &str) -> Spec {
    let mut visits = BTreeSet::new();
    let mut ids = spec_test_ids(src);
    for l in literals(src) {
        if let Some(v) = as_visit(&l) {
            visits.insert(v);
        }
    }
    // Page objects, two levels deep.
    let mut queue: VecDeque<(String, String, u8)> = VecDeque::new();
    let dir = path.rsplit_once('/').map_or("", |(d, _)| d).to_owned();
    for p in page_object_imports(&dir, src) {
        queue.push_back((p, dir.clone(), 1));
    }
    let mut seen = HashSet::new();
    while let Some((stem, _, depth)) = queue.pop_front() {
        let Some(file) = ["", ".ts", ".js", ".mjs", "/index.ts", "/index.js"]
            .iter()
            .map(|ext| format!("{stem}{ext}"))
            .find(|f| files.contains(f))
        else {
            continue;
        };
        if !seen.insert(file.clone()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(root.join(&file)) else {
            continue;
        };
        for l in literals(&text) {
            if let Some(v) = as_visit(&l) {
                visits.insert(v);
            }
        }
        ids.extend(spec_test_ids(&text));
        if depth < 2 {
            let d = file.rsplit_once('/').map_or("", |(d, _)| d).to_owned();
            for p in page_object_imports(&d, &text) {
                queue.push_back((p, d.clone(), depth + 1));
            }
        }
    }
    Spec {
        path: path.to_owned(),
        visits,
        ids,
    }
}

/// Browser tests for changed frontend files.
#[derive(Debug, Default)]
pub struct BrowserImpact {
    /// `(test file, why it is affected)`: tests that open a screen the
    /// changed code is close to (at most [`NEAR`] steps), or that look for a
    /// test id the changed code sets.
    pub tests: Vec<(String, String)>,
    /// The (at most 5) tests that use the change most directly, best first.
    pub top: Vec<(String, String)>,
    /// Tests that only open screens which include the change further down
    /// (e.g. a home page built from shared blocks): worth a run before release.
    pub wider: Vec<(String, String)>,
    /// Layouts / the app shell reached from the changed files: they wrap
    /// every screen, so any browser test may be affected.
    pub shells: Vec<String>,
    /// Screens the changed files are close to.
    pub screens: Vec<String>,
    /// Screens reached only further away or through shared building blocks.
    pub wider_screens: Vec<String>,
}

/// Steps from the changed file to a screen that still count as "close":
/// the screen itself (0), a component on it (1), or one level further (2).
pub const NEAR: u8 = 2;

pub fn for_files(store: &Store, project: &Project, changed: &[String]) -> R<BrowserImpact> {
    let mut out = BrowserImpact::default();
    let files: HashSet<String> = store.list_files()?.into_iter().map(|f| f.path).collect();
    let root = &project.root;
    let mut specs: Vec<Spec> = Vec::new();
    let mut paths: Vec<&String> = files.iter().filter(|f| is_test_path(f)).collect();
    paths.sort();
    for p in paths {
        if p.ends_with(".php") {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(root.join(p)) else {
            continue;
        };
        if is_browser_test(p, &src) {
            specs.push(read_spec(root, &files, p, &src));
        }
    }
    let changed: Vec<&String> = changed
        .iter()
        .filter(|f| !f.ends_with(".php") && !is_test_path(f) && files.contains(*f))
        .collect();
    if specs.is_empty() || changed.is_empty() {
        return Ok(out);
    }

    // Who uses each file: components rendering it, files importing it, and
    // files calling its exported functions without an import (auto-imports).
    let conn = store.conn();
    let mut users: HashMap<String, Vec<String>> = HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT from_path, to_path FROM component_uses WHERE to_path IS NOT NULL")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (from, to) = row?;
            users.entry(to).or_default().push(from);
        }
    }
    let mut callers_by_name: HashMap<String, Vec<String>> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT f.path, r.kind, r.target FROM refs r JOIN files f ON f.id = r.file_id
             WHERE r.kind IN ('import', 'js_call')",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (from, kind, target) = row?;
            if kind == "import" {
                let file = target.split('#').next().unwrap_or(&target).to_owned();
                users.entry(file).or_default().push(from);
            } else {
                callers_by_name.entry(target).or_default().push(from);
            }
        }
    }
    let mut names_by_file: HashMap<String, Vec<String>> = HashMap::new();
    for s in symbols::all(conn)? {
        if !s.file.ends_with(".php") {
            names_by_file.entry(s.file).or_default().push(s.name);
        }
    }
    let shells: HashSet<String> = {
        let mut stmt = conn.prepare("SELECT path FROM js_modules WHERE kind = 'layout'")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };

    let pages: Vec<(String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT route_path, file FROM pages WHERE file IS NOT NULL AND source IN ('nuxt', 'router')",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let page_files: HashSet<&str> = pages.iter().map(|(_, f)| f.as_str()).collect();
    let next_of = |f: &str| -> Vec<String> {
        let mut next: Vec<String> = users.get(f).cloned().unwrap_or_default();
        for name in names_by_file.get(f).into_iter().flatten() {
            next.extend(callers_by_name.get(name).cloned().unwrap_or_default());
        }
        next.retain(|n| !is_test_path(n));
        next.sort();
        next.dedup();
        next
    };
    // A shared building block (a block renderer used by many screens): what
    // reaches screens only through it is not "close" to them.
    let is_hub = |f: &str| {
        let next = next_of(f);
        next.iter()
            .filter(|n| page_files.contains(n.as_str()))
            .count()
            >= 5
            || next.len() >= 12
    };
    // Breadth first, so each file keeps its shortest distance.
    let walk = |stop_at_hubs: bool| -> HashMap<String, u8> {
        let mut reached: HashMap<String, u8> = HashMap::new();
        let mut queue: VecDeque<(String, u8)> = changed.iter().map(|f| ((*f).clone(), 0)).collect();
        while let Some((f, depth)) = queue.pop_front() {
            if reached.contains_key(&f) || reached.len() > 2000 {
                continue;
            }
            reached.insert(f.clone(), depth);
            if depth >= 10 || (stop_at_hubs && (depth >= NEAR || (depth > 0 && is_hub(&f)))) {
                continue;
            }
            for n in next_of(&f) {
                if !reached.contains_key(&n) {
                    queue.push_back((n, depth + 1));
                }
            }
        }
        reached
    };
    let reached = walk(false);
    let close = walk(true);
    let mut shell_hits: Vec<String> = reached
        .keys()
        .filter(|f| {
            shells.contains(*f)
                || f.ends_with("/app.vue")
                || *f == "app.vue"
                || f.ends_with("/App.vue")
        })
        .cloned()
        .collect();
    shell_hits.sort();
    out.shells = shell_hits;

    // Screens the change ends up in.
    // Screens the change ends up in: distance, or past NEAR when only reached
    // through a shared building block.
    let mut screens: BTreeMap<String, u8> = BTreeMap::new();
    for (route, file) in &pages {
        if route == "(dynamic)" {
            continue;
        }
        let d = match (close.get(file), reached.get(file)) {
            (Some(d), _) if *d <= NEAR => *d,
            (_, Some(d)) => (*d).max(NEAR + 1),
            _ => continue,
        };
        let e = screens.entry(route.clone()).or_insert(d);
        *e = (*e).min(d);
    }
    out.screens = screens
        .iter()
        .filter(|(_, d)| **d <= NEAR)
        .map(|(r, _)| r.clone())
        .collect();
    out.wider_screens = screens
        .iter()
        .filter(|(_, d)| **d > NEAR)
        .map(|(r, _)| r.clone())
        .collect();

    let mut ids: BTreeSet<String> = BTreeSet::new();
    for f in changed.iter().filter(|f| f.ends_with(".vue")) {
        if let Ok(src) = std::fs::read_to_string(root.join(f.as_str())) {
            ids.extend(component_test_ids(&src));
        }
    }

    // Words of the changed files' names (`stores/cart.ts` → cart,
    // `MallListCard.vue` → mall, list, card), to favour tests named after them.
    let change_words: BTreeSet<String> = changed
        .iter()
        .flat_map(|f| name_words(f.rsplit('/').next().unwrap_or(f)))
        .collect();
    let mut ranked: Vec<(i64, String, String)> = Vec::new();
    for spec in &specs {
        // The closest screen this test opens.
        let mut by_screen: Option<(u8, String)> = None;
        let mut close_screens: BTreeSet<&String> = BTreeSet::new();
        // Most tests open `/` only as a starting point: it counts for tests
        // that open nothing else.
        let only_home = spec.visits.iter().all(|v| v == "/");
        for v in spec.visits.iter().filter(|v| *v != "/" || only_home) {
            for (route, d) in &screens {
                if route_matches(route, v) {
                    if *d <= NEAR {
                        close_screens.insert(route);
                    }
                    if by_screen.as_ref().is_none_or(|(bd, _)| d < bd) {
                        by_screen = Some((*d, format!("opens {v} (screen {route})")));
                    }
                }
            }
        }
        let by_id = || {
            spec.ids.iter().find_map(|sid| {
                ids.iter().find(|cid| ids_match(sid, cid)).map(|cid| {
                    format!("looks for test id {sid} (set in the changed code as {cid})")
                })
            })
        };
        // How directly the test exercises the change: a test id it sets, the
        // screen itself or a component on it, tests named after the change;
        // tests that open many screens (smoke tests) count for less.
        let id_hit = by_id();
        let named: usize = name_words(spec.path.rsplit('/').next().unwrap_or(&spec.path))
            .iter()
            .filter(|w| change_words.contains(*w))
            .count();
        let mut score: i64 = match (&id_hit, &by_screen) {
            (Some(_), _) => 100,
            (None, Some((d, _))) if *d <= NEAR => 60 - 15 * i64::from(*d),
            _ => 0,
        };
        if score > 0 {
            score += 8 * (close_screens.len().min(4) as i64 - 1).max(0);
            score += 30 * named.min(2) as i64;
            score -= (spec.visits.len() as i64 - 4).max(0) * 3;
            let why = id_hit
                .clone()
                .or_else(|| by_screen.as_ref().map(|(_, w)| w.clone()))
                .unwrap_or_default();
            ranked.push((score, spec.path.clone(), why));
        }
        match by_screen {
            Some((d, why)) if d <= NEAR => out.tests.push((spec.path.clone(), why)),
            near => match id_hit {
                Some(why) => out.tests.push((spec.path.clone(), why)),
                None => {
                    if let Some((d, why)) = near {
                        out.wider.push((
                            spec.path.clone(),
                            format!("{why}, {d} steps from the change"),
                        ));
                    }
                }
            },
        }
    }
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    out.top = ranked
        .into_iter()
        .take(5)
        .map(|(_, t, why)| (t, why))
        .collect();
    Ok(out)
}

/// Lowercased words of a file name (`commerce-cart-checkout.spec.ts` →
/// commerce, cart, checkout), without short or generic ones.
fn name_words(name: &str) -> BTreeSet<String> {
    const GENERIC: &[&str] = &[
        "spec",
        "test",
        "index",
        "vue",
        "ts",
        "js",
        "tsx",
        "page",
        "component",
        "store",
        "use",
        "api",
        "the",
        "and",
        "commerce",
    ];
    let mut out = BTreeSet::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            if c.is_ascii_uppercase() && prev_lower && !cur.is_empty() {
                out.insert(std::mem::take(&mut cur));
            }
            prev_lower = c.is_ascii_lowercase();
            cur.push(c.to_ascii_lowercase());
        } else {
            if !cur.is_empty() {
                out.insert(std::mem::take(&mut cur));
            }
            prev_lower = false;
        }
    }
    if !cur.is_empty() {
        out.insert(cur);
    }
    out.retain(|w| w.len() >= 3 && !GENERIC.contains(&w.as_str()));
    // `carts` and `cart` are the same word.
    out.into_iter()
        .map(|w| {
            w.strip_suffix('s')
                .filter(|x| x.len() >= 3)
                .map_or(w.clone(), str::to_owned)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_name_words() {
        let w: Vec<String> = name_words("commerce-cart-checkout.spec.ts")
            .into_iter()
            .collect();
        assert_eq!(w, ["cart", "checkout"]);
        let w: Vec<String> = name_words("MallListCard.vue").into_iter().collect();
        assert_eq!(w, ["card", "list", "mall"]);
        let w: Vec<String> = name_words("carts.ts").into_iter().collect();
        assert_eq!(w, ["cart"]);
    }

    #[test]
    fn routes_match_visits() {
        assert!(route_matches("/malls/:id", "/malls/1"));
        assert!(route_matches("/malls/:id", "/malls/{}"));
        assert!(!route_matches("/malls/:id", "/malls"));
        assert!(route_matches("/:lang?/about", "/about"));
        assert!(route_matches("/:lang?/about", "/ms/about"));
        assert!(route_matches("/docs/:slug(.*)*", "/docs/a/b"));
        assert!(route_matches("/shop/item-:id", "/shop/item-12"));
        assert!(route_matches("/", "/"));
        assert!(!route_matches("/", "/cart"));
    }

    #[test]
    fn reads_tests_and_templates() {
        let spec = "import { test } from '@playwright/test'\n// page.goto('/old')\ntest('x', async ({ page }) => {\n  await page.goto(`/outlets/${id}?tab=menu`)\n  await page.goto('/api/v1/x')\n  await page.getByTestId(`mall-fav-${id}`).click()\n  await page.locator('[data-testid=\"cart-line\"]').click()\n})";
        let visits: Vec<String> = literals(spec).iter().filter_map(|l| as_visit(l)).collect();
        assert_eq!(visits, ["/outlets/{}"]);
        let ids: Vec<String> = spec_test_ids(spec).into_iter().collect();
        assert_eq!(ids, ["cart-line", "mall-fav-{}"]);
        let tpl = "<div data-testid=\"mall-card\"><button :data-testid=\"`mall-fav-${mall.id}`\" /><i :data-testid=\"'logo-' + id\" /></div>";
        let ids: Vec<String> = component_test_ids(tpl).into_iter().collect();
        assert_eq!(ids, ["logo-{}", "mall-card", "mall-fav-{}"]);
        assert!(ids_match("mall-fav-{}", "mall-fav-{}"));
        assert!(ids_match("mall-12", "mall-{}"));
        assert!(!ids_match("mall-branches", "mall-{}"));
        assert!(!ids_match("mall-card", "mall-cards"));
    }
}
