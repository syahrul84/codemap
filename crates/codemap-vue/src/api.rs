//! HTTP calls found in the frontend, with the response fields each one reads
//! (script reads of the result, plus template reads of the state it is
//! stored in). Matching to backend routes happens at query time.

use codemap_core::Result;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use codemap_js::{ApiCall, DeclKind, UrlTemplate};

use crate::paths::{Resolved, Resolver};
use crate::registry::{Module, Use};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallRow {
    pub file: String,
    pub line: u32,
    pub method: Option<String>,
    pub url: String,
    pub raw: String,
    pub via: String,
    pub base_var: bool,
    pub optional_tail: bool,
    pub external: bool,
    /// Paths read from the response (as written, e.g. `data.data.name`);
    /// see [`field_path`] for the field they name.
    pub fields: Vec<String>,
    /// Fields the response's TypeScript type requires (`useApi()<Mall>()`).
    pub typed: Vec<String>,
    /// Values the variable prefix can take (see `ApiCall::prefixes`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prefixes: Vec<String>,
    /// Why the prefix was not expanded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix_note: Option<String>,
    /// The object method the call sits in (see `ApiCall::in_member`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_member: Option<String>,
    /// Every full URL the call can have (see `ApiCall::variants`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<String>,
    /// See `ApiCall::variants_loose`.
    #[serde(default)]
    pub variants_loose: bool,
    /// The keys the call sends, when its payload is visible (not through a
    /// project wrapper).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sends: Option<codemap_js::facts::Sent>,
}

/// Segments that wrap a payload rather than name a field.
const WRAPPERS: &[&str] = &["data", "value", "body", "[]", "_data"];

/// Properties of JS values, not response fields.
const BUILTINS: &[&str] = &[
    "length",
    "map",
    "filter",
    "forEach",
    "find",
    "findIndex",
    "some",
    "every",
    "reduce",
    "slice",
    "includes",
    "indexOf",
    "push",
    "concat",
    "join",
    "sort",
    "toString",
    "toFixed",
    "then",
    "catch",
    "finally",
    "keys",
    "values",
    "entries",
    "trim",
    "split",
    "replace",
    "toLowerCase",
    "toUpperCase",
    "status",
    "statusText",
    "headers",
    "config",
    "ok",
    "json",
    "pending",
    "error",
    "refresh",
    "execute",
    "flatMap",
    "at",
    "pop",
    "shift",
    "splice",
];

/// `data.data.items[].name` → `items[].name`… minus wrappers → `items.name`;
/// `value.address.city` → `address.city`.
pub fn field_path(path: &str) -> Option<String> {
    let segs: Vec<&str> = path
        .split('.')
        .flat_map(|s| s.split("[]"))
        .filter(|s| !s.is_empty())
        .collect();
    let rest: Vec<&str> = segs
        .into_iter()
        .skip_while(|s| WRAPPERS.contains(s))
        .collect();
    let rest: Vec<&str> = rest.into_iter().filter(|s| !BUILTINS.contains(s)).collect();
    let first = rest.first()?;
    if first.starts_with('$') || first.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(rest.join("."))
}

fn state_name(target: &str) -> String {
    let t = target.trim_start_matches("this.");
    let t = t.strip_suffix(".value").unwrap_or(t);
    t.to_owned()
}

/// Test files (`tests/`, `__tests__`, `*.spec.*`, `*.test.*`): their HTTP
/// calls are mocks, not app traffic.
pub fn is_test_file(p: &str) -> bool {
    p.starts_with("tests/")
        || p.starts_with("test/")
        || p.contains("/tests/")
        || p.contains("/__tests__/")
        || p.contains(".spec.")
        || p.contains(".test.")
        || p.contains("/e2e/")
}

/// `a` + `name` → `a.name`; `a` + `[].name` → `a[].name`.
fn join(a: &str, rest: &str) -> String {
    let rest = rest.trim_start_matches('.');
    if rest.is_empty() {
        a.to_owned()
    } else if a.is_empty() {
        rest.to_owned()
    } else if rest.starts_with("[]") {
        format!("{a}{rest}")
    } else {
        format!("{a}.{rest}")
    }
}

/// The part of `path` under `root`: `mall.name` under `mall` → `name`,
/// `items[].name` under `items` → `[].name`.
fn under<'p>(path: &'p str, root: &str) -> Option<&'p str> {
    let rest = path.strip_prefix(root)?;
    if rest.is_empty() {
        Some("")
    } else if let Some(r) = rest.strip_prefix('.') {
        Some(r)
    } else if rest.starts_with("[]") {
        Some(rest)
    } else {
        None
    }
}

/// Follows response data across files: into child components through props,
/// and from helper functions (`fetchMalls()`) back to the screens that call them.
struct Flow<'m> {
    by_path: &'m HashMap<&'m str, &'m Module>,
    resolver: &'m Resolver<'m>,
    /// `(from file, tag)` → component file.
    children: HashMap<(&'m str, &'m str), &'m str>,
    /// Exported function name → files exporting it (for auto-imports).
    exported: HashMap<&'m str, Vec<&'m Module>>,
    prop_memo: RefCell<HashMap<(String, String), Vec<String>>>,
    expanded_memo: RefCell<HashMap<String, Rc<Vec<String>>>>,
}

impl<'m> Flow<'m> {
    fn new(
        modules: &'m [Module],
        by_path: &'m HashMap<&'m str, &'m Module>,
        resolver: &'m Resolver<'m>,
        uses: &'m [Use],
    ) -> Self {
        let children = uses
            .iter()
            .filter_map(|u| {
                u.to_path
                    .as_deref()
                    .map(|to| ((u.from_path.as_str(), u.tag.as_str()), to))
            })
            .collect();
        let mut exported: HashMap<&str, Vec<&Module>> = HashMap::new();
        for m in modules.iter().filter(|m| !is_test_file(&m.path)) {
            for d in &m.facts.script.decls {
                if d.exported && d.kind == DeclKind::Function {
                    exported.entry(d.name.as_str()).or_default().push(m);
                }
            }
        }
        Self {
            by_path,
            resolver,
            children,
            exported,
            prop_memo: RefCell::default(),
            expanded_memo: RefCell::default(),
        }
    }

    fn child(&self, m: &Module, tag: &str) -> Option<&'m Module> {
        let to = self.children.get(&(m.path.as_str(), tag))?;
        self.by_path.get(to).copied()
    }

    fn prop_names(m: &Module) -> Vec<String> {
        m.facts
            .script
            .options
            .iter()
            .flat_map(|o| o.props.iter())
            .chain(&m.facts.script.setup.props)
            .map(|p| p.name.clone())
            .collect()
    }

    /// Paths a component reads from its prop `prop` (relative to the prop),
    /// including what its own children read from it.
    fn prop_reads(&self, m: &Module, prop: &str) -> Vec<String> {
        let key = (m.path.clone(), prop.to_owned());
        if let Some(v) = self.prop_memo.borrow().get(&key) {
            return v.clone();
        }
        // Mark as in progress: a component that renders itself stops here.
        self.prop_memo.borrow_mut().insert(key.clone(), Vec::new());
        let mut out: Vec<String> = Vec::new();
        let mut add = |p: String| {
            if !p.is_empty() && !out.contains(&p) && out.len() < 200 {
                out.push(p);
            }
        };
        for p in m
            .facts
            .template_paths
            .iter()
            .chain(&m.facts.script.prop_reads)
        {
            if let Some(r) = under(p, prop) {
                add(r.to_owned());
            }
        }
        for pass in &m.facts.prop_passes {
            let Some(prefix) = under(&pass.path, prop) else {
                continue;
            };
            let Some(child) = self.child(m, &pass.tag) else {
                continue;
            };
            for (cp, r) in self.passed_reads(child, &pass.prop) {
                add(join(prefix, &join(&cp, &r)));
            }
        }
        self.prop_memo.borrow_mut().insert(key, out.clone());
        out
    }

    /// Reads of a value handed to `child` as `prop` (`""` = `v-bind="obj"`):
    /// `(key under the value, path)`.
    fn passed_reads(&self, child: &Module, prop: &str) -> Vec<(String, String)> {
        if prop.is_empty() {
            Self::prop_names(child)
                .into_iter()
                .flat_map(|cp| {
                    self.prop_reads(child, &cp)
                        .into_iter()
                        .map(move |r| (cp.clone(), r))
                })
                .collect()
        } else {
            self.prop_reads(child, prop)
                .into_iter()
                .map(|r| (String::new(), r))
                .collect()
        }
    }

    /// Template paths plus what child components read from values passed to them.
    fn expanded(&self, m: &Module) -> Rc<Vec<String>> {
        if let Some(v) = self.expanded_memo.borrow().get(&m.path) {
            return Rc::clone(v);
        }
        let mut out = m.facts.template_paths.clone();
        let mut seen: HashSet<String> = out.iter().cloned().collect();
        for pass in &m.facts.prop_passes {
            let Some(child) = self.child(m, &pass.tag) else {
                continue;
            };
            for (cp, r) in self.passed_reads(child, &pass.prop) {
                let p = join(&pass.path, &join(&cp, &r));
                if seen.insert(p.clone()) {
                    out.push(p);
                }
            }
        }
        let out = Rc::new(out);
        self.expanded_memo
            .borrow_mut()
            .insert(m.path.clone(), Rc::clone(&out));
        out
    }

    /// `fname` in `t` hands back response data (an HTTP call's result, or
    /// another function's that does).
    fn returns_data(t: &Module, fname: &str) -> bool {
        let s = &t.facts.script;
        s.api_calls
            .iter()
            .any(|c| c.in_fn.as_deref() == Some(fname))
            || s.fn_calls.iter().any(|c| c.in_fn.as_deref() == Some(fname))
    }

    /// Paths read from a call result: in script, and in the template through
    /// the state it is stored in.
    fn result_paths(
        &self,
        m: &Module,
        reads: &[String],
        stored_as: &[(String, String)],
    ) -> Vec<String> {
        let mut paths: Vec<String> = reads.to_vec();
        if stored_as.is_empty() {
            return paths;
        }
        let tpaths = self.expanded(m);
        for (target, from) in stored_as {
            let name = state_name(target);
            for tp in tpaths.iter() {
                let rest = tp
                    .strip_prefix(&format!("{name}[]."))
                    .or_else(|| tp.strip_prefix(&format!("{name}.")));
                if let Some(rest) = rest {
                    paths.push(join(from, rest));
                }
            }
        }
        paths
    }

    /// The module and function name a call to `name` in `m` runs.
    fn resolve_fn(&self, m: &'m Module, name: &str) -> Option<(&'m Module, String)> {
        if m.facts
            .script
            .decls
            .iter()
            .any(|d| d.name == name && d.kind == DeclKind::Function)
        {
            return Some((m, name.to_owned()));
        }
        if let Some(imp) = m
            .facts
            .script
            .imports
            .iter()
            .find(|i| !i.type_only && !i.dynamic && i.binds(name))
        {
            let exported = if imp.default.as_deref() == Some(name) {
                "default".to_owned()
            } else {
                imp.named.iter().find(|(_, l)| l == name)?.0.clone()
            };
            let Resolved::File(p) = self.resolver.resolve(&m.path, &imp.source) else {
                return None;
            };
            let target = self.by_path.get(p.as_str())?;
            return self.find_export(target, &exported, 0);
        }
        // Auto-imported (Nuxt `composables/`, `utils/`): one exporter only.
        match self.exported.get(name).map(Vec::as_slice) {
            Some([one]) => Some((one, name.to_owned())),
            _ => None,
        }
    }

    fn find_export(&self, t: &'m Module, name: &str, depth: u8) -> Option<(&'m Module, String)> {
        if depth > 3 {
            return None;
        }
        let decls = &t.facts.script.decls;
        if name == "default" {
            if let Some(d) = decls
                .iter()
                .find(|d| d.default_export && d.kind == DeclKind::Function)
            {
                return Some((t, d.name.clone()));
            }
        } else if decls
            .iter()
            .any(|d| d.name == name && d.exported && d.kind == DeclKind::Function)
        {
            return Some((t, name.to_owned()));
        }
        for r in &t.facts.script.reexports {
            if r.exported != name && r.local != "*" {
                continue;
            }
            let local = if r.local == "*" {
                name
            } else {
                r.local.as_str()
            };
            match &r.from {
                Some(from) => {
                    if let Resolved::File(p) = self.resolver.resolve(&t.path, from)
                        && let Some(t2) = self.by_path.get(p.as_str())
                        && let Some(hit) = self.find_export(t2, local, depth + 1)
                    {
                        return Some(hit);
                    }
                }
                None if r.local != "*" => return Some((t, local.to_owned())),
                None => {}
            }
        }
        None
    }

    /// A wrapper template for `key` in `t` or behind its re-exports.
    fn find_template(&self, t: &'m Module, key: &str, depth: u8) -> Option<&'m UrlTemplate> {
        if depth > 3 {
            return None;
        }
        if let Some(tpl) = t.facts.script.url_templates.iter().find(|u| u.key == key) {
            return Some(tpl);
        }
        let (head, rest) = match key.split_once('.') {
            Some((h, r)) => (h, Some(r)),
            None => (key, None),
        };
        for r in &t.facts.script.reexports {
            if r.exported != head && r.local != "*" {
                continue;
            }
            let local = if r.local == "*" {
                head
            } else {
                r.local.as_str()
            };
            let next = match rest {
                Some(rest) => format!("{local}.{rest}"),
                None => local.to_owned(),
            };
            let target = match &r.from {
                Some(from) => match self.resolver.resolve(&t.path, from) {
                    Resolved::File(p) => self.by_path.get(p.as_str()).copied(),
                    _ => None,
                },
                None => Some(t),
            };
            if let Some(t2) = target
                && let Some(hit) = self.find_template(t2, &next, depth + 1)
            {
                return Some(hit);
            }
        }
        None
    }

    /// The URL (and method) of a call through a project wrapper whose own
    /// HTTP call builds the URL from its parameters:
    /// `generalLedgerApi.get(endpoint, id, scope, '/journal-voucher')` with
    /// `get(endpoint, id, scope, suffix = '') { api.get(\`${endpoint}/${id}${suffix}\`) }`.
    fn wrapper_url(&self, m: &'m Module, c: &ApiCall) -> Option<(String, Option<String>)> {
        if c.args.is_empty() {
            return None;
        }
        let (obj, member) = match c.via.split_once('.') {
            Some((o, mem)) => (o, Some(mem)),
            None => (c.via.as_str(), None),
        };
        let ident = |x: &str| {
            !x.is_empty()
                && x.chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$')
        };
        if !ident(obj) || member.is_some_and(|x| !ident(x)) {
            return None;
        }
        let key_for = |name: &str| match member {
            Some(mem) => format!("{name}.{mem}"),
            None => name.to_owned(),
        };
        let tpl = match m
            .facts
            .script
            .imports
            .iter()
            .find(|i| !i.type_only && !i.dynamic && i.binds(obj))
        {
            Some(imp) => {
                let exported = if imp.default.as_deref() == Some(obj) {
                    "default".to_owned()
                } else {
                    imp.named.iter().find(|(_, l)| l == obj)?.0.clone()
                };
                let Resolved::File(p) = self.resolver.resolve(&m.path, &imp.source) else {
                    return None;
                };
                let target = self.by_path.get(p.as_str())?;
                self.find_template(target, &key_for(&exported), 0)?
            }
            None => self.find_template(m, &key_for(obj), 0)?,
        };
        let mut parts: Vec<(bool, String)> = Vec::new();
        for (is_static, t) in &tpl.parts {
            let param = (!is_static)
                .then(|| tpl.params.iter().position(|p| p == t))
                .flatten();
            match param {
                Some(i) => match c.args.get(i) {
                    Some(Some(arg)) => parts.extend(arg.iter().cloned()),
                    Some(None) => parts.push((false, t.clone())),
                    None => match tpl.defaults.get(i).cloned().flatten() {
                        Some(d) => parts.extend(d),
                        None => parts.push((false, t.clone())),
                    },
                },
                None => parts.push((*is_static, t.clone())),
            }
        }
        let mut url = String::new();
        for (is_static, t) in &parts {
            if *is_static {
                url.push_str(t);
            } else {
                url.push_str("{}");
            }
        }
        let url = url.split(['?', '#']).next().unwrap_or("");
        Some((
            codemap_js::script::normalize_url(url),
            tpl.method.clone().or_else(|| c.method.clone()),
        ))
    }

    /// Hands paths read from `fname`'s result to the HTTP calls it returns.
    fn push_through(
        &self,
        t: &'m Module,
        fname: &str,
        paths: &[String],
        depth: u8,
        extra: &mut HashMap<(String, u32), Vec<String>>,
    ) {
        if depth > 3 || paths.is_empty() {
            return;
        }
        for c in &t.facts.script.api_calls {
            if c.in_fn.as_deref() == Some(fname) {
                let e = extra.entry((t.path.clone(), c.line)).or_default();
                for p in map_through(paths, &c.returned) {
                    if !e.contains(&p) && e.len() < 200 {
                        e.push(p);
                    }
                }
            }
        }
        for fc in &t.facts.script.fn_calls {
            if fc.in_fn.as_deref() == Some(fname)
                && fc.name != fname
                && let Some((t2, f2)) = self.resolve_fn(t, &fc.name)
            {
                let mapped = map_through(paths, &fc.returned);
                self.push_through(t2, &f2, &mapped, depth + 1, extra);
            }
        }
    }
}

/// `base` with a spoofed `m`: `?PUT` (only sometimes) on `POST` is `POST|PUT`.
fn either(base: Option<&str>, m: &str) -> String {
    match m.strip_prefix('?') {
        Some(s) => {
            let mut v = vec![base.unwrap_or("POST")];
            for x in s.split('|') {
                if !v.contains(&x) {
                    v.push(x);
                }
            }
            v.join("|")
        }
        None => m.to_owned(),
    }
}

impl<'m> Flow<'m> {
    /// Resolves a call's `spoof_via` (see `ApiCall::spoof_via`).
    fn spoof(&self, m: &'m Module, via: &str, modules: &'m [Module]) -> Option<String> {
        if via.contains(';') {
            return via.split(';').find_map(|v| self.spoof(m, v, modules));
        }
        if via.starts_with('@') {
            return self.helper_spoof(m, via);
        }
        let (key, idx) = via.strip_prefix("param:")?.rsplit_once('#')?;
        let idx: u32 = idx.parse().ok()?;
        let mut methods: Vec<String> = Vec::new();
        for mm in modules {
            for sc in &mm.facts.script.spoof_calls {
                if sc.index == idx && self.calls_into(mm, &sc.callee, m, key) {
                    // Built by a helper the caller imports: that helper's method.
                    let method = if sc.method.starts_with('@') {
                        match self.helper_spoof(mm, &sc.method) {
                            Some(sp) => sp,
                            None => continue,
                        }
                    } else {
                        sc.method.clone()
                    };
                    let x = method.trim_start_matches('?').to_owned();
                    if !methods.contains(&x) {
                        methods.push(x);
                    }
                }
            }
        }
        // Other callers may pass a body without `_method`: one of the methods.
        match methods.as_slice() {
            [] => None,
            [one] => Some(format!("?{one}")),
            many => Some(format!("?{}", many.join("|"))),
        }
    }

    /// `@a,@b`: the method the first of these helpers (as seen from `m`) sets.
    fn helper_spoof(&self, m: &'m Module, markers: &str) -> Option<String> {
        markers.split(',').find_map(|h| {
            let (t, f) = self.resolve_fn(m, h.strip_prefix('@')?)?;
            t.facts
                .script
                .fn_spoofs
                .iter()
                .find(|(n, _)| *n == f)
                .map(|(_, s)| s.clone())
        })
    }

    /// Whether `callee` written in `from` is `key` of module `target`
    /// (`api.save` with `import { api } from './api'`, or a local call).
    fn calls_into(&self, from: &'m Module, callee: &str, target: &'m Module, key: &str) -> bool {
        if from.path == target.path && callee == key {
            return true;
        }
        let (obj, rest) = match callee.split_once('.') {
            Some((o, r)) => (o, Some(r)),
            None => (callee, None),
        };
        let Some(imp) = from
            .facts
            .script
            .imports
            .iter()
            .find(|i| !i.type_only && !i.dynamic && i.binds(obj))
        else {
            return false;
        };
        let exported = if imp.default.as_deref() == Some(obj) {
            "default".to_owned()
        } else {
            match imp.named.iter().find(|(_, l)| l == obj) {
                Some((e, _)) => e.clone(),
                None => return false,
            }
        };
        let Resolved::File(p) = self.resolver.resolve(&from.path, &imp.source) else {
            return false;
        };
        if p.as_str() != target.path {
            return false;
        }
        let want = match rest {
            Some(r) => format!("{exported}.{r}"),
            None => exported,
        };
        want == key
    }
}

impl crate::ts_types::TypeLookup for Flow<'_> {
    fn find(&self, module: &str, name: &str) -> Option<(codemap_js::TsTypeDef, String)> {
        let m = self.by_path.get(module)?;
        self.find_type(m, name, 0)
    }
}

impl<'m> Flow<'m> {
    /// A type named `name` as seen from `m`: its own, imported (also
    /// `import type`), or re-exported.
    fn find_type(
        &self,
        m: &'m Module,
        name: &str,
        depth: u8,
    ) -> Option<(codemap_js::TsTypeDef, String)> {
        if depth > 4 {
            return None;
        }
        if let Some(t) = m.facts.script.types.iter().find(|t| t.name == name) {
            return Some((t.clone(), m.path.clone()));
        }
        let imp = m
            .facts
            .script
            .imports
            .iter()
            .find(|i| !i.dynamic && i.binds(name));
        let (source, exported) = match imp {
            Some(i) => (
                i.source.clone(),
                i.named
                    .iter()
                    .find(|(_, l)| l == name)
                    .map(|(o, _)| o.clone())
                    .unwrap_or_else(|| name.to_owned()),
            ),
            None => {
                // Re-exported from here: `export type { Mall } from './malls'`.
                let r = m
                    .facts
                    .script
                    .reexports
                    .iter()
                    .find(|r| (r.exported == name || r.local == "*") && r.from.is_some())?;
                (
                    r.from.clone()?,
                    if r.local == "*" {
                        name.to_owned()
                    } else {
                        r.local.clone()
                    },
                )
            }
        };
        let Resolved::File(p) = self.resolver.resolve(&m.path, &source) else {
            return None;
        };
        let target = self.by_path.get(p.as_str())?;
        self.find_type(target, &exported, depth + 1)
    }
}

/// Paths read from a function's return value, as paths of the result it was
/// built from: `malls.name` with `("malls", "data")` → `data.name`.
fn map_through(paths: &[String], returned: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for (to, from) in returned {
        for p in paths {
            let rest = if to.is_empty() {
                Some(p.as_str())
            } else {
                under(p, to)
            };
            if let Some(rest) = rest {
                let m = join(from, rest);
                if !m.is_empty() && !out.contains(&m) {
                    out.push(m);
                }
            }
        }
    }
    out
}

pub fn rows(
    modules: &[Module],
    by_path: &HashMap<&str, &Module>,
    resolver: &Resolver<'_>,
    uses: &[Use],
) -> Vec<CallRow> {
    let flow = Flow::new(modules, by_path, resolver, uses);
    // Reads made by the callers of helper functions that return response data.
    let mut extra: HashMap<(String, u32), Vec<String>> = HashMap::new();
    for m in modules.iter().filter(|m| !is_test_file(&m.path)) {
        for fc in &m.facts.script.fn_calls {
            let Some((t, f)) = flow.resolve_fn(m, &fc.name) else {
                continue;
            };
            if !Flow::returns_data(t, &f) {
                continue;
            }
            let paths = flow.result_paths(m, &fc.reads, &fc.stored_as);
            flow.push_through(t, &f, &paths, 0, &mut extra);
        }
    }
    let mut out = Vec::new();
    for m in modules.iter().filter(|m| !is_test_file(&m.path)) {
        for c in &m.facts.script.api_calls {
            let mut paths = flow.result_paths(m, &c.reads, &c.stored_as);
            if let Some(more) = extra.remove(&(m.path.clone(), c.line)) {
                paths.extend(more);
            }
            let mut fields: Vec<String> = paths
                .into_iter()
                .filter(|p| field_path(p).is_some())
                .collect();
            fields.sort();
            fields.dedup();
            let wrapped = flow.wrapper_url(m, c);
            let via_wrapper = wrapped.is_some();
            let (url, method, base_var, optional_tail) = match wrapped {
                Some((url, method)) => (url, method, false, false),
                None => (c.url.clone(), c.method.clone(), c.base_var, c.optional_tail),
            };
            // A method spoofed by a helper in another file, or by the callers
            // that pass the body this call posts.
            let method = match c.spoof_via.as_deref() {
                Some(via) if !via_wrapper => match flow.spoof(m, via, modules) {
                    Some(sp) => Some(either(method.as_deref(), &sp)),
                    None => method,
                },
                _ => method,
            };
            let typed = c
                .type_arg
                .as_deref()
                .map(|t| crate::ts_types::required_paths(t, &m.path, &flow))
                .unwrap_or_default();
            out.push(CallRow {
                sends: if via_wrapper { None } else { c.sends.clone() },
                in_member: c.in_member.clone(),
                variants_loose: c.variants_loose,
                variants: if via_wrapper {
                    Vec::new()
                } else {
                    c.variants.clone()
                },
                typed,
                prefixes: if base_var {
                    c.prefixes.clone()
                } else {
                    Vec::new()
                },
                prefix_note: if base_var {
                    c.prefix_note.clone()
                } else {
                    None
                },
                file: m.path.clone(),
                line: c.line,
                method,
                url,
                raw: c.raw.clone(),
                via: c.via.clone(),
                base_var,
                optional_tail,
                external: c.external,
                fields,
            });
        }
    }
    out
}

pub fn store(tx: &rusqlite::Transaction<'_>, rows: &[CallRow]) -> Result<()> {
    tx.execute("DELETE FROM api_calls", [])?;
    let mut ins = tx.prepare(
        "INSERT INTO api_calls (file, line, method, url, raw, via, base_var, external, fields_json, typed_json, prefixes_json, sends_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )?;
    for r in rows {
        ins.execute(params![
            r.file,
            r.line,
            r.method,
            r.url,
            r.raw,
            r.via,
            i64::from(r.base_var) | (i64::from(r.optional_tail) << 1),
            r.external,
            (!r.fields.is_empty()).then(|| serde_json::to_string(&r.fields).unwrap_or_default()),
            (!r.typed.is_empty()).then(|| serde_json::to_string(&r.typed).unwrap_or_default()),
            (!r.prefixes.is_empty()
                || r.prefix_note.is_some()
                || !r.variants.is_empty()
                || r.in_member.is_some())
            .then(|| {
                serde_json::json!({ "values": r.prefixes, "note": r.prefix_note, "variants": r.variants, "loose": r.variants_loose, "member": r.in_member })
                    .to_string()
            }),
            r.sends
                .as_ref()
                .map(|s| serde_json::to_string(s).unwrap_or_default())
        ])?;
    }
    Ok(())
}

pub fn load(conn: &rusqlite::Connection) -> Result<Vec<CallRow>> {
    let mut stmt = conn.prepare(
        "SELECT file, line, method, url, raw, via, base_var, external, fields_json, typed_json, prefixes_json, sends_json FROM api_calls ORDER BY file, line",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(CallRow {
            file: r.get(0)?,
            line: r.get(1)?,
            method: r.get(2)?,
            url: r.get(3)?,
            raw: r.get(4)?,
            via: r.get(5)?,
            base_var: r.get::<_, i64>(6)? & 1 == 1,
            optional_tail: r.get::<_, i64>(6)? & 2 == 2,
            external: r.get(7)?,
            fields: r
                .get::<_, Option<String>>(8)?
                .and_then(|j| serde_json::from_str(&j).ok())
                .unwrap_or_default(),
            typed: r
                .get::<_, Option<String>>(9)?
                .and_then(|j| serde_json::from_str(&j).ok())
                .unwrap_or_default(),
            prefixes: Vec::new(),
            prefix_note: None,
            variants: Vec::new(),
            in_member: None,
            variants_loose: false,
            sends: r
                .get::<_, Option<String>>(11)?
                .and_then(|j| serde_json::from_str(&j).ok()),
        })
        .map(|mut row| {
            if let Ok(Some(j)) = r.get::<_, Option<String>>(10)
                && let Ok(v) = serde_json::from_str::<serde_json::Value>(&j)
            {
                row.prefixes = v["values"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect();
                row.prefix_note = v["note"].as_str().map(str::to_owned);
                row.in_member = v["member"].as_str().map(str::to_owned);
                row.variants_loose = v["loose"].as_bool().unwrap_or(false);
                row.variants = v["variants"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect();
            }
            row
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_paths() {
        assert_eq!(field_path("data.data.name").as_deref(), Some("name"));
        assert_eq!(
            field_path("value.address.city").as_deref(),
            Some("address.city")
        );
        assert_eq!(
            field_path("data.items[].name").as_deref(),
            Some("items.name")
        );
        assert_eq!(field_path("data.data.length"), None);
        assert_eq!(field_path("data"), None);
        assert_eq!(field_path("data.meta.total").as_deref(), Some("meta.total"));
    }
}
