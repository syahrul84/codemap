//! Blade views: which template is which view, what shows it (another
//! template, a controller, a mailable, `Route::view`) and what it needs.
//!
//! The templates' own facts are read by `codemap_php::blade` when a file is
//! parsed; this module turns them into the shared `views` / `view_uses` rows
//! (framework `blade`) and answers `get_component` for a view.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use codemap_core::store::{get_meta, set_meta};
use codemap_core::views::{Use, View};
use codemap_core::{Result, Store};
use codemap_php::blade::BladeFacts;
use rusqlite::{Connection, params};
use serde_json::{Value, json};

pub const FRAMEWORK: &str = "blade";
/// Bump when the rows `rebuild` writes change.
const VIEWS_VERSION: &str = "1";
const META_KEY: &str = "blade_views";
const SITES_KEY: &str = "blade_render_sites";
const CLASSES_KEY: &str = "blade_component_files";

/// One template.
struct Template {
    file_id: i64,
    path: String,
    name: String,
    facts: BladeFacts,
}

/// `resources/views/orders/index.blade.php` → `orders.index`. The part after
/// the last `views/` folder, so module and package views work the same.
pub fn view_name(path: &str) -> String {
    let p = path.strip_suffix(".blade.php").unwrap_or(path);
    let rel = match p.rfind("/views/") {
        Some(i) => &p[i + 7..],
        None => p.strip_prefix("views/").unwrap_or(p),
    };
    rel.replace('/', ".")
}

fn load(conn: &Connection) -> Result<Vec<Template>> {
    let mut stmt =
        conn.prepare_cached("SELECT file_id, path, facts_json FROM blade_files ORDER BY path")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (file_id, path, json) = row?;
        out.push(Template {
            file_id,
            name: view_name(&path),
            path,
            facts: serde_json::from_str(&json).unwrap_or_default(),
        });
    }
    Ok(out)
}

/// `alert-box` → `AlertBox`.
fn pascal(part: &str) -> String {
    part.split(['-', '_'])
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        })
        .collect()
}

/// Where a name written in a template or in PHP leads.
struct Index<'a> {
    by_name: HashMap<String, Vec<&'a Template>>,
    all: &'a [Template],
}

impl<'a> Index<'a> {
    fn new(all: &'a [Template]) -> Self {
        let mut by_name: HashMap<String, Vec<&Template>> = HashMap::new();
        for t in all {
            by_name.entry(t.name.to_lowercase()).or_default().push(t);
        }
        Self { by_name, all }
    }

    /// The templates a view name means: `orders.index`, `shop::orders.index`
    /// (a module or package namespace) or `pages.*` (built at run time).
    fn views(&self, raw: &str) -> Vec<&'a Template> {
        let (ns, name) = match raw.split_once("::") {
            Some((ns, n)) => (Some(ns.to_lowercase()), n),
            None => (None, raw),
        };
        if let Some(prefix) = name.strip_suffix('*') {
            let prefix = prefix.to_lowercase();
            return self
                .all
                .iter()
                .filter(|t| t.name.to_lowercase().starts_with(&prefix))
                .collect();
        }
        let key = name.to_lowercase();
        let mut hits: Vec<&Template> = self.by_name.get(&key).cloned().unwrap_or_default();
        if let Some(ns) = &ns {
            // A published package view: `resources/views/vendor/<ns>/…`.
            if let Some(v) = self.by_name.get(&format!("vendor.{ns}.{key}")) {
                hits.extend(v.iter().copied());
            }
            let of_ns: Vec<&Template> = hits
                .iter()
                .copied()
                .filter(|t| t.path.to_lowercase().contains(ns.as_str()))
                .collect();
            if !of_ns.is_empty() {
                return of_ns;
            }
        }
        hits
    }

    /// As `views`, but nothing for a name built at run time: such a use is
    /// stored once under its pattern (`pages.*`), not once per view it may mean.
    fn known(&self, raw: &str) -> Vec<&'a Template> {
        if raw.ends_with('*') {
            Vec::new()
        } else {
            self.views(raw)
        }
    }

    /// `x-forms.input` → `components.forms.input` (or its `index`).
    fn component_views(&self, tag: &str) -> Vec<&'a Template> {
        let name = tag.rsplit("::").next().unwrap_or(tag);
        for candidate in [
            format!("components.{name}"),
            format!("components.{name}.index"),
        ] {
            let v = self.views(&candidate);
            if !v.is_empty() {
                return v;
            }
        }
        Vec::new()
    }
}

/// The project's component classes (`…\\View\\Components\\…`, `…\\Livewire\\…`),
/// loaded once: a template can use hundreds of tags.
struct Classes(Vec<(String, String, u32)>);

impl Classes {
    fn load(conn: &Connection) -> Result<Self> {
        let mut stmt = conn.prepare_cached(
            "SELECT s.fqn, f.path, s.start_line FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE s.kind = 'class' AND (s.fqn LIKE '%!\\View!\\Components!\\%' ESCAPE '!'
                OR s.fqn LIKE '%Livewire!\\%' ESCAPE '!')
             ORDER BY length(s.fqn)",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?.to_lowercase(),
                r.get::<_, String>(1)?,
                r.get::<_, u32>(2)?,
            ))
        })?;
        Ok(Self(rows.collect::<rusqlite::Result<_>>()?))
    }

    /// The class behind `x-forms.input` (`…\\View\\Components\\Forms\\Input`) or
    /// `livewire:cart-badge` (`…\\Livewire\\CartBadge`): its file and line.
    fn find(&self, folder: &str, name: &str) -> Option<(String, u32)> {
        let name = name.rsplit("::").next().unwrap_or(name);
        let class: Vec<String> = name.split('.').map(pascal).collect();
        let suffix = format!("\\{folder}\\{}", class.join("\\")).to_lowercase();
        self.0
            .iter()
            .find(|(fqn, _, _)| fqn.ends_with(&suffix))
            .map(|(_, path, line)| (path.clone(), *line))
    }
}

/// Where a `<x-…>` / `<livewire:…>` tag leads: a class, else a template.
fn component_target(classes: &Classes, ix: &Index<'_>, tag: &str) -> Option<(String, u32)> {
    if let Some(name) = tag.strip_prefix("livewire:") {
        return classes.find("Livewire", name).or_else(|| {
            ix.views(&format!("livewire.{name}"))
                .first()
                .map(|t| (t.path.clone(), 1))
        });
    }
    let name = tag.strip_prefix("x-").unwrap_or(tag);
    if let Some(t) = ix.component_views(name).first() {
        return Some((t.path.clone(), 1));
    }
    classes.find("View\\Components", name)
}

/// Rewrites the `views` and `view_uses` rows of every Blade template.
pub fn rebuild(store: &mut Store) -> Result<usize> {
    let tx = store.conn_mut().transaction()?;
    let templates = load(&tx)?;
    let ix = Index::new(&templates);
    let classes = Classes::load(&tx)?;
    let mut uses: Vec<Use> = Vec::new();
    let mut shown: HashMap<&str, (bool, bool)> = HashMap::new(); // path → (extended, included)
    for t in &templates {
        let from = |to_name: String, to_path: Option<String>, line: u32| Use {
            from_path: t.path.clone(),
            from_name: Some(t.name.clone()),
            to_name,
            to_path,
            line,
        };
        if let Some(e) = &t.facts.extends {
            let hits = ix.known(&e.name);
            for h in &hits {
                shown.entry(h.path.as_str()).or_default().0 = true;
                uses.push(from(h.name.clone(), Some(h.path.clone()), e.line));
            }
            if hits.is_empty() {
                uses.push(from(e.name.clone(), None, e.line));
            }
        }
        for inc in &t.facts.includes {
            let hits = ix.known(&inc.name);
            for h in &hits {
                shown.entry(h.path.as_str()).or_default().1 = true;
                uses.push(from(h.name.clone(), Some(h.path.clone()), inc.line));
            }
            if hits.is_empty() {
                uses.push(from(inc.name.clone(), None, inc.line));
            }
        }
        for c in &t.facts.components {
            let name = c.name.strip_prefix("x-").unwrap_or(&c.name);
            let views = if c.name.starts_with("x-") {
                ix.component_views(name)
            } else {
                ix.views(&format!(
                    "livewire.{}",
                    name.trim_start_matches("livewire:")
                ))
            };
            for h in &views {
                shown.entry(h.path.as_str()).or_default().1 = true;
                uses.push(from(h.name.clone(), Some(h.path.clone()), c.line));
            }
            if views.is_empty() {
                let target = component_target(&classes, &ix, &c.name);
                uses.push(from(c.name.clone(), target.map(|t| t.0), c.line));
            }
        }
    }
    // PHP that renders a view: controllers, mailables, components, routes.
    {
        let mut stmt = tx.prepare(
            "SELECT f.id, f.path, r.line, r.target FROM refs r JOIN files f ON f.id = r.file_id
             WHERE r.kind = 'view' ORDER BY f.path, r.line",
        )?;
        let mut owner = tx.prepare(
            "SELECT fqn FROM symbols WHERE file_id = ?1 AND kind IN ('method', 'function')
             AND start_line <= ?2 AND end_line >= ?2 ORDER BY start_line DESC LIMIT 1",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u32>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (file_id, path, line, target) = row?;
            let from_name: Option<String> = owner
                .query_map(params![file_id, line], |r| r.get(0))?
                .next()
                .transpose()?;
            let hits = ix.known(&target);
            for h in &hits {
                uses.push(Use {
                    from_path: path.clone(),
                    from_name: from_name.clone(),
                    to_name: h.name.clone(),
                    to_path: Some(h.path.clone()),
                    line,
                });
            }
            if hits.is_empty() {
                uses.push(Use {
                    from_path: path,
                    from_name,
                    to_name: target,
                    to_path: None,
                    line,
                });
            }
        }
    }
    let views: Vec<View> = templates
        .iter()
        .map(|t| {
            let (extended, included) = shown.get(t.path.as_str()).copied().unwrap_or_default();
            let last = t.name.rsplit('.').next().unwrap_or(&t.name);
            let kind = if t.path.contains("/components/") || t.name.starts_with("components.") {
                "component"
            } else if extended || t.name.starts_with("layouts.") {
                "layout"
            } else if last.starts_with('_')
                || t.path.contains("/partials/")
                || (included && t.facts.extends.is_none())
            {
                "partial"
            } else {
                "page"
            };
            View {
                file_id: t.file_id,
                name: t.name.clone(),
                path: t.path.clone(),
                line: 1,
                kind: kind.into(),
            }
        })
        .collect();
    codemap_core::views::replace(&tx, FRAMEWORK, &views, &uses)?;
    set_meta(&tx, META_KEY, VIEWS_VERSION)?;
    let n = views.len();
    tx.commit()?;
    Ok(n)
}

/// A fingerprint of every place PHP names a view: when it is the same as at
/// the last rebuild, no render site was added, removed or moved.
fn render_sites(conn: &Connection) -> Result<String> {
    let mut stmt = conn.prepare_cached(
        "SELECT f.path, r.line, r.target FROM refs r JOIN files f ON f.id = r.file_id
         WHERE r.kind = 'view' ORDER BY f.path, r.line, r.target",
    )?;
    let mut rows = stmt.query([])?;
    // FNV-1a: a change detector, not a secret.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut n = 0u64;
    let mut eat = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    while let Some(r) = rows.next()? {
        eat(r.get_ref(0)?.as_str().unwrap_or_default().as_bytes());
        eat(&r.get::<_, i64>(1)?.to_le_bytes());
        eat(r.get_ref(2)?.as_str().unwrap_or_default().as_bytes());
        eat(&[0]);
        n += 1;
    }
    Ok(format!("{n}:{h:016x}"))
}

/// Rebuilds the Blade rows when something they are built from changed: a
/// template, a component class, or a place PHP renders a view. Editing any
/// other file leaves them as they are.
pub fn refresh(store: &mut Store, scan: &codemap_core::ScanReport, full: bool) -> Result<usize> {
    let conn = store.conn();
    let any: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM blade_files) OR EXISTS (SELECT 1 FROM views WHERE framework = ?1)",
        [FRAMEWORK],
        |r| r.get(0),
    )?;
    if !any {
        return Ok(0);
    }
    let built = get_meta(conn, META_KEY)?.as_deref() == Some(VIEWS_VERSION);
    let mut due = full || !built;
    if !due {
        let mut renders = conn.prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM refs r JOIN files f ON f.id = r.file_id
             WHERE r.kind = 'view' AND f.path = ?1)",
        )?;
        // A component class is known by its name, wherever its file is:
        // the files that held one at the last rebuild, and any that hold
        // one now.
        let component_files: Vec<String> = get_meta(conn, CLASSES_KEY)?
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default();
        let mut declares = conn.prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE f.path = ?1 AND s.kind = 'class'
               AND (s.fqn LIKE '%!\\View!\\Components!\\%' ESCAPE '!' OR s.fqn LIKE '%Livewire!\\%' ESCAPE '!'))",
        )?;
        for p in scan.changed_paths.iter().chain(&scan.removed_paths) {
            if p.ends_with(".blade.php")
                || component_files.contains(p)
                || renders.query_row([p], |r| r.get::<_, bool>(0))?
                || declares.query_row([p], |r| r.get::<_, bool>(0))?
            {
                due = true;
                break;
            }
        }
    }
    let sites = render_sites(conn)?;
    if !due && get_meta(conn, SITES_KEY)?.as_deref() == Some(sites.as_str()) {
        return Ok(0);
    }
    let n = rebuild(store)?;
    let conn = store.conn();
    set_meta(conn, SITES_KEY, &sites)?;
    let files: std::collections::BTreeSet<String> = Classes::load(conn)?
        .0
        .into_iter()
        .map(|(_, path, _)| path)
        .collect();
    set_meta(conn, CLASSES_KEY, &serde_json::to_string(&files)?)?;
    Ok(n)
}

/// How many Blade views of each kind, for the overview.
pub fn overview(conn: &Connection) -> Result<Value> {
    let mut stmt = conn.prepare_cached(
        "SELECT COALESCE(kind, 'page'), COUNT(*) FROM views WHERE framework = ?1 GROUP BY 1",
    )?;
    let rows = stmt.query_map([FRAMEWORK], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?))
    })?;
    let mut kinds: BTreeMap<String, u64> = BTreeMap::new();
    for row in rows {
        let (k, n) = row?;
        kinds.insert(k, n);
    }
    if kinds.is_empty() {
        return Ok(Value::Null);
    }
    Ok(json!({
        "views": kinds.values().sum::<u64>(),
        "kinds": kinds,
        "next": "get_component with a view name (`orders.index`), tag (`x-alert`) or file for what a template extends, includes, expects and who renders it",
    }))
}

/// The keys a render call gives the view: `view('x', ['order' => $o])`,
/// `compact('order', 'items')`, `->with('order', $o)`, `->with([...])`.
/// Read from the source at the call; `$data` when an array variable is passed.
pub fn passed_keys(src: &str, line: u32) -> Vec<String> {
    let start: usize = src
        .split_inclusive('\n')
        .take(line.saturating_sub(1) as usize)
        .map(str::len)
        .sum();
    let Some(rest) = src.get(start..) else {
        return Vec::new();
    };
    // From the call's name on that line to the end of its statement.
    let from = [
        "view(",
        "loadView(",
        "make(",
        "markdown(",
        "render(",
        "send(",
        "first(",
        "view:",
        "markdown:",
    ]
    .iter()
    .filter_map(|w| rest.lines().next().and_then(|l| l.find(w)))
    .min()
    .unwrap_or(0);
    let b = rest.as_bytes();
    let mut keys: Vec<String> = Vec::new();
    let mut add = |k: String| {
        if !k.is_empty() && !keys.contains(&k) {
            keys.push(k);
        }
    };
    let (mut depth, mut i) = (0i32, from);
    let mut compact_until: Option<i32> = None;
    let mut with_at: Option<i32> = None;
    let mut first_literal = true;
    // Still inside the call that names the view (not a later `->with(…)`).
    let mut in_call = true;
    while i < b.len() && i < from + 4000 {
        match b[i] {
            b'\'' | b'"' => {
                let q = b[i];
                let mut e = i + 1;
                while e < b.len() && b[e] != q {
                    e += if b[e] == b'\\' { 2 } else { 1 };
                }
                let e = e.min(b.len());
                let lit = rest.get(i + 1..e).unwrap_or_default();
                let mut k = e + 1;
                while k < b.len() && b[k].is_ascii_whitespace() {
                    k += 1;
                }
                let arrow = rest.get(k..).is_some_and(|r| r.starts_with("=>"));
                let comma = b.get(k) == Some(&b',');
                let ident =
                    !lit.is_empty() && lit.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_');
                if first_literal {
                    // The view's own name.
                    first_literal = false;
                } else if ident
                    && (compact_until.is_some()
                        || (arrow && depth == 2)
                        || (comma && with_at == Some(depth)))
                {
                    add(lit.to_owned());
                }
                i = e + 1;
                continue;
            }
            b'(' | b'[' => {
                let word_end = i;
                let mut ws = i;
                while ws > 0 && (b[ws - 1].is_ascii_alphanumeric() || b[ws - 1] == b'_') {
                    ws -= 1;
                }
                let word = rest.get(ws..word_end).unwrap_or_default();
                depth += 1;
                if b[i] == b'(' && word == "compact" {
                    compact_until = Some(depth);
                } else if b[i] == b'(' && word == "with" {
                    with_at = Some(depth);
                } else if b[i] == b'(' && word.starts_with("with") && word.len() > 4 {
                    // `->withOrder($order)`.
                    let mut c = word[4..].chars();
                    if let Some(f) = c.next() {
                        add(f.to_lowercase().collect::<String>() + c.as_str());
                    }
                }
            }
            b')' | b']' => {
                if compact_until == Some(depth) {
                    compact_until = None;
                }
                if with_at == Some(depth) {
                    with_at = None;
                }
                depth -= 1;
                if depth <= 0 {
                    in_call = false;
                }
            }
            b'$' if depth == 1 && in_call && !first_literal => {
                // `view('x', $data)`: the whole array comes from a variable.
                let mut e = i + 1;
                while e < b.len() && (b[e].is_ascii_alphanumeric() || b[e] == b'_') {
                    e += 1;
                }
                let mut k = e;
                while k < b.len() && b[k].is_ascii_whitespace() {
                    k += 1;
                }
                let mut p = i;
                while p > 0 && b[p - 1].is_ascii_whitespace() {
                    p -= 1;
                }
                if p > 0 && b[p - 1] == b',' && matches!(b.get(k), Some(b')' | b',')) {
                    add(rest[i..e].to_owned());
                }
                i = e;
                continue;
            }
            b';' if depth <= 0 => break,
            _ => {}
        }
        i += 1;
    }
    keys
}

fn cut(mut list: Vec<String>, full: bool, max: usize) -> Vec<String> {
    if !full && list.len() > max {
        let more = list.len() - max;
        list.truncate(max);
        list.push(format!("… {more} more (detail: \"full\" for all)"));
    }
    list
}

/// The view `query` names: a path, a view name (`orders.index`,
/// `orders/index`, `shop::orders.index`) or a component tag (`x-alert`).
/// `None` when the project has no Blade templates or nothing matches.
pub fn get_view(store: &Store, root: &Path, query: &str, full: bool) -> Result<Option<Value>> {
    let conn = store.conn();
    let templates = load(conn)?;
    if templates.is_empty() {
        return Ok(None);
    }
    let ix = Index::new(&templates);
    let classes = Classes::load(conn)?;
    let q = query.trim().trim_matches(['<', '>', '/']).trim();
    let hits: Vec<&Template> = if let Some(t) = templates.iter().find(|t| t.path == q) {
        vec![t]
    } else if let Some(tag) = q.strip_prefix("x-") {
        ix.component_views(tag)
    } else {
        let name = q.strip_suffix(".blade.php").unwrap_or(q).replace('/', ".");
        let name = name.rsplit("views.").next().unwrap_or(&name).to_owned();
        let mut v = if name.ends_with('*') {
            Vec::new()
        } else {
            ix.views(&name)
        };
        if v.is_empty() && !name.contains("::") {
            let tail = format!(".{}", name.to_lowercase());
            v = templates
                .iter()
                .filter(|t| t.name.to_lowercase().ends_with(&tail))
                .collect();
        }
        v
    };
    let t = match hits.as_slice() {
        [] => return Ok(None),
        [t] => *t,
        many => {
            return Ok(Some(json!({
                "found": true,
                "framework": FRAMEWORK,
                "ambiguous": true,
                "candidates": many.iter().map(|t| format!("{}  {}", t.name, t.path)).collect::<Vec<_>>(),
                "hint": "several views match; pass one of these names or its file",
            })));
        }
    };

    let kind: Option<String> = conn
        .query_row(
            "SELECT kind FROM views WHERE framework = ?1 AND path = ?2",
            params![FRAMEWORK, t.path],
            |r| r.get(0),
        )
        .unwrap_or(None);
    let f = &t.facts;
    let mut v = json!({
        "found": true,
        "framework": FRAMEWORK,
        "view": t.name,
        "file": t.path,
        "type": kind.unwrap_or_else(|| "page".into()),
    });
    if let Some(tag) = t.name.strip_prefix("components.") {
        v["tag"] = json!(format!("<x-{}>", tag.strip_suffix(".index").unwrap_or(tag)));
    }
    let place = |name: &str, line: u32| -> String {
        match ix.views(name).as_slice() {
            [] if name.ends_with('*') => format!("{name} (name built at run time)  :{line}"),
            [] if name.contains("::") => format!("{name} (a package's view)  :{line}"),
            [] => format!("{name} (no such view in this project)  :{line}"),
            many if name.ends_with('*') => {
                format!(
                    "{name} (name built at run time: {} views fit)  :{line}",
                    many.len()
                )
            }
            [one] => format!("{name} → {}  :{line}", one.path),
            many => format!("{name} → {} views  :{line}", many.len()),
        }
    };
    if let Some(e) = &f.extends {
        v["extends"] = json!(place(&e.name, e.line));
    }
    let mut set = |key: &str, list: &[String]| {
        if !list.is_empty() {
            v[key] = json!(list);
        }
    };
    set("sections", &f.sections);
    set("yields", &f.yields);
    set("props", &f.props);
    set("slots", &f.slots);
    set("stacks", &f.stacks);
    set("pushes_to", &f.pushes);
    set("expects", &f.vars);
    if !f.includes.is_empty() {
        let list = f.includes.iter().map(|i| place(&i.name, i.line)).collect();
        v["includes"] = json!(cut(list, full, 15));
    }
    if !f.components.is_empty() {
        let mut list = Vec::new();
        for c in &f.components {
            list.push(match component_target(&classes, &ix, &c.name) {
                Some((path, _)) => format!("{} → {path}  :{}", c.name, c.line),
                None => format!(
                    "{} (not in this project: a package's?)  :{}",
                    c.name, c.line
                ),
            });
        }
        list.dedup_by(|a, b| a.rsplit("  :").nth(1) == b.rsplit("  :").nth(1));
        v["components"] = json!(cut(list, full, 15));
    }
    if !f.forms.is_empty() {
        let mut route = conn.prepare_cached(
            "SELECT methods, uri FROM routes WHERE name = ?1 ORDER BY seq LIMIT 1",
        )?;
        let mut list = Vec::new();
        for form in &f.forms {
            let mut line = format!("{} {}", form.method, form.action);
            if let Some(name) = form.action.strip_prefix("route:")
                && let Some((methods, uri)) = route
                    .query_map([name], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                    })?
                    .next()
                    .transpose()?
            {
                line.push_str(&format!(" → {} {uri}", methods.replace("GET|HEAD", "GET")));
                if !methods.split('|').any(|m| m == form.method) {
                    line.push_str(&format!(" (the route does not take {})", form.method));
                }
            }
            line.push_str(&format!("  :{}", form.line));
            list.push(line);
        }
        v["forms"] = json!(cut(list, full, 10));
    }
    {
        let mut stmt = conn.prepare_cached(
            "SELECT DISTINCT r.target FROM refs r WHERE r.file_id = ?1 AND r.kind = 'route' ORDER BY r.line",
        )?;
        let names: Vec<String> = stmt
            .query_map([t.file_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if !names.is_empty() {
            v["links_to_routes"] = json!(cut(names, full, 15));
        }
    }

    // Who shows it.
    let mut stmt = conn.prepare_cached(
        "SELECT from_path, from_name, line FROM view_uses
         WHERE framework = ?1 AND to_path = ?2 AND from_path != ?2 ORDER BY from_path, line",
    )?;
    let mut rows: Vec<(String, Option<String>, u32)> = stmt
        .query_map(params![FRAMEWORK, t.path], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    // Names built at run time that this view fits (`'pages.' . $slug`).
    let dynamic = by_pattern(conn, &t.name)?;
    let dynamic_at: Vec<(String, u32)> = dynamic.iter().map(|d| (d.0.clone(), d.2)).collect();
    rows.extend(dynamic.into_iter().map(|d| (d.0, d.1, d.2)));
    let by_path: HashMap<&str, &Template> =
        templates.iter().map(|x| (x.path.as_str(), x)).collect();
    let (mut rendered, mut extended, mut included, mut routes) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut route_of = conn.prepare_cached(
        "SELECT methods, uri, name FROM routes
         WHERE (controller_fqn = ?1 COLLATE NOCASE AND COALESCE(controller_method, '__invoke') = ?2)
            OR (file = ?3 AND line = ?4) ORDER BY seq LIMIT 6",
    )?;
    for (from_path, from_name, line) in rows {
        if let Some(src) = by_path.get(from_path.as_str()) {
            let is_layout = src
                .facts
                .extends
                .as_ref()
                .is_some_and(|e| ix.views(&e.name).iter().any(|h| h.path == t.path));
            let row = format!("{}  {from_path}:{line}", src.name);
            if is_layout && src.facts.extends.as_ref().is_some_and(|e| e.line == line) {
                extended.push(row);
            } else {
                included.push(row);
            }
            continue;
        }
        let mut row = match &from_name {
            Some(n) => format!("{n}  {from_path}:{line}"),
            None => format!("{from_path}:{line}"),
        };
        if dynamic_at.contains(&(from_path.clone(), line)) {
            row.push_str("  (name built at run time: may be this view)");
        }
        if let Ok(text) = std::fs::read_to_string(root.join(&from_path)) {
            let keys = passed_keys(&text, line);
            if !keys.is_empty() {
                row.push_str(&format!("  passes: {}", keys.join(", ")));
            }
        }
        rendered.push(row);
        let (class, method) = from_name
            .as_deref()
            .and_then(|n| n.split_once("::"))
            .unwrap_or_default();
        let found = route_of.query_map(params![class, method, from_path, line], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?;
        for r in found {
            let (methods, uri, name) = r?;
            let mut line = format!("{} {uri}", methods.replace("GET|HEAD", "GET"));
            if let Some(n) = name {
                line.push_str(&format!("  name:{n}"));
            }
            if !routes.contains(&line) {
                routes.push(line);
            }
        }
    }
    if !rendered.is_empty() {
        v["rendered_by"] = json!(cut(rendered, full, 10));
    }
    if !routes.is_empty() {
        v["routes"] = json!(cut(routes, full, 10));
    }
    if !extended.is_empty() {
        v["extended_by_count"] = json!(extended.len());
        v["extended_by"] = json!(cut(extended, full, 8));
    }
    if !included.is_empty() {
        v["shown_in"] = json!(cut(included, full, 10));
    }
    if v.get("rendered_by").is_none()
        && v.get("extended_by").is_none()
        && v.get("shown_in").is_none()
    {
        v["note"] = json!(
            "nothing in the project names this view: it may be rendered under a name built at run time, by a package, or not at all"
        );
    }
    Ok(Some(v))
}

/// `(file, Class::method, line, pattern)`.
pub type PatternUse = (String, Option<String>, u32, String);

/// Uses whose view name was built at run time and fits `name`:
/// `(file, Class::method, line, pattern)`.
pub fn by_pattern(conn: &Connection, name: &str) -> Result<Vec<PatternUse>> {
    let mut stmt = conn.prepare_cached(
        "SELECT from_path, from_name, line, to_name FROM view_uses
         WHERE framework = ?1 AND to_path IS NULL AND to_name LIKE '%*' ORDER BY from_path, line",
    )?;
    let rows = stmt.query_map([FRAMEWORK], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, u32>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    let lower = name.to_lowercase();
    let mut out = Vec::new();
    for row in rows {
        let row = row?;
        let prefix = row.3.trim_end_matches('*');
        let prefix = prefix.rsplit("::").next().unwrap_or(prefix).to_lowercase();
        if lower.starts_with(&prefix) {
            out.push(row);
        }
    }
    Ok(out)
}

/// What a change to a template reaches.
#[derive(Debug, Default)]
pub struct Reach {
    /// The view's name; `None` when `path` is not a known template.
    pub name: Option<String>,
    /// Files that include, extend or render it directly: `(file, line)`.
    pub users: Vec<(String, u32)>,
    /// PHP that renders it, or a view that shows it (through includes and
    /// layouts): `(Class::method, file, line)`.
    pub renderers: Vec<(Option<String>, String, u32)>,
}

/// Follows includes and layouts up from `path` to the PHP that renders the
/// pages it ends up in.
pub fn reach(conn: &Connection, path: &str) -> Result<Reach> {
    const MOST: usize = 60;
    let mut out = Reach {
        name: conn
            .query_row(
                "SELECT name FROM views WHERE framework = ?1 AND path = ?2",
                params![FRAMEWORK, path],
                |r| r.get(0),
            )
            .ok(),
        ..Reach::default()
    };
    if out.name.is_none() {
        return Ok(out);
    }
    let mut stmt = conn.prepare_cached(
        "SELECT from_path, from_name, line FROM view_uses
         WHERE framework = ?1 AND to_path = ?2 AND from_path != ?2 ORDER BY from_path, line",
    )?;
    let mut seen: Vec<String> = vec![path.to_owned()];
    let mut queue: Vec<String> = vec![path.to_owned()];
    while let Some(cur) = queue.pop() {
        let rows = stmt.query_map(params![FRAMEWORK, cur], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, u32>(2)?,
            ))
        })?;
        for row in rows {
            let (from, from_name, line) = row?;
            if cur == path {
                out.users.push((from.clone(), line));
            }
            if from.ends_with(".blade.php") {
                if !seen.contains(&from) && seen.len() < 400 {
                    seen.push(from.clone());
                    queue.push(from);
                }
            } else if out.renderers.len() < MOST {
                out.renderers.push((from_name, from, line));
            }
        }
    }
    Ok(out)
}

/// View names closest to `query`, for a not-found answer.
pub fn suggestions(conn: &Connection, query: &str) -> Result<Vec<String>> {
    let q = query
        .trim()
        .trim_start_matches("x-")
        .replace('/', ".")
        .to_lowercase();
    let last = q.rsplit('.').next().unwrap_or(&q).to_owned();
    let mut stmt =
        conn.prepare_cached("SELECT name FROM views WHERE framework = ?1 ORDER BY name")?;
    let names: Vec<String> = stmt
        .query_map([FRAMEWORK], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut scored: Vec<(u8, String)> = names
        .into_iter()
        .filter_map(|n| {
            let l = n.to_lowercase();
            let score = if l.contains(&q) {
                0
            } else if !last.is_empty() && l.rsplit('.').next() == Some(last.as_str()) {
                1
            } else if last.len() >= 3 && l.contains(&last) {
                2
            } else {
                return None;
            };
            Some((score, n))
        })
        .collect();
    scored.sort();
    Ok(scored.into_iter().take(6).map(|(_, n)| n).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_paths() {
        assert_eq!(
            view_name("resources/views/orders/index.blade.php"),
            "orders.index"
        );
        assert_eq!(
            view_name("Modules/Shop/Resources/views/cart/show.blade.php"),
            "cart.show"
        );
        assert_eq!(view_name("views/welcome.blade.php"), "welcome");
        assert_eq!(pascal("alert-box"), "AlertBox");
    }

    #[test]
    fn keys_given_to_a_view() {
        let src = "<?php\nreturn view('orders.index', [\n    'orders' => $orders,\n    'filters' => ['status' => $s],\n])->with('total', $total)->withCount($n);\n";
        assert_eq!(passed_keys(src, 2), ["orders", "filters", "total", "count"]);
        assert_eq!(
            passed_keys("return view('a.b', compact('order', 'items'));", 1),
            ["order", "items"]
        );
        assert_eq!(passed_keys("return view('a.b', $data);", 1), ["$data"]);
        assert_eq!(
            passed_keys("return View::make('a.b')->with(['x' => 1, 'y' => 2]);", 1),
            ["x", "y"]
        );
        assert!(passed_keys("return view('a.b');\n$z = ['k' => 1];", 1).is_empty());
        assert!(passed_keys("", 9).is_empty());
    }
}
