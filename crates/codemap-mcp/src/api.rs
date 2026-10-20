//! Frontend ↔ backend: matches the HTTP calls found in Vue / Nuxt code to
//! Laravel routes (this project's, or a linked backend's), infers the API base
//! path by voting, and compares the fields the frontend reads with the fields
//! the endpoint returns.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use codemap_core::envelope::Detail;
use codemap_core::{Config, ScanOptions, Store, paths};
use codemap_laravel::responses::{self, ResponseShape};
use codemap_laravel::routes::{self, RouteDef};
use codemap_vue::api::CallRow;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::tools::{DetailParam, Project, ToolError};

type R<T> = Result<T, ToolError>;

/// A backend whose routes calls are matched against.
pub struct Backend {
    pub name: String,
    pub root: PathBuf,
    pub routes: Vec<RouteDef>,
    /// `None`: this project's own store.
    pub store: Option<Store>,
    pub configured_base: Option<Vec<String>>,
}

impl Backend {
    fn conn<'a>(&'a self, own: &'a Store) -> &'a rusqlite::Connection {
        self.store.as_ref().unwrap_or(own).conn()
    }
}

/// This project's routes (Laravel + Vue in one repository) and every linked
/// backend. Linked indexes are refreshed first (they live in the shared data
/// dir); the linked project itself is only read.
pub fn backends(store: &Store, project: &Project) -> R<(Vec<Backend>, Vec<String>)> {
    let mut out = Vec::new();
    let mut warnings = Vec::new();
    let own = routes::all(store.conn())?;
    if !own.is_empty() {
        out.push(Backend {
            name: project.name(store),
            root: project.root.clone(),
            routes: own,
            store: None,
            configured_base: None,
        });
    }
    let data_dir = project
        .index_path
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    for link in &project.config.links {
        let root = if link.path.is_absolute() {
            link.path.clone()
        } else {
            project.root.join(&link.path)
        };
        let root = match root.canonicalize() {
            Ok(r) if r.is_dir() => r,
            _ => {
                warnings.push(format!(
                    "linked project `{}` not found at {} (in Docker, mount it, e.g. -v ../backend:/links/backend:ro)",
                    link.name,
                    root.display()
                ));
                continue;
            }
        };
        let Some(data_dir) = &data_dir else { continue };
        let opened = (|| -> codemap_core::Result<Store> {
            let index = match &link.index_path {
                Some(p) => p.clone(),
                None => paths::index_path(data_dir, &paths::resolve_project_id(None, &root)?),
            };
            let mut s = Store::open(&index)?;
            // Re-check the linked project at most every LINK_TTL (it is a
            // whole scan of another repository). Keyed by index: the same
            // repository can have an index in another data dir.
            let due = {
                let mut seen = last_link_refresh()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let due = seen.get(&index).is_none_or(|t| t.elapsed() >= LINK_TTL);
                if due {
                    seen.insert(index.clone(), std::time::Instant::now());
                }
                due
            };
            if due {
                let config = Config::load(&root)?;
                let started = std::time::Instant::now();
                let r = crate::refresh::refresh(&mut s, &root, &config, ScanOptions::default());
                crate::usage::record_index_at(&index, "linked", false, started, r.as_ref().ok());
                r?;
            }
            Ok(s)
        })();
        match opened {
            Ok(s) => {
                let r = routes::all(s.conn())?;
                if r.is_empty() {
                    warnings.push(format!(
                        "linked project `{}` has no Laravel routes",
                        link.name
                    ));
                }
                out.push(Backend {
                    name: link.name.clone(),
                    root,
                    routes: r,
                    store: Some(s),
                    configured_base: link.api_base.as_deref().map(segments),
                });
            }
            Err(e) => warnings.push(format!(
                "linked project `{}` could not be indexed: {e}",
                link.name
            )),
        }
    }
    Ok((out, warnings))
}

/// How long a linked project's index is trusted before it is re-checked.
const LINK_TTL: std::time::Duration = std::time::Duration::from_secs(30);

fn last_link_refresh() -> &'static std::sync::Mutex<HashMap<PathBuf, std::time::Instant>> {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, std::time::Instant>>> =
        std::sync::OnceLock::new();
    SEEN.get_or_init(Default::default)
}

fn segments(s: &str) -> Vec<String> {
    let s = s.split("://").last().unwrap_or(s);
    let s = if s.contains('/')
        && !s.starts_with('/')
        && s.split('/')
            .next()
            .is_some_and(|h| h.contains('.') || h.contains(':'))
    {
        // `localhost:8000/api/v1` → `api/v1`
        s.split_once('/').map_or("", |(_, r)| r)
    } else {
        s
    };
    s.split('/')
        .filter(|x| !x.is_empty())
        .map(str::to_owned)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Seg {
    Lit(String),
    Param,
    Opt,
}

pub(crate) fn route_segs(uri: &str) -> Vec<Seg> {
    uri.split('/')
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.starts_with('{') && s.ends_with("?}") {
                Seg::Opt
            } else if s.starts_with('{') {
                Seg::Param
            } else {
                Seg::Lit(s.to_owned())
            }
        })
        .collect()
}

/// `duplicate-{}` / `{}fixed-asset-schedule` against a literal route segment.
/// Each variable stands for one word at most: `{}-permit` fits `work-permit`
/// but not `spfm-work-permit` (that would take two words).
fn pattern_fits(pattern: &str, lit: &str) -> bool {
    let parts: Vec<&str> = pattern.split("{}").collect();
    let one_word = |fill: &str| !fill.trim_matches(['-', '_', '.']).contains(['-', '_', '.']);
    let mut rest = lit;
    for (i, p) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(p) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.len() >= p.len()
                && rest.ends_with(p)
                && one_word(&rest[..rest.len() - p.len()]);
        } else {
            match rest.find(p) {
                Some(at) if one_word(&rest[..at]) => rest = &rest[at + p.len()..],
                _ => return false,
            }
        }
    }
    rest.is_empty()
}

/// Literal segments that matched, when `call` (with `prefix`) fits `route`.
pub(crate) fn fit(call: &[&str], route: &[Seg]) -> Option<usize> {
    fit_detail(call, route).map(|(lits, _)| lits)
}

/// `(literal segments matched, variable segments that landed on a route
/// parameter)`: `/orders/{}` fits `orders/{order}` better than `orders/export`.
///
/// A fit that only works by crossing wildcards is no fit: the call's variable
/// lands on a word of the route while the route's parameter lands on a word
/// of the call (`/widgets/{}/archive` against `widgets/history/{widgetId}`).
fn fit_detail(call: &[&str], route: &[Seg]) -> Option<(usize, usize)> {
    fit_walk(call, route, false)
        .filter(|f| !(f.var_on_word && f.word_on_param))
        .map(|f| (f.lits, f.params))
}

#[derive(Clone, Copy, Default)]
struct Fit {
    lits: usize,
    params: usize,
    /// A call variable after the first matched word, on a route word.
    var_on_word: bool,
    /// A call word (not a number or id) on a route parameter.
    word_on_param: bool,
}

/// A call segment that names something (`archive`), not a value (`5`, an id).
fn is_word(seg: &str) -> bool {
    seg.chars().any(|c| c.is_ascii_alphabetic())
        && !seg.chars().any(|c| c.is_ascii_digit())
        && !seg.contains("{}")
}

fn fit_walk(call: &[&str], route: &[Seg], matched: bool) -> Option<Fit> {
    match (call.split_first(), route.split_first()) {
        (None, None) => Some(Fit::default()),
        (None, Some((Seg::Opt, rest))) => fit_walk(call, rest, matched),
        (None, Some(_)) | (Some(_), None) => None,
        (Some((c, crest)), Some((r, rrest))) => {
            let on_param = |mut f: Fit| {
                f.params += usize::from(*c == "{}");
                f.word_on_param |= is_word(c);
                f
            };
            match r {
                Seg::Opt => fit_walk(crest, rrest, matched)
                    .map(on_param)
                    .or_else(|| fit_walk(call, rrest, matched)),
                Seg::Param => fit_walk(crest, rrest, matched).map(on_param),
                Seg::Lit(l) => {
                    if *c == "{}" || (c.contains("{}") && pattern_fits(c, l)) {
                        fit_walk(crest, rrest, matched).map(|mut f| {
                            f.var_on_word |= matched && *c == "{}";
                            f
                        })
                    } else if c == l {
                        fit_walk(crest, rrest, true).map(|mut f| {
                            f.lits += 1;
                            f
                        })
                    } else {
                        None
                    }
                }
            }
        }
    }
}

/// `POST|PATCH` (a body that spoofs its method on some paths) fits a route
/// with either method.
/// A URL that names a placeholder, not a path: `/_api_url_/{}`, `/unitURL`,
/// `/admin/data-setup/_moduleUrl/{}` (filled in by a scaffold or at run time).
fn is_placeholder(url: &str) -> bool {
    url.split('/').any(|seg| {
        !seg.contains("{}")
            && seg.len() > 2
            && ((seg.starts_with('_') && seg.ends_with('_'))
                || seg.starts_with('_') && seg.to_ascii_lowercase().contains("url")
                || (!seg.contains('-')
                    && seg.chars().any(|c| c.is_ascii_uppercase())
                    && seg.to_ascii_lowercase().ends_with("url"))
                || (seg.contains('_') && !seg.contains('-') && seg.ends_with("_url")))
    })
}

/// A call whose URL is one of a few known full URLs.
fn expandable(c: &CallRow) -> bool {
    !c.variants.is_empty() || (c.base_var && !c.prefixes.is_empty())
}

/// Those full URLs, as calls of their own.
fn full_urls(c: &CallRow) -> Vec<CallRow> {
    let urls: Vec<String> = if !c.variants.is_empty() {
        c.variants.clone()
    } else {
        c.prefixes
            .iter()
            .map(|p| format!("/{}/{}", p.trim_matches('/'), c.url.trim_start_matches('/')))
            .collect()
    };
    urls.into_iter()
        .map(|url| CallRow {
            url,
            base_var: false,
            prefixes: Vec::new(),
            variants: Vec::new(),
            ..c.clone()
        })
        .collect()
}

fn method_ok(call: Option<&str>, route: &RouteDef) -> bool {
    match call {
        None => true,
        Some(ms) => ms.split('|').any(|m| {
            route
                .methods
                .iter()
                .any(|r| r == m || r == "ANY" || (m == "GET" && r == "HEAD"))
        }),
    }
}

#[derive(Debug, Clone)]
pub enum Outcome {
    /// `(backend, route index)`, best first.
    Matched(Vec<(usize, usize)>),
    /// The path exists with other methods.
    MethodMismatch(Vec<(usize, usize)>),
    NotFound(Vec<(usize, usize)>),
    External,
    /// The URL starts with a variable path, and its tail fits several routes
    /// (or none): it can't be pinned down statically.
    Unresolved(Vec<(usize, usize)>),
}

/// A route's first and last word for one call length (`None`: a parameter,
/// or not fixed because the route has optional segments).
type Ends = (Option<String>, Option<String>);

/// Routes of one call length, by their first and last words.
#[derive(Default)]
struct LenBucket {
    both: HashMap<Ends, Vec<usize>>,
    by_first: HashMap<Option<String>, Vec<usize>>,
    by_last: HashMap<Option<String>, Vec<usize>>,
    all: Vec<usize>,
}

/// Routes of one backend by the call lengths they can take, then by their
/// first and last words, so a call is only tried against routes that could
/// fit it (a literal word must match exactly; a variable fits anything).
#[derive(Default)]
struct RouteIndex {
    by_len: Vec<LenBucket>,
}

impl RouteIndex {
    fn new(segs: &[Vec<Seg>]) -> Self {
        let mut idx = RouteIndex::default();
        for (ri, r) in segs.iter().enumerate() {
            let max = r.len();
            let has_opt = r.iter().any(|s| matches!(s, Seg::Opt));
            let min = r.iter().filter(|s| !matches!(s, Seg::Opt)).count();
            if idx.by_len.len() <= max {
                idx.by_len.resize_with(max + 1, Default::default);
            }
            let word = |s: Option<&Seg>| match s {
                Some(Seg::Lit(w)) => Some(w.clone()),
                _ => None,
            };
            for len in min..=max {
                // A literal segment is never skipped, so a route without
                // optional segments lines up word for word with the call.
                let first = word(r.first()).filter(|_| !matches!(r.first(), Some(Seg::Opt)));
                let last = if has_opt { None } else { word(r.last()) };
                let b = &mut idx.by_len[len];
                b.both
                    .entry((first.clone(), last.clone()))
                    .or_default()
                    .push(ri);
                b.by_first.entry(first).or_default().push(ri);
                b.by_last.entry(last).or_default().push(ri);
                b.all.push(ri);
            }
        }
        idx
    }

    /// Routes that may fit `call`, in route order.
    fn candidates(&self, call: &[&str]) -> Vec<usize> {
        let Some(b) = self.by_len.get(call.len()) else {
            return Vec::new();
        };
        let known = |s: Option<&&str>| s.filter(|w| !w.contains("{}")).map(|w| (*w).to_owned());
        let (f, l) = (known(call.first()), known(call.last()));
        fn get<K: std::hash::Hash + Eq>(m: &HashMap<K, Vec<usize>>, k: K) -> Vec<usize> {
            m.get(&k).cloned().unwrap_or_default()
        }
        let mut out: Vec<usize> = match (&f, &l) {
            (Some(f), Some(l)) => [
                (Some(f.clone()), Some(l.clone())),
                (Some(f.clone()), None),
                (None, Some(l.clone())),
                (None, None),
            ]
            .into_iter()
            .flat_map(|k| get(&b.both, k))
            .collect(),
            (Some(f), None) => [Some(f.clone()), None]
                .into_iter()
                .flat_map(|k| get(&b.by_first, k))
                .collect(),
            (None, Some(l)) => [Some(l.clone()), None]
                .into_iter()
                .flat_map(|k| get(&b.by_last, k))
                .collect(),
            (None, None) => b.all.clone(),
        };
        out.sort_unstable();
        out.dedup();
        out
    }
}

pub struct Matcher<'b> {
    pub backends: &'b [Backend],
    segs: Vec<Vec<Vec<Seg>>>,
    index: Vec<RouteIndex>,
    /// Per backend: the distinct route words, and each route's words as
    /// positions in that list (for "closest route" suggestions).
    words: Vec<(Vec<String>, Vec<Vec<u32>>)>,
    /// Per backend: the inferred base and how many calls it explained.
    pub bases: Vec<(Vec<String>, usize, usize)>,
}

impl<'b> Matcher<'b> {
    pub fn new(backends: &'b [Backend], calls: &[CallRow]) -> Self {
        let segs: Vec<Vec<Vec<Seg>>> = backends
            .iter()
            .map(|b| b.routes.iter().map(|r| route_segs(&r.uri)).collect())
            .collect();
        let index = segs.iter().map(|s| RouteIndex::new(s)).collect();
        let words = segs
            .iter()
            .map(|routes| {
                let mut at: HashMap<&str, u32> = HashMap::new();
                let mut list: Vec<String> = Vec::new();
                let per_route = routes
                    .iter()
                    .map(|r| {
                        r.iter()
                            .filter_map(|s| match s {
                                Seg::Lit(x) => Some(*at.entry(x.as_str()).or_insert_with(|| {
                                    list.push(x.clone());
                                    (list.len() - 1) as u32
                                })),
                                _ => None,
                            })
                            .collect()
                    })
                    .collect();
                (list, per_route)
            })
            .collect();
        let mut m = Self {
            backends,
            segs,
            index,
            words,
            bases: Vec::new(),
        };
        // Calls with a known variable prefix vote with their full URLs, so a
        // frontend whose calls all start with such a prefix still finds its base.
        let expanded: Vec<CallRow> = calls
            .iter()
            .filter(|c| !c.external && expandable(c))
            .flat_map(full_urls)
            .collect();
        let internal: Vec<&CallRow> = calls
            .iter()
            .filter(|c| !c.external && !expandable(c))
            .chain(expanded.iter())
            .collect();
        // Voting on a sample keeps large frontends (thousands of calls) quick.
        let step = internal.len().div_ceil(600).max(1);
        let sample: Vec<&CallRow> = internal.iter().step_by(step).copied().collect();
        for (bi, b) in backends.iter().enumerate() {
            if let Some(base) = &b.configured_base {
                let n = internal.iter().filter(|c| m.fits_any(bi, base, c)).count();
                m.bases.push((base.clone(), n, internal.len()));
                continue;
            }
            // Candidate bases: literal prefixes (up to 3 segments) shared by a
            // fair share of the routes; an API base prefixes many of them.
            let mut counts: HashMap<Vec<String>, usize> = HashMap::new();
            for r in &m.segs[bi] {
                let mut prefix = Vec::new();
                for s in r.iter().take(3) {
                    match s {
                        Seg::Lit(l) => {
                            prefix.push(l.clone());
                            *counts.entry(prefix.clone()).or_default() += 1;
                        }
                        _ => break,
                    }
                }
            }
            let threshold = (m.segs[bi].len() / 20).max(2);
            let mut ranked: Vec<(Vec<String>, usize)> = counts
                .into_iter()
                .filter(|(_, n)| *n >= threshold)
                .collect();
            ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let mut candidates: Vec<Vec<String>> = vec![Vec::new()];
            candidates.extend(ranked.into_iter().take(16).map(|(p, _)| p));
            let mut best = (Vec::new(), 0usize);
            for cand in candidates {
                let n = sample.iter().filter(|c| m.fits_any(bi, &cand, c)).count();
                if n > best.1 || (n == best.1 && cand.len() < best.0.len()) {
                    best = (cand, n);
                }
            }
            let explained = internal
                .iter()
                .filter(|c| m.fits_any(bi, &best.0, c))
                .count();
            m.bases.push((best.0, explained, internal.len()));
        }
        // A call counts for a backend only when no backend that explains more
        // calls fits it too: generic routes of a CMS linked next to the API
        // must not look like calls to the CMS.
        if m.backends.len() > 1 {
            let order: Vec<usize> = {
                let mut o: Vec<usize> = (0..m.bases.len()).collect();
                o.sort_by(|a, b| m.bases[*b].1.cmp(&m.bases[*a].1));
                o
            };
            let mut own = vec![0usize; m.bases.len()];
            for c in &internal {
                if let Some(bi) = order
                    .iter()
                    .copied()
                    .find(|bi| m.fits_any(*bi, &m.bases[*bi].0, c))
                {
                    own[bi] += 1;
                }
            }
            for (bi, n) in own.into_iter().enumerate() {
                m.bases[bi].1 = n;
            }
        }
        m
    }

    /// The call fits at least one route of backend `bi` with `prefix`.
    fn fits_any(&self, bi: usize, prefix: &[String], c: &CallRow) -> bool {
        let call: Vec<&str> = prefix
            .iter()
            .map(String::as_str)
            .chain(c.url.split('/').filter(|s| !s.is_empty()))
            .collect();
        self.index[bi]
            .candidates(&call)
            .into_iter()
            .any(|ri| fit(&call, &self.segs[bi][ri]).is_some())
    }

    /// Routes of backend `bi` the call fits with `prefix`, ignoring the method.
    /// Best fits first: most literal segments, then variables on parameters.
    fn raw_matches(
        &self,
        bi: usize,
        prefix: &[String],
        c: &CallRow,
    ) -> Vec<(usize, (usize, usize))> {
        let call: Vec<&str> = prefix
            .iter()
            .map(String::as_str)
            .chain(c.url.split('/').filter(|s| !s.is_empty()))
            .collect();
        let mut out: Vec<(usize, (usize, usize))> = self.index[bi]
            .candidates(&call)
            .into_iter()
            .filter_map(|ri| fit_detail(&call, &self.segs[bi][ri]).map(|score| (ri, score)))
            .collect();
        out.sort_by_key(|a| std::cmp::Reverse(a.1));
        out
    }

    pub fn outcome(&self, c: &CallRow) -> Outcome {
        if c.external || framework_endpoint(&c.url) {
            return Outcome::External;
        }
        // A variable prefix with known values (`kind === 'x' ? 'area/x' : 'area'`):
        // try each full URL; the call reaches every route they match.
        if expandable(c) {
            let mut all = Vec::new();
            let mut wrong = Vec::new();
            let mut closest = Vec::new();
            for full in full_urls(c) {
                match self.outcome(&full) {
                    Outcome::Matched(v) => all.extend(
                        v.into_iter()
                            .filter(|h| !all.contains(h))
                            .collect::<Vec<_>>(),
                    ),
                    Outcome::MethodMismatch(v) => wrong.extend(v),
                    Outcome::NotFound(v) => closest.extend(v),
                    _ => {}
                }
            }
            if !all.is_empty() {
                return Outcome::Matched(all);
            }
            // Every value is known and none has the route with this method.
            if !wrong.is_empty() {
                wrong.dedup();
                wrong.truncate(3);
                return Outcome::MethodMismatch(wrong);
            }
            // Every full URL is known and none has a route: not a guess on the
            // template (`/{}-permit/{}` would fit some other `…-permit`).
            if !c.variants.is_empty() && !c.variants_loose {
                closest.dedup();
                closest.truncate(3);
                return Outcome::NotFound(closest);
            }
        }
        let mut matched = Vec::new();
        let mut wrong_method = Vec::new();
        for bi in 0..self.backends.len() {
            let mut prefixes = vec![self.bases[bi].0.clone()];
            if !prefixes[0].is_empty() {
                prefixes.push(Vec::new());
            }
            for p in prefixes {
                let hits = self.raw_matches(bi, &p, c);
                if hits.is_empty() {
                    continue;
                }
                // The most literal fits; among those with the right method,
                // variables landing on route parameters win (`/orders/{}` is
                // `orders/{order}`, not `orders/export`).
                let best_lits = hits[0].1.0;
                let (ok, wrong): (Vec<_>, Vec<_>) = hits
                    .into_iter()
                    .take_while(|(_, score)| score.0 == best_lits)
                    .partition(|(ri, _)| {
                        method_ok(c.method.as_deref(), &self.backends[bi].routes[*ri])
                    });
                let best_params = ok.iter().map(|(_, score)| score.1).max();
                for (ri, score) in ok {
                    if Some(score.1) == best_params {
                        matched.push((bi, ri, best_lits));
                    }
                }
                for (ri, _) in wrong {
                    wrong_method.push((bi, ri, best_lits));
                }
                break;
            }
        }
        // Across backends keep the most specific fit (a catch-all route in a
        // second app must not claim calls meant for the API).
        let keep_best = |v: Vec<(usize, usize, usize)>| -> Vec<(usize, usize)> {
            let best = v.iter().map(|x| x.2).max().unwrap_or(0);
            v.into_iter()
                .filter(|x| x.2 == best)
                .map(|x| (x.0, x.1))
                .collect()
        };
        let matched = keep_best(matched);
        let mut wrong_method = keep_best(wrong_method);
        if matched.is_empty() && c.optional_tail && !c.url.ends_with("/{}") {
            let with = CallRow {
                url: format!("{}/{{}}", c.url.trim_end_matches('/')),
                optional_tail: false,
                ..c.clone()
            };
            if let Outcome::Matched(v) = self.outcome(&with) {
                return Outcome::Matched(v);
            }
        }
        if !matched.is_empty() {
            return Outcome::Matched(matched);
        }
        // `${base}/mark-sold-out`, `${path}/email`: match the known tail.
        let tail: Vec<&str> = c
            .url
            .split('/')
            .filter(|s| !s.is_empty())
            .skip_while(|s| *s == "{}")
            .collect();
        let unknown_start = c.base_var || c.url.starts_with("/{}");
        if unknown_start && !tail.is_empty() {
            // Routes ending in the known tail, ranked by how many of its words
            // they match (`api/selfcare/contacts` before `invoice/{hashId}`).
            let mut hits: Vec<(usize, usize, usize)> = Vec::new();
            for (bi, b) in self.backends.iter().enumerate() {
                for (ri, r) in self.segs[bi].iter().enumerate() {
                    if r.len() >= tail.len()
                        && let Some(lits) = fit(&tail, &r[r.len() - tail.len()..])
                        && method_ok(c.method.as_deref(), &b.routes[ri])
                    {
                        hits.push((bi, ri, lits));
                    }
                }
            }
            let best = hits.iter().map(|h| h.2).max().unwrap_or(0);
            let top: Vec<(usize, usize)> = hits
                .iter()
                .filter(|h| h.2 == best)
                .map(|h| (h.0, h.1))
                .collect();
            return match top.len() {
                1..=3 if best > 0 => Outcome::Matched(top),
                _ => Outcome::Unresolved(top.into_iter().take(3).collect()),
            };
        }
        if !wrong_method.is_empty() {
            wrong_method.truncate(3);
            Outcome::MethodMismatch(wrong_method)
        } else {
            Outcome::NotFound(self.closest(c))
        }
    }

    /// Routes sharing the longest run of leading segments with the call.
    fn closest(&self, c: &CallRow) -> Vec<(usize, usize)> {
        let mut scored = Vec::new();
        for (bi, b) in self.backends.iter().enumerate() {
            let base = &self.bases[bi].0;
            let call: Vec<String> = base
                .iter()
                .cloned()
                .chain(
                    c.url
                        .split('/')
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned),
                )
                .collect();
            // How alike the call's last word is to each distinct route word.
            let (list, per_route) = &self.words[bi];
            let last_lit = call.iter().rev().find(|s| *s != "{}");
            let alike: Vec<f64> = match last_lit {
                Some(l) => list.iter().map(|x| strsim::jaro_winkler(x, l)).collect(),
                None => Vec::new(),
            };
            for (ri, r) in self.segs[bi].iter().enumerate() {
                let mut same = 0;
                for (a, s) in call.iter().zip(r) {
                    match s {
                        Seg::Lit(l) if l == a || a == "{}" => same += 1,
                        Seg::Param | Seg::Opt => same += 1,
                        _ => break,
                    }
                }
                let sim = per_route[ri]
                    .iter()
                    .filter_map(|w| alike.get(*w as usize).copied())
                    .fold(0.0, f64::max);
                let score = same as f64 + sim;
                if same > base.len() || sim > 0.85 {
                    scored.push((score, method_ok(c.method.as_deref(), &b.routes[ri]), bi, ri));
                }
            }
        }
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.cmp(&a.1)));
        scored
            .into_iter()
            .take(3)
            .map(|(_, _, bi, ri)| (bi, ri))
            .collect()
    }

    pub fn route_label(&self, (bi, ri): (usize, usize)) -> String {
        let r = &self.backends[bi].routes[ri];
        let mut s = format!("{} {}", r.methods_str(), r.uri);
        if self.backends.len() > 1 {
            s = format!("{}: {s}", self.backends[bi].name);
        }
        s
    }
}

pub fn call_label(c: &CallRow) -> String {
    format!("{} {}", c.method.as_deref().unwrap_or("?"), c.url)
}

/// Fields named by the read paths (`data.data.name` → `name`).
pub fn fields_read(c: &CallRow) -> Vec<String> {
    let mut out: Vec<String> = c
        .fields
        .iter()
        .filter_map(|p| codemap_vue::api::field_path(p))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Fields read that the endpoint does not return. Only when its shape is
/// fully known; read paths are walked against the shape, so an explicit
/// `'data' => [...]` key and a client's own `data` wrapper are both allowed.
pub fn not_returned(c: &CallRow, shape: &ResponseShape) -> Vec<String> {
    const BUILTINS: &[&str] = &[
        "length", "map", "filter", "forEach", "find", "some", "every", "reduce", "slice",
        "includes", "indexOf", "toString", "then", "catch", "status", "headers", "config",
    ];
    if !shape.complete
        || !matches!(
            shape.kind.as_str(),
            "resource" | "collection" | "array" | "json"
        )
    {
        return Vec::new();
    }
    let wrapped = matches!(shape.kind.as_str(), "resource" | "collection");
    let body_level = |s: &str| {
        (shape.kind == "collection" && matches!(s, "links" | "meta"))
            || shape.additional.iter().any(|f| f.name == s)
    };
    // The first segment must be a field; its child, when the children are known.
    let check = |segs: &[&str]| -> Option<String> {
        let first = segs.first()?;
        let Some(f) = shape.fields.iter().find(|f| f.name == *first) else {
            return Some((*first).to_owned());
        };
        match (&f.children, segs.get(1)) {
            (Some(children), Some(next)) if !children.iter().any(|c| c == next) => {
                Some(format!("{first}.{next}"))
            }
            _ => None,
        }
    };
    let mut out: Vec<String> = Vec::new();
    for p in &c.fields {
        let segs: Vec<&str> = p
            .split('.')
            .flat_map(|s| s.split("[]"))
            .filter(|s| !s.is_empty() && *s != "value")
            .take_while(|s| !BUILTINS.contains(s))
            .collect();
        let missing = if wrapped {
            let n = segs.iter().take(2).take_while(|s| **s == "data").count();
            match segs.get(n) {
                None => None,
                Some(f) if body_level(f) => None,
                Some(_) => check(&segs[n..]),
            }
        } else {
            // Read as the body itself, or through a client's `data` wrapper (axios).
            match check(&segs) {
                None => None,
                Some(m) if segs.first() == Some(&"data") => {
                    if segs.len() == 1 || check(&segs[1..]).is_none() {
                        None
                    } else {
                        Some(m)
                    }
                }
                Some(m) => Some(m),
            }
        };
        if let Some(m) = missing
            && !out.contains(&m)
        {
            out.push(m);
        }
    }
    out
}

/// Routes Laravel registers itself (broadcasting, Sanctum, health check).
fn framework_endpoint(url: &str) -> bool {
    let u = url.trim_end_matches('/');
    ["/broadcasting/auth", "/sanctum/csrf-cookie", "/up"]
        .iter()
        .any(|f| u.ends_with(f))
}

/// Response shapes, cached per action.
pub struct Shapes<'a> {
    own: &'a Store,
    cache: HashMap<(usize, String, String), ResponseShape>,
    accepts: HashMap<(usize, String, String), Option<codemap_laravel::accepts::Accepted>>,
    accepts_cache: codemap_laravel::accepts::AcceptsCache,
    /// Time spent reading controllers, for the debug log.
    pub spent_ms: u128,
}

impl<'a> Shapes<'a> {
    pub fn new(own: &'a Store) -> Self {
        Self {
            own,
            cache: HashMap::new(),
            accepts: HashMap::new(),
            accepts_cache: Default::default(),
            spent_ms: 0,
        }
    }

    pub fn get(&mut self, m: &Matcher<'_>, (bi, ri): (usize, usize)) -> Option<ResponseShape> {
        let b = &m.backends[bi];
        let r = &b.routes[ri];
        let (ctrl, action) = (
            r.controller_fqn.clone()?,
            r.controller_method
                .clone()
                .unwrap_or_else(|| "__invoke".into()),
        );
        let key = (bi, ctrl.clone(), action.clone());
        if let Some(s) = self.cache.get(&key) {
            return Some(s.clone());
        }
        let started = std::time::Instant::now();
        let s = responses::for_action(b.conn(self.own), &b.root, &ctrl, &action);
        let ms = started.elapsed().as_millis();
        self.spent_ms += ms;
        if ms > 100 {
            tracing::warn!(ms, "slow response shape for {ctrl}@{action}");
        }
        self.cache.insert(key, s.clone());
        Some(s)
    }

    /// What the route's action accepts (Form Request rules, `validate()`, reads).
    pub fn accepts(
        &mut self,
        m: &Matcher<'_>,
        (bi, ri): (usize, usize),
    ) -> Option<codemap_laravel::accepts::Accepted> {
        let b = &m.backends[bi];
        let r = &b.routes[ri];
        let ctrl = r.controller_fqn.clone()?;
        let action = r
            .controller_method
            .clone()
            .unwrap_or_else(|| "__invoke".into());
        let own = self.own;
        let cache = &mut self.accepts_cache;
        self.accepts
            .entry((bi, ctrl.clone(), action.clone()))
            .or_insert_with(|| {
                codemap_laravel::accepts::for_action_with(
                    b.conn(own),
                    &b.root,
                    &ctrl,
                    &action,
                    cache,
                )
            })
            .clone()
    }

    /// Where a resource class builds its keys: its `toArray`, else the class.
    pub fn resource_at(&self, m: &Matcher<'_>, bi: usize, fqn: &str) -> Option<String> {
        let conn = m.backends[bi].conn(self.own);
        conn.query_row(
            "SELECT f.path, s.start_line FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE (s.parent_fqn = ?1 AND s.kind = 'method' AND s.name = 'toArray') OR s.fqn = ?1
             ORDER BY s.kind = 'method' DESC LIMIT 1",
            [fqn],
            |r| {
                Ok(format!(
                    "{}:{}",
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?
                ))
            },
        )
        .ok()
    }
}

// ---------------------------------------------------------------- api_usage

#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApiUsageParams {
    /// Workspace app to look at (`backend`, `consumer`, …). Omit to search every app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// A backend endpoint: `GET api/v1/malls/{mall}`, `api/v1/malls`, or part of a URI (`malls`).
    #[serde(default)]
    pub endpoint: Option<String>,
    /// A frontend file (or part of its path): its calls with their routes and fields.
    #[serde(default)]
    pub file: Option<String>,
    /// `missing` (calls with no route), `fields` (fields read but not returned),
    /// `payload` (required fields not sent, sent body fields the action does not
    /// take, likely renames), `payload_all` (also GET query parameters the
    /// action does not read),
    /// `unresolved` (URL could not be tied to one route), or `all` (default).
    #[serde(default)]
    pub status: Option<String>,
    /// Only what this branch touches: calls in files changed since it left the
    /// default branch (plus uncommitted changes), and calls anywhere to backend
    /// routes whose route file or controller changed.
    #[serde(default)]
    pub git_changes: bool,
    /// Like `git_changes`, for these files of this project (backend changes
    /// still come from git).
    #[serde(default)]
    pub changed_files: Option<Vec<String>>,
    /// The branch `git_changes` compares with (default: origin/HEAD, main, master, develop).
    #[serde(default)]
    pub base: Option<String>,
    /// Hide findings saved by `codemap baseline`: report only new ones.
    #[serde(default)]
    pub new_only: bool,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub detail: DetailParam,
}

/// The findings of one call, as stable strings (no line numbers, so a
/// baseline survives edits above the call).
pub fn finding_keys(
    file: &str,
    call: &str,
    status: &str,
    lists: &[(&str, &[String])],
) -> Vec<String> {
    let mut out = Vec::new();
    if matches!(status, "missing" | "method_mismatch" | "unresolved") {
        out.push(format!("{status} {file} {call}"));
    }
    for (kind, list) in lists {
        out.extend(list.iter().map(|f| format!("{kind} {file} {call} {f}")));
    }
    out
}

/// The row fields that hold findings, with their key kind.
const FINDING_LISTS: &[(&str, &str)] = &[
    ("fields", "read_but_not_returned"),
    ("typed", "typed_but_not_returned"),
    ("required", "missing_required"),
    ("unknown", "unknown_field"),
    ("rename", "possible_rename"),
];

/// What a call sends against what its action accepts.
#[derive(Debug, Default)]
struct PayloadCheck {
    /// Required fields never sent (only when the payload is fully known).
    required: Vec<String>,
    /// Sent body fields the action does not take (only when both the payload
    /// and the accepted list are fully known).
    unknown: Vec<String>,
    /// The same for a GET (or a call whose method is unknown): query
    /// parameters Laravel ignores, so only a note.
    unknown_query: Vec<String>,
    /// `sent → required` pairs on one call: a likely rename.
    renames: Vec<String>,
}

fn payload_check(c: &CallRow, acc: &codemap_laravel::accepts::Accepted) -> PayloadCheck {
    let mut out = PayloadCheck::default();
    let Some(sent) = &c.sends else {
        return out;
    };
    if !sent.complete {
        return out;
    }
    out.required = acc
        .fields
        .iter()
        .filter(|(f, req)| **req && !sent.keys.contains(f))
        .map(|(f, _)| f.clone())
        .collect();
    let unknown: Vec<String> = if acc.complete {
        sent.keys
            .iter()
            .filter(|k| !acc.fields.contains_key(*k))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    // A required field missing and an unknown one sent: pair the most alike.
    let mut free: Vec<&String> = unknown.iter().collect();
    for r in &out.required {
        if let Some((i, _)) = free
            .iter()
            .enumerate()
            .map(|(i, u)| (i, strsim::jaro_winkler(u, r)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
        {
            out.renames.push(format!("{} → {r}", free.remove(i)));
        }
    }
    let query_only = c
        .method
        .as_deref()
        .is_none_or(|m| m.split('|').all(|x| matches!(x, "GET" | "HEAD")));
    if query_only {
        out.unknown_query = unknown;
    } else {
        out.unknown = unknown;
    }
    out
}

/// The same keys, from a full `api_usage` row.
pub fn row_keys(v: &Value) -> Vec<String> {
    let at = v["at"].as_str().unwrap_or_default();
    let file = at.rsplit_once(':').map_or(at, |(f, _)| f);
    let list = |k: &str| -> Vec<String> {
        v[k].as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let lists: Vec<(&str, Vec<String>)> = FINDING_LISTS
        .iter()
        .map(|(kind, field)| (*kind, list(field)))
        .collect();
    let lists: Vec<(&str, &[String])> = lists.iter().map(|(k, l)| (*k, l.as_slice())).collect();
    finding_keys(
        file,
        v["call"].as_str().unwrap_or_default(),
        v["status"].as_str().unwrap_or_default(),
        &lists,
    )
}

/// The file name `codemap baseline` writes inside a project's index folder;
/// a copy committed to the repository root is read too (for CI).
pub const BASELINE_FILE: &str = "baseline.json";
pub const BASELINE_IN_REPO: &str = ".codemap-baseline.json";

/// Where this project's baseline is read from, in order.
pub fn baseline_paths(project: &Project) -> Vec<PathBuf> {
    vec![
        project.index_path.with_file_name(BASELINE_FILE),
        project.root.join(BASELINE_IN_REPO),
    ]
}

/// The keys saved for this app (`""` outside a workspace).
pub fn load_baseline(path: &Path, app: Option<&str>) -> Option<HashSet<String>> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let list = v["findings"][app.unwrap_or("")].as_array()?;
    Some(
        list.iter()
            .filter_map(|x| x.as_str().map(str::to_owned))
            .collect(),
    )
}

/// Files a change touches, per side.
struct Scope {
    base: String,
    own: HashSet<String>,
    /// Per backend (same order as `backends`).
    backend: Vec<HashSet<String>>,
}

pub struct ApiOutput {
    pub data: Value,
    pub next_cursor: Option<String>,
    pub truncated: bool,
}

pub fn api_usage(store: &Store, project: &Project, p: ApiUsageParams) -> R<ApiOutput> {
    let detail: Detail = p.detail.into();
    let t0 = std::time::Instant::now();
    let calls = codemap_vue::api::load(store.conn())?;
    let (backends, warnings) = backends(store, project)?;
    let t_backends = t0.elapsed().as_millis();
    let matcher = Matcher::new(&backends, &calls);
    let t_matcher = t0.elapsed().as_millis();
    // The controllers behind the routes, parsed on all cores now rather than
    // one at a time while the calls are checked.
    for b in &backends {
        let mut ctrls: Vec<&str> = b
            .routes
            .iter()
            .filter_map(|r| r.controller_fqn.as_deref())
            .collect();
        ctrls.sort_unstable();
        ctrls.dedup();
        responses::prefetch(b.conn(store), &b.root, &ctrls);
    }
    let t_prefetch = t0.elapsed().as_millis();
    let mut shapes = Shapes::new(store);
    let limit = p.limit.unwrap_or(20).clamp(1, 100);
    let offset = p
        .cursor
        .as_deref()
        .and_then(|c| c.strip_prefix('o'))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0usize);

    let backend_info: Vec<Value> = backends
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let (base, n, total) = &matcher.bases[i];
            json!({
                "name": b.name,
                "routes": b.routes.len(),
                "api_base": format!("/{}", base.join("/")),
                "api_base_source": if b.configured_base.is_some() {
                    "configured".to_owned()
                } else if *n == 0 && backends.len() > 1 {
                    "none: no call of this app goes to this backend".to_owned()
                } else {
                    format!("inferred: {n} of {total} calls match with it")
                },
            })
        })
        .collect();
    let mut data = json!({ "backends": backend_info });
    if calls.is_empty() {
        data["hint"] = json!("No HTTP calls were found in this project's Vue / Nuxt code.");
        return Ok(ApiOutput {
            data,
            next_cursor: None,
            truncated: false,
        });
    }
    if backends.is_empty() {
        data["hint"] = json!(
            "No backend routes to match against. Link the Laravel project: [[links]] name/path in .codemap.toml or --link backend=/path (mount it read-only in Docker)."
        );
    }
    if !warnings.is_empty() {
        data["warnings"] = json!(warnings);
    }

    let scope = if p.git_changes || p.changed_files.is_some() {
        let (base, mut own) = match codemap_core::git::branch_files(
            &project.root,
            p.base.as_deref(),
        ) {
            Some((b, f)) if p.git_changes => (b, f),
            None if p.git_changes => {
                return Err(ToolError::InvalidParams(
                    "git_changes: this folder is not a git repository (or git is unavailable); pass changed_files".into(),
                ));
            }
            _ => ("changed_files".to_owned(), Vec::new()),
        };
        own.extend(p.changed_files.clone().unwrap_or_default());
        let own: HashSet<String> = own.into_iter().collect();
        let backend = backends
            .iter()
            .map(|b| {
                if b.store.is_none() {
                    own.clone()
                } else {
                    codemap_core::git::branch_files(&b.root, p.base.as_deref())
                        .map(|(_, f)| f.into_iter().collect())
                        .unwrap_or_default()
                }
            })
            .collect();
        data["scope"] = json!({
            "compared_with": base,
            "changed_files": own.len(),
        });
        Some(Scope { base, own, backend })
    } else {
        None
    };
    let mut controller_files: HashMap<(usize, String), Option<String>> = HashMap::new();
    let mut route_changed = |(bi, ri): (usize, usize)| -> bool {
        let Some(sc) = &scope else { return true };
        let changed = &sc.backend[bi];
        if changed.is_empty() {
            return false;
        }
        let r = &backends[bi].routes[ri];
        if r.file.as_deref().is_some_and(|f| changed.contains(f)) {
            return true;
        }
        let Some(ctrl) = r.controller_fqn.clone() else {
            return false;
        };
        controller_files
            .entry((bi, ctrl.clone()))
            .or_insert_with(|| {
                backends[bi]
                    .conn(store)
                    .query_row(
                        "SELECT f.path FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.fqn = ?1 LIMIT 1",
                        [&ctrl],
                        |row| row.get::<_, String>(0),
                    )
                    .ok()
            })
            .as_deref()
            .is_some_and(|f| changed.contains(f))
    };
    let baseline: Option<(PathBuf, HashSet<String>)> = if p.new_only {
        let found = baseline_paths(project)
            .into_iter()
            .find_map(|path| load_baseline(&path, project.app.as_deref()).map(|b| (path, b)));
        if found.is_none() {
            return Err(ToolError::InvalidParams(
                "new_only: no baseline saved; run `codemap baseline` first".into(),
            ));
        }
        found
    } else {
        None
    };
    let mut hidden_by_baseline = 0usize;

    // One endpoint: its callers and fields.
    if let Some(ep) = &p.endpoint {
        let (method, uri) = match ep.trim().split_once(' ') {
            Some((m, u)) if m.chars().all(|c| c.is_ascii_alphabetic()) => {
                (Some(m.to_ascii_uppercase()), u.trim())
            }
            _ => (None, ep.trim()),
        };
        let uri = uri.trim_matches('/');
        let mut targets = Vec::new();
        for (bi, b) in backends.iter().enumerate() {
            for (ri, r) in b.routes.iter().enumerate() {
                // `{mall}` and `{mall:slug}` are the same route.
                let uri_ok = route_segs(&r.uri) == route_segs(uri)
                    || (!uri.contains('{') && r.uri.contains(uri));
                let m_ok = method
                    .as_deref()
                    .is_none_or(|m| r.methods.iter().any(|x| x == m));
                if uri_ok && m_ok {
                    targets.push((bi, ri));
                }
            }
        }
        let exact: Vec<(usize, usize)> = targets
            .iter()
            .copied()
            .filter(|(bi, ri)| route_segs(&backends[*bi].routes[*ri].uri) == route_segs(uri))
            .collect();
        if !exact.is_empty() {
            targets = exact;
        }
        let total = targets.len();
        let mut list = Vec::new();
        for t in targets.into_iter().skip(offset).take(limit) {
            let r = &backends[t.0].routes[t.1];
            let shape = shapes.get(&matcher, t);
            let callers: Vec<Value> = calls
                .iter()
                .filter(|c| match matcher.outcome(c) {
                    Outcome::Matched(v) => v.contains(&t),
                    _ => false,
                })
                .map(|c| {
                    let missing = shape.as_ref().map(|s| not_returned(c, s)).unwrap_or_default();
                    let mut v = json!({ "at": format!("{}:{}", c.file, c.line), "call": format!("{} {}", c.via, c.raw) });
                    if !c.fields.is_empty() {
                        v["fields_read"] = json!(fields_read(c));
                    }
                    if !missing.is_empty() {
                        v["read_but_not_returned"] = json!(missing);
                    }
                    v
                })
                .collect();
            list.push(json!({
                "route": matcher.route_label(t),
                "name": r.name,
                "action": r.action_label(),
                "defined_at": r.location(),
                "returns": shape,
                "callers": callers,
            }));
        }
        data["found"] = json!(total > 0);
        data["endpoints"] = json!(list);
        if total == 0 {
            data["hint"] = json!(format!(
                "No backend route matches `{ep}`. Use list_routes on the backend to browse."
            ));
        }
        let next = (offset + limit < total).then(|| format!("o{}", offset + limit));
        return Ok(ApiOutput {
            data,
            truncated: next.is_some(),
            next_cursor: next,
        });
    }

    // Calls (optionally of one file / one status).
    let status = p.status.as_deref().unwrap_or("all");
    let mut summary: BTreeMap<&str, usize> = BTreeMap::new();
    let mut rows = Vec::new();
    for c in calls.iter().filter(|c| {
        p.file
            .as_deref()
            .is_none_or(|f| c.file == f || c.file.contains(f))
    }) {
        let o = matcher.outcome(c);
        let (label, routes_hit) = match &o {
            Outcome::Matched(v) => ("matched", v.clone()),
            Outcome::MethodMismatch(v) => ("method_mismatch", v.clone()),
            Outcome::NotFound(v) if is_placeholder(&c.url) => ("placeholder", v.clone()),
            Outcome::NotFound(v) => ("missing", v.clone()),
            Outcome::External => ("external", Vec::new()),
            Outcome::Unresolved(v) => ("unresolved", v.clone()),
        };
        // URL variants (`/${kind}-permit/…`) that have no route, also when
        // another variant matched.
        let variants_missing: Vec<String> = if c.variants.is_empty() || c.variants_loose {
            Vec::new()
        } else {
            full_urls(c)
                .into_iter()
                .filter(|f| matches!(matcher.outcome(f), Outcome::NotFound(_)))
                .map(|f| f.url)
                .collect()
        };
        if let Some(sc) = &scope
            && !sc.own.contains(&c.file)
            && !routes_hit.iter().any(|h| route_changed(*h))
        {
            continue;
        }
        // Fields the call's TypeScript type requires but the endpoint never sends.
        let typed_missing: Vec<String> = match &o {
            Outcome::Matched(v) if !c.typed.is_empty() => shapes
                .get(&matcher, v[0])
                .map(|s| {
                    let as_read = CallRow {
                        fields: c.typed.clone(),
                        ..CallRow::default()
                    };
                    not_returned(&as_read, &s)
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        // Only a typed field the app actually reads is a problem; one it never
        // reads is a note (the type is out of date, nothing breaks).
        let read_now = fields_read(c);
        let is_read = |m: &String| {
            let m = m.trim_start_matches("data.");
            read_now.iter().any(|r| {
                r == m || r.starts_with(&format!("{m}.")) || m.starts_with(&format!("{r}."))
            })
        };
        let (typed_missing, typed_unread): (Vec<String>, Vec<String>) =
            typed_missing.into_iter().partition(|m| is_read(m));
        let missing_fields = match &o {
            Outcome::Matched(v) if !c.fields.is_empty() => shapes
                .get(&matcher, v[0])
                .map(|s| not_returned(c, &s))
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let accepted = match &o {
            Outcome::Matched(v) if c.sends.is_some() => shapes.accepts(&matcher, v[0]),
            _ => None,
        };
        let PayloadCheck {
            required: missing_required,
            unknown: unknown_field,
            unknown_query,
            renames,
        } = accepted
            .as_ref()
            .map(|a| payload_check(c, a))
            .unwrap_or_default();
        if let Some((_, known)) = &baseline {
            let keys = finding_keys(
                &c.file,
                &call_label(c),
                label,
                &[
                    ("fields", &missing_fields),
                    ("typed", &typed_missing),
                    ("required", &missing_required),
                    ("unknown", &unknown_field),
                    ("rename", &renames),
                ],
            );
            if !keys.is_empty() && keys.iter().all(|k| known.contains(k)) {
                hidden_by_baseline += 1;
                continue;
            }
        }
        *summary.entry(label).or_insert(0) += 1;
        if !typed_missing.is_empty() {
            *summary.entry("typed_not_returned").or_insert(0) += 1;
        }
        if !typed_unread.is_empty() {
            *summary.entry("typed_unread_not_returned").or_insert(0) += 1;
        }
        if !missing_fields.is_empty() {
            *summary.entry("fields_not_returned").or_insert(0) += 1;
        }
        if !missing_required.is_empty() {
            *summary.entry("missing_required").or_insert(0) += 1;
        }
        if !unknown_field.is_empty() {
            *summary.entry("unknown_field").or_insert(0) += 1;
        }
        if !unknown_query.is_empty() {
            *summary.entry("unknown_query_note").or_insert(0) += 1;
        }
        if !renames.is_empty() {
            *summary.entry("possible_rename").or_insert(0) += 1;
        }
        let payload_bad =
            !missing_required.is_empty() || !unknown_field.is_empty() || !renames.is_empty();
        let partly_missing = label == "matched" && !variants_missing.is_empty();
        if partly_missing {
            *summary.entry("variant_without_route").or_insert(0) += 1;
        }
        let keep = match status {
            "missing" => {
                label != "placeholder"
                    && (matches!(o, Outcome::NotFound(_) | Outcome::MethodMismatch(_))
                        || partly_missing)
            }
            "placeholder" => label == "placeholder",
            "fields" => {
                !missing_fields.is_empty() || !typed_missing.is_empty() || !typed_unread.is_empty()
            }
            "unresolved" => matches!(o, Outcome::Unresolved(_)),
            "payload" => payload_bad,
            "payload_all" => payload_bad || !unknown_query.is_empty(),
            _ => {
                payload_bad
                    || partly_missing
                    || p.file.is_some()
                    // With `git_changes` / `changed_files`, the calls the
                    // change touches are what was asked for, matched or not.
                    || scope.is_some()
                    || matches!(
                        o,
                        Outcome::NotFound(_) | Outcome::MethodMismatch(_) | Outcome::Unresolved(_)
                    )
                    || !missing_fields.is_empty()
                    || !typed_missing.is_empty()
                    || !typed_unread.is_empty()
            }
        };
        if !keep {
            continue;
        }
        rows.push(Row {
            c,
            label,
            hits: routes_hit,
            missing: missing_fields,
            typed_missing,
            typed_unread,
            missing_required,
            unknown_field,
            unknown_query: if status == "payload_all" {
                unknown_query
            } else {
                Vec::new()
            },
            renames,
            variants_missing,
            accepted,
        });
    }
    let total = rows.len();
    let shown: Vec<Value> = rows
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|row| {
            let Row {
                c,
                label,
                hits,
                missing,
                typed_missing,
                typed_unread,
                missing_required,
                unknown_field,
                unknown_query,
                renames,
                variants_missing,
                accepted,
            } = row;
            let routes: Vec<String> = hits.iter().map(|h| matcher.route_label(*h)).collect();
            // A broken call in a function nothing uses is dead code, not a live bug.
            let dead = matches!(label, "missing" | "method_mismatch")
                .then(|| unused_function_at(store, &project.root, c))
                .flatten();
            if detail == Detail::Full || p.file.is_some() {
                let mut v = json!({
                    "at": format!("{}:{}", c.file, c.line),
                    "call": call_label(c),
                    "via": c.via,
                    "status": label,
                });
                if let Some(f) = &dead {
                    v["in_unused_function"] = json!(f);
                }
                if !variants_missing.is_empty() {
                    v["variants_without_route"] = json!(variants_missing);
                }
                if label == "placeholder" {
                    v["note"] = json!(
                        "the URL looks like a placeholder (`_api_url_`, `unitURL`) filled in elsewhere, not a real path"
                    );
                }
                match label {
                    "matched" => v["route"] = json!(routes.first()),
                    "missing" => v["closest"] = json!(routes),
                    "method_mismatch" => v["path_exists_as"] = json!(routes),
                    "unresolved" => {
                        v["why"] = json!(unresolved_why(c, &routes));
                        if !routes.is_empty() {
                            v["could_be"] = json!(routes);
                        }
                    }
                    _ => {}
                }
                if !c.fields.is_empty() {
                    v["fields_read"] = json!(fields_read(c));
                }
                if !missing.is_empty() {
                    v["read_but_not_returned"] = json!(missing);
                }
                if !typed_missing.is_empty() {
                    v["typed_but_not_returned"] = json!(typed_missing);
                }
                // How codemap decided what the endpoint returns, and what the
                // frontend read: to tell a real bug from a heuristic miss.
                if (!missing.is_empty() || !typed_missing.is_empty())
                    && let Some(shape) = hits.first().and_then(|h| shapes.get(&matcher, *h))
                {
                    let mut ev = shape_evidence(&shape);
                    if let Some(at) = shape
                        .resource
                        .as_deref()
                        .and_then(|r| shapes.resource_at(&matcher, hits[0].0, r))
                    {
                        ev["resource_at"] = json!(at);
                    }
                    ev["read_paths"] = json!(c.fields);
                    if !c.typed.is_empty() {
                        ev["typed_paths"] = json!(c.typed);
                    }
                    v["evidence"] = ev;
                }
                if !typed_unread.is_empty() {
                    v["note_typed_unread_not_returned"] = json!(typed_unread);
                }
                if !missing_required.is_empty() {
                    v["missing_required"] = json!(missing_required);
                }
                if !unknown_field.is_empty() {
                    v["unknown_field"] = json!(unknown_field);
                }
                if !renames.is_empty() {
                    v["possible_rename"] = json!(renames);
                }
                if !unknown_query.is_empty() {
                    v["note_unknown_query"] = json!(unknown_query);
                }
                if let (Some(acc), Some(sent)) = (&accepted, &c.sends) {
                    if !missing_required.is_empty()
                        || !unknown_field.is_empty()
                        || !renames.is_empty()
                        || !unknown_query.is_empty()
                    {
                        v["payload_evidence"] = json!({
                            "sends": sent.keys,
                            "sends_complete": sent.complete,
                            "accepts_from": acc.source,
                            "accepts_at": acc.at,
                            "accepts": acc.fields.iter().map(|(f, r)| if *r { format!("{f} (required)") } else { f.clone() }).collect::<Vec<_>>(),
                            "accepts_complete": acc.complete,
                        });
                    }
                    // Why the payload could not be checked, when it matters.
                    if !sent.complete && acc.fields.values().any(|r| *r) {
                        v["payload_unchecked"] = json!(sent.note);
                    }
                }
                v
            } else {
                let mut s = format!("{}  {}:{}", call_label(c), c.file, c.line);
                match label {
                    "placeholder" => s.push_str("  → a placeholder URL, not a real path"),
                    "missing" if !variants_missing.is_empty() => s.push_str(&format!(
                        "  → no route for {}",
                        variants_missing.join(" or ")
                    )),
                    "matched" if !variants_missing.is_empty() => s.push_str(&format!(
                        "  → matched, but no route for {}",
                        variants_missing.join(" or ")
                    )),
                    "missing" if routes.is_empty() => s.push_str("  → no route"),
                    "missing" => {
                        s.push_str(&format!("  → no route; closest: {}", routes.join(" | ")))
                    }
                    "method_mismatch" => s.push_str(&format!(
                        "  → wrong method; route is {}",
                        routes.join(" | ")
                    )),
                    "unresolved" => {
                        s.push_str(&format!("  → unclear: {}", unresolved_why(c, &routes)));
                        if !routes.is_empty() {
                            s.push_str(&format!("; could be {}", routes.join(" | ")));
                        }
                    }
                    _ => {}
                }
                if !missing.is_empty() {
                    s.push_str(&format!(
                        "  → reads {} but the endpoint does not return it",
                        missing.join(", ")
                    ));
                    if let Some(shape) = hits.first().and_then(|h| shapes.get(&matcher, *h)) {
                        s.push_str(&format!(" ({})", shape_summary(&shape)));
                    }
                }
                if !typed_missing.is_empty() {
                    s.push_str(&format!(
                        "  → its TypeScript type requires {} but the endpoint does not send it",
                        typed_missing.join(", ")
                    ));
                }
                if !typed_unread.is_empty() {
                    s.push_str(&format!(
                        "  → note: its type lists {}, which the endpoint does not send and nothing reads",
                        typed_unread.join(", ")
                    ));
                }
                if let Some(f) = &dead {
                    s.push_str(&format!("  (in {f}, which nothing uses: dead code)"));
                }
                if !missing_required.is_empty() {
                    s.push_str(&format!(
                        "  → does not send {}, which {} requires",
                        missing_required.join(", "),
                        accepted.as_ref().map(|a| a.source.as_str()).unwrap_or("the action")
                    ));
                }
                if !renames.is_empty() {
                    s.push_str(&format!(
                        "  → possibly renamed (sent → required): {}",
                        renames.join(", ")
                    ));
                }
                if !unknown_query.is_empty() {
                    s.push_str(&format!(
                        "  → note: query {} is not read by the action",
                        unknown_query.join(", ")
                    ));
                }
                if !unknown_field.is_empty() {
                    s.push_str(&format!(
                        "  → sends {}, which the action does not accept (renamed?); it takes {}",
                        unknown_field.join(", "),
                        accepted
                            .as_ref()
                            .map(|a| a.fields.keys().cloned().collect::<Vec<_>>().join(", "))
                            .unwrap_or_default()
                    ));
                }
                json!(s)
            }
        })
        .collect();
    tracing::debug!(
        backends_ms = t_backends,
        matcher_ms = t_matcher - t_backends,
        prefetch_ms = t_prefetch - t_matcher,
        rest_ms = t0.elapsed().as_millis() - t_prefetch,
        shapes_ms = shapes.spent_ms,
        shapes = shapes.cache.len(),
        "api_usage timings"
    );
    data["summary"] = json!(summary);
    if let Some((path, known)) = &baseline {
        data["baseline"] = json!({
            "file": path.display().to_string(),
            "saved_findings": known.len(),
            "calls_hidden": hidden_by_baseline,
        });
    }
    if let Some(sc) = &scope
        && sc.own.is_empty()
        && sc.backend.iter().all(HashSet::is_empty)
    {
        data["hint"] = json!(format!("Nothing changed (compared with {}).", sc.base));
    }
    data["calls"] = json!(shown);
    data["fields_note"] = json!(
        "fields_read is a heuristic (reads of the result and of template state it is stored in); read_but_not_returned is only reported when the endpoint's response shape is fully known."
    );
    let next = (offset + limit < total).then(|| format!("o{}", offset + limit));
    Ok(ApiOutput {
        data,
        truncated: next.is_some(),
        next_cursor: next,
    })
}

/// The exported JS function around `file:line`, when nothing imports or calls it.
fn unused_function_at(store: &Store, root: &Path, c: &CallRow) -> Option<String> {
    let (file, line) = (c.file.as_str(), c.line);
    let conn = store.conn();
    let name: Option<String> = conn
        .query_row(
            "SELECT s.name FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE f.path = ?1 AND s.start_line <= ?2 AND s.end_line >= ?2
               AND s.kind IN ('function', 'composable')
             ORDER BY s.end_line - s.start_line LIMIT 1",
            rusqlite::params![file, line],
            |r| r.get(0),
        )
        .ok();
    // An object's method (`billingApi = { voidInvoice() {…} }`): used when
    // anything calls `billingApi.voidInvoice(…)` (or `this.voidInvoice(…)`
    // inside it).
    let Some(name) = name else {
        let chain = c.in_member.as_deref()?;
        let (_, method) = chain.rsplit_once('.')?;
        let used: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM refs WHERE kind = 'js_member' AND target = ?1",
                [chain],
                |r| r.get(0),
            )
            .ok()?;
        let src = std::fs::read_to_string(root.join(file)).ok();
        let local = src.is_some_and(|t| t.contains(&format!("this.{method}(")));
        return (used == 0 && !local).then(|| chain.to_owned());
    };
    let used: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM refs r JOIN files f ON f.id = r.file_id
             WHERE (r.kind = 'import' AND r.target = ?1) OR (r.kind = 'js_call' AND r.target = ?2)",
            rusqlite::params![format!("{file}#{name}"), name],
            |r| r.get(0),
        )
        .ok()?;
    // Calls inside the same file (not recorded as refs) count as uses too.
    let src = std::fs::read_to_string(root.join(file)).ok();
    let local = src.is_some_and(|t| t.matches(&format!("{name}(")).count() > 1);
    (used == 0 && !local).then_some(name)
}

/// One call kept for the answer, with its findings.
struct Row<'c> {
    c: &'c CallRow,
    label: &'static str,
    hits: Vec<(usize, usize)>,
    missing: Vec<String>,
    typed_missing: Vec<String>,
    typed_unread: Vec<String>,
    missing_required: Vec<String>,
    unknown_field: Vec<String>,
    unknown_query: Vec<String>,
    renames: Vec<String>,
    variants_missing: Vec<String>,
    accepted: Option<codemap_laravel::accepts::Accepted>,
}

/// Where a response shape came from and every key it holds (conditional ones
/// marked), for a fields finding.
fn shape_evidence(shape: &ResponseShape) -> Value {
    let keys = |fields: &[codemap_laravel::responses::ShapeField]| -> Vec<String> {
        fields
            .iter()
            .map(|f| {
                if f.conditional {
                    format!("{} (conditional)", f.name)
                } else {
                    f.name.clone()
                }
            })
            .collect()
    };
    let mut v = json!({
        "shape_from": shape.resource.clone().unwrap_or_else(|| shape.kind.clone()),
        "returned_at": shape.returned_at,
        "returns": keys(&shape.fields),
        "complete": shape.complete,
    });
    if shape.wrapped_in_data {
        v["wrapped_in_data"] = json!(true);
    }
    if !shape.additional.is_empty() {
        v["additional"] = json!(keys(&shape.additional));
    }
    if !shape.notes.is_empty() {
        v["notes"] = json!(shape.notes);
    }
    v
}

/// The same, in one line for brief answers.
fn shape_summary(shape: &ResponseShape) -> String {
    let from = shape.resource.as_deref().unwrap_or(&shape.kind);
    let short = from.rsplit('\\').next().unwrap_or(from);
    let mut names: Vec<&str> = shape.fields.iter().map(|f| f.name.as_str()).collect();
    let more = names.len().saturating_sub(12);
    names.truncate(12);
    format!(
        "shape from {short}{}; returns {}{}",
        shape
            .returned_at
            .as_deref()
            .map(|a| format!(" at {a}"))
            .unwrap_or_default(),
        names.join(", "),
        if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        }
    )
}

/// Why a call could not be tied to one route.
fn unresolved_why(c: &CallRow, candidates: &[String]) -> String {
    let start = if c.base_var {
        "starts with a variable"
    } else {
        "starts with a value only known at run time"
    };
    let why = match candidates.len() {
        0 => format!("the URL {start} ({}) and no route ends like it", c.raw),
        n => format!(
            "the URL {start} ({}) and its tail fits {}{n} routes",
            c.raw,
            if n >= 3 { "at least " } else { "" }
        ),
    };
    // Say when a limit or an unfollowed value stopped the expansion.
    match &c.prefix_note {
        Some(note) => format!("{why}; {note}"),
        None => why,
    }
}

/// For `codemap doctor`: calls to missing endpoints and fields not returned.
pub fn doctor_text(store: &Store, project: &Project) -> R<String> {
    let out = api_usage(
        store,
        project,
        ApiUsageParams {
            limit: Some(100),
            ..ApiUsageParams::default()
        },
    )?;
    let d = &out.data;
    if d["calls"].as_array().is_none() {
        return Ok(String::new());
    }
    let mut s = String::from("\nAPI calls\n");
    for b in d["backends"].as_array().into_iter().flatten() {
        s.push_str(&format!(
            "  backend {} ({} routes), api base {} ({})\n",
            b["name"].as_str().unwrap_or("?"),
            b["routes"],
            b["api_base"].as_str().unwrap_or(""),
            b["api_base_source"].as_str().unwrap_or("")
        ));
    }
    s.push_str(&format!("  {}\n", d["summary"]));
    for w in d["warnings"].as_array().into_iter().flatten() {
        s.push_str(&format!("  ! {}\n", w.as_str().unwrap_or("")));
    }
    if let Some(h) = d["hint"].as_str() {
        s.push_str(&format!("  {h}\n"));
    }
    for c in d["calls"].as_array().into_iter().flatten() {
        s.push_str(&format!("  {}\n", c.as_str().unwrap_or("")));
    }
    Ok(s)
}

/// Frontend calls that hit any of `routes` (`"GET|HEAD api/v1/malls"`), as
/// `file:line  METHOD /url → route`. Used by change impact across a workspace.
pub fn callers_of_routes(store: &Store, project: &Project, routes: &[String]) -> R<Vec<String>> {
    let calls: Vec<CallRow> = codemap_vue::api::load(store.conn())?;
    if calls.is_empty() || routes.is_empty() {
        return Ok(Vec::new());
    }
    let (backends, _) = backends(store, project)?;
    if backends.is_empty() {
        return Ok(Vec::new());
    }
    let matcher = Matcher::new(&backends, &calls);
    let mut out = Vec::new();
    for c in &calls {
        let Outcome::Matched(hits) = matcher.outcome(c) else {
            continue;
        };
        if let Some(hit) = hits.iter().find(|(bi, ri)| {
            let r = &backends[*bi].routes[*ri];
            let label = format!("{} {}", r.methods_str(), r.uri);
            routes.contains(&label)
        }) {
            let r = &backends[hit.0].routes[hit.1];
            out.push(format!(
                "{}:{}  {} → {} {}",
                c.file,
                c.line,
                call_label(c),
                r.methods_str(),
                r.uri
            ));
        }
    }
    Ok(out)
}

/// `verify` claims `GET /orders/{id}` or `POST api/v1/carts`.
pub fn verify_endpoint(
    store: &Store,
    project: &Project,
    item: &str,
) -> R<(String, Option<String>, Vec<String>)> {
    let (backends, _) = backends(store, project)?;
    if backends.is_empty() {
        return Ok((
            "unresolved".into(),
            Some("no backend routes (link the Laravel project)".into()),
            Vec::new(),
        ));
    }
    let (method, url) = match item.trim().split_once(' ') {
        Some((m, u)) if m.chars().all(|c| c.is_ascii_alphabetic()) => {
            (Some(m.to_ascii_uppercase()), u.trim())
        }
        _ => (None, item.trim()),
    };
    let url: String = url
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.starts_with('{') || s.starts_with(':') {
                "{}"
            } else {
                s
            }
        })
        .collect::<Vec<_>>()
        .join("/");
    let call = CallRow {
        method,
        url: format!("/{url}"),
        ..CallRow::default()
    };
    let calls = codemap_vue::api::load(store.conn())?;
    let matcher = Matcher::new(&backends, &calls);
    Ok(match matcher.outcome(&call) {
        Outcome::Matched(v) => (
            "exists".into(),
            backends[v[0].0].routes[v[0].1].location(),
            Vec::new(),
        ),
        Outcome::MethodMismatch(v) => (
            "not_found".into(),
            Some("the path exists with another method".into()),
            v.into_iter().map(|h| matcher.route_label(h)).collect(),
        ),
        Outcome::NotFound(v) => (
            "not_found".into(),
            None,
            v.into_iter().map(|h| matcher.route_label(h)).collect(),
        ),
        Outcome::External => ("external".into(), None, Vec::new()),
        Outcome::Unresolved(v) => (
            "unresolved".into(),
            Some("the URL starts with a variable path".into()),
            v.into_iter().map(|h| matcher.route_label(h)).collect(),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crossing_wildcards_is_no_fit() {
        let route = route_segs("widgets/history/{widgetId}");
        assert_eq!(fit(&["widgets", "{}", "archive"], &route), None);
        // Still fine: a value on a parameter, a variable on a parameter, a
        // variable prefix, and a word on a parameter alone (`users/me`).
        assert!(fit(&["widgets", "history", "{}"], &route).is_some());
        assert!(fit(&["widgets", "history", "42"], &route).is_some());
        assert!(fit(&["{}", "history", "7"], &route).is_some());
        assert!(fit(&["users", "me"], &route_segs("users/{user}")).is_some());
    }

    #[test]
    fn additional_keys_count_as_returned() {
        let shape = ResponseShape {
            kind: "collection".into(),
            complete: true,
            fields: vec![codemap_laravel::responses::ShapeField {
                name: "rating".into(),
                ..Default::default()
            }],
            additional: vec![codemap_laravel::responses::ShapeField {
                name: "summary".into(),
                ..Default::default()
            }],
            ..ResponseShape::default()
        };
        let read = CallRow {
            fields: vec!["data.summary.count".into(), "data.data[].rating".into()],
            ..CallRow::default()
        };
        assert!(not_returned(&read, &shape).is_empty());
        let wrong = CallRow {
            fields: vec!["data.totals".into()],
            ..CallRow::default()
        };
        assert_eq!(not_returned(&wrong, &shape), ["totals"]);
    }

    #[test]
    fn fitting() {
        let r = route_segs("api/v1/orders/{order}/items/{item?}");
        assert_eq!(fit(&["api", "v1", "orders", "{}", "items"], &r), Some(4));
        assert_eq!(
            fit(&["api", "v1", "orders", "12", "items", "{}"], &r),
            Some(4)
        );
        assert_eq!(fit(&["api", "v1", "orders"], &r), None);
        // A variable segment fits a route parameter better than a fixed word.
        let call = ["orders", "{}"];
        assert!(
            fit_detail(&call, &route_segs("orders/{order}"))
                > fit_detail(&call, &route_segs("orders/export"))
        );
        assert_eq!(
            fit(
                &["api", "v1", "{}", "{}", "items"],
                &route_segs("api/v1/orders/{order}/items")
            ),
            Some(3)
        );
        assert_eq!(segments("http://localhost:8000/api/v1"), ["api", "v1"]);
        assert_eq!(segments("/api"), ["api"]);
        assert!(pattern_fits("duplicate-{}", "duplicate-budget"));
        assert!(pattern_fits(
            "{}fixed-asset-schedule",
            "monthly-fixed-asset-schedule"
        ));
        assert!(pattern_fits("{}stock-{}", "stock-in"));
        assert!(!pattern_fits("duplicate-{}", "copy-budget"));
        assert!(pattern_fits("{}-permit", "work-permit"));
        assert!(is_placeholder("/_api_url_/{}"));
        assert!(is_placeholder("/unitURL"));
        assert!(is_placeholder("/api_lookup_url"));
        assert!(is_placeholder("/admin/data-setup/_moduleUrl/{}"));
        assert!(!is_placeholder("/selfcare/e-form/unloading-permit-url"));
        assert!(!is_placeholder("/admin/lookup/payment-gateway"));
        assert!(!pattern_fits("{}-permit", "spfm-work-permit"));
        assert_eq!(
            fit(
                &["admin", "budget", "duplicate-{}"],
                &route_segs("admin/budget/duplicate-group")
            ),
            Some(2)
        );
    }
}
