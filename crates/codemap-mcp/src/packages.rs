//! Installed packages (composer, npm): the version in use, which code uses
//! each one, and where it is wired up outside the code (config, Docker,
//! process managers, scripts). Answers "is this installed?", "what uses
//! it?" and "can it go?" without reading the manifests and lock files.
//! Asked through `project_overview` (`packages`, `package`): the number of
//! tools is kept small on purpose.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use codemap_core::Store;
use serde_json::{Value, json};

use crate::tools::{Output, Project, ToolError};

type R<T> = Result<T, ToolError>;

/// What `project_overview` asks about packages.
#[derive(Debug, Default)]
pub struct PackagesQuery {
    /// One package by its name, or part of a name.
    pub name: Option<String>,
    /// Only the packages nothing references.
    pub unused: bool,
}

#[derive(Debug, Default, Clone)]
struct Package {
    name: String,
    /// `composer` or `npm`.
    manager: &'static str,
    /// The constraint in the manifest, for a direct dependency.
    declared: Option<String>,
    /// The manifest section (`require-dev`, `devDependencies`, …).
    section: Option<&'static str>,
    installed: Option<String>,
    /// PHP namespaces the package autoloads.
    namespaces: Vec<String>,
    /// Class aliases Laravel registers for it (`PDF`).
    aliases: Vec<String>,
    /// Service providers Laravel discovers by itself.
    providers: Vec<String>,
    /// Programs it installs (`vendor/bin/…`, `node_modules/.bin/…`).
    bins: Vec<String>,
    /// What the packages it is installed for (a dependency of a dependency).
    required_by: Vec<String>,
}

impl Package {
    fn dev(&self) -> bool {
        matches!(self.section, Some("require-dev" | "devDependencies"))
    }
}

fn read_json(path: &Path) -> Option<Value> {
    // A lock file of a large app is a few MB; anything far beyond is not one.
    if std::fs::metadata(path).ok()?.len() > 64 * 1024 * 1024 {
        return None;
    }
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn strings(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a
            .iter()
            .filter_map(|x| x.as_str().map(str::to_owned))
            .collect(),
        Value::Object(o) => o.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

/// The last path part of a program (`bin/echo-server` → `echo-server`).
fn bin_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_owned()
}

/// Composer's platform requirements: not packages.
fn is_platform(name: &str) -> bool {
    name == "php"
        || name == "hhvm"
        || name == "composer"
        || name.starts_with("ext-")
        || name.starts_with("lib-")
        || name.starts_with("composer-")
        || !name.contains('/')
}

/// Every composer package: the direct ones from `composer.json`, what is
/// installed from `composer.lock`.
fn composer(root: &Path) -> Vec<Package> {
    let Some(manifest) = read_json(&root.join("composer.json")) else {
        return Vec::new();
    };
    let mut by_name: BTreeMap<String, Package> = BTreeMap::new();
    for section in ["require", "require-dev"] {
        for (name, constraint) in manifest[section].as_object().into_iter().flatten() {
            if is_platform(name) {
                continue;
            }
            by_name.insert(
                name.to_ascii_lowercase(),
                Package {
                    name: name.clone(),
                    manager: "composer",
                    declared: Some(constraint.as_str().unwrap_or_default().to_owned()),
                    section: Some(if section == "require" {
                        "require"
                    } else {
                        "require-dev"
                    }),
                    ..Package::default()
                },
            );
        }
    }
    let lock = read_json(&root.join("composer.lock")).unwrap_or(Value::Null);
    let mut requires: Vec<(String, Vec<String>)> = Vec::new();
    for list in ["packages", "packages-dev"] {
        for p in lock[list].as_array().into_iter().flatten() {
            let Some(name) = p["name"].as_str() else {
                continue;
            };
            let entry = by_name
                .entry(name.to_ascii_lowercase())
                .or_insert_with(|| Package {
                    name: name.to_owned(),
                    manager: "composer",
                    ..Package::default()
                });
            entry.installed = p["version"].as_str().map(str::to_owned);
            for kind in ["psr-4", "psr-0"] {
                for ns in p["autoload"][kind].as_object().into_iter().flatten() {
                    if !ns.0.is_empty() && !entry.namespaces.contains(ns.0) {
                        entry.namespaces.push(ns.0.clone());
                    }
                }
            }
            entry.bins = strings(&p["bin"]).iter().map(|b| bin_name(b)).collect();
            entry.providers = strings(&p["extra"]["laravel"]["providers"]);
            entry.aliases = strings(&p["extra"]["laravel"]["aliases"]);
            requires.push((
                name.to_owned(),
                p["require"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(k, _)| k.to_ascii_lowercase())
                    .collect(),
            ));
        }
    }
    for (name, needs) in requires {
        for n in needs {
            if let Some(p) = by_name.get_mut(&n)
                && !p.required_by.contains(&name)
            {
                p.required_by.push(name.clone());
            }
        }
    }
    by_name.into_values().collect()
}

/// A version from `yarn.lock` (`name@range:` then `version "1.2.3"`) or
/// `pnpm-lock.yaml` (`name:` then `version: 1.2.3`, or `name: 1.2.3`).
fn locked_version(text: &str, name: &str) -> Option<String> {
    let clean = |v: &str| {
        let v = v.trim().trim_matches(['"', '\'']);
        let v = v.split('(').next().unwrap_or(v);
        v.chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit())
            .then(|| v.to_owned())
    };
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let t = line.trim().trim_end_matches(':');
        let key = t.trim_matches(['"', '\'']);
        // `vue@^3.5.0, vue@^3.4.0` (yarn) or `vue` (pnpm).
        let names_it = key == name
            || key.split(", ").any(|k| {
                k.trim_matches('"')
                    .rsplit_once('@')
                    .is_some_and(|(n, _)| n == name)
            });
        if names_it && line.trim_end().ends_with(':') {
            for _ in 0..3 {
                let Some(next) = lines.peek() else { break };
                let n = next.trim();
                if let Some(v) = n
                    .strip_prefix("version:")
                    .or_else(|| n.strip_prefix("version "))
                {
                    return clean(v);
                }
                if !n.starts_with("specifier") && !n.starts_with("resolved") {
                    break;
                }
                lines.next();
            }
        } else if let Some(v) = line
            .trim()
            .strip_prefix(&format!("{name}: "))
            .or_else(|| line.trim().strip_prefix(&format!("'{name}': ")))
            && let Some(v) = clean(v)
        {
            return Some(v);
        }
    }
    None
}

/// Every npm package the manifest names, with what is installed.
fn npm(root: &Path) -> Vec<Package> {
    let Some(manifest) = read_json(&root.join("package.json")) else {
        return Vec::new();
    };
    let mut out: Vec<Package> = Vec::new();
    for section in [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ] {
        for (name, range) in manifest[section].as_object().into_iter().flatten() {
            if out.iter().any(|p| p.name == *name) {
                continue;
            }
            out.push(Package {
                name: name.clone(),
                manager: "npm",
                declared: Some(range.as_str().unwrap_or_default().to_owned()),
                section: Some(match section {
                    "dependencies" => "dependencies",
                    "devDependencies" => "devDependencies",
                    "optionalDependencies" => "optionalDependencies",
                    _ => "peerDependencies",
                }),
                ..Package::default()
            });
        }
    }
    if out.is_empty() {
        return out;
    }
    let lock = read_json(&root.join("package-lock.json"));
    let text_lock = ["yarn.lock", "pnpm-lock.yaml"]
        .iter()
        .find_map(|f| std::fs::read_to_string(root.join(f)).ok());
    for p in &mut out {
        // What is on disk first: it is what runs.
        if let Some(own) = read_json(&root.join("node_modules").join(&p.name).join("package.json"))
        {
            p.installed = own["version"].as_str().map(str::to_owned);
            p.bins = match &own["bin"] {
                Value::String(_) => vec![bin_name(&p.name)],
                Value::Object(o) => o.keys().cloned().collect(),
                _ => Vec::new(),
            };
        }
        if p.installed.is_none()
            && let Some(lock) = &lock
        {
            let entry = &lock["packages"][format!("node_modules/{}", p.name)];
            p.installed = entry["version"]
                .as_str()
                .or_else(|| lock["dependencies"][&p.name]["version"].as_str())
                .map(str::to_owned);
            if p.bins.is_empty() {
                p.bins = strings(&entry["bin"]);
            }
        }
        if p.installed.is_none()
            && let Some(text) = &text_lock
        {
            p.installed = locked_version(text, &p.name);
        }
    }
    out
}

/// An npm package that is not in the manifest but installed for another
/// (`name` as asked), from `package-lock.json`.
fn npm_transitive(root: &Path, name: &str) -> Option<Package> {
    let lock = read_json(&root.join("package-lock.json"))?;
    let packages = lock["packages"].as_object()?;
    let key = format!("node_modules/{name}");
    let entry = packages
        .iter()
        .find(|(k, _)| **k == key || k.ends_with(&format!("/{key}")))?
        .1;
    let mut required_by: Vec<String> = packages
        .iter()
        .filter(|(_, v)| {
            ["dependencies", "peerDependencies", "optionalDependencies"]
                .iter()
                .any(|s| v[*s].get(name).is_some())
        })
        .map(|(k, _)| match k.rsplit_once("node_modules/") {
            Some((_, n)) => n.to_owned(),
            None => "this project".to_owned(),
        })
        .collect();
    required_by.sort();
    required_by.dedup();
    Some(Package {
        name: name.to_owned(),
        manager: "npm",
        installed: entry["version"].as_str().map(str::to_owned),
        bins: strings(&entry["bin"]),
        required_by,
        ..Package::default()
    })
}

/// The files that use a package in code: `(file, first line)`.
fn used_in(store: &Store, p: &Package) -> R<Vec<(String, u32)>> {
    let conn = store.conn();
    let mut out: BTreeMap<String, u32> = BTreeMap::new();
    let mut add = |sql: &str, arg: &str| -> R<()> {
        let mut stmt = conn.prepare_cached(sql)?;
        let rows = stmt.query_map([arg], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?)))?;
        for row in rows {
            let (file, line) = row?;
            let at = out.entry(file).or_insert(line);
            *at = (*at).min(line);
        }
        Ok(())
    };
    if p.manager == "npm" {
        add(
            "SELECT f.path, MIN(r.line) FROM refs r JOIN files f ON f.id = r.file_id
             WHERE r.kind = 'package' AND r.target = ?1 GROUP BY f.path",
            &p.name,
        )?;
    } else {
        const UNDER: &str =
            "SELECT f.path, MIN(r.line) FROM refs r JOIN files f ON f.id = r.file_id
             WHERE r.kind IN ('class', 'call') AND r.target LIKE ?1 GROUP BY f.path";
        for ns in &p.namespaces {
            add(UNDER, &format!("{ns}%"))?;
            // The class named like the namespace itself (`Mockery::mock()`
            // beside `Mockery\MockInterface`).
            let bare = ns.trim_end_matches(['\\', '_']);
            add(
                "SELECT f.path, MIN(r.line) FROM refs r JOIN files f ON f.id = r.file_id
                 WHERE r.kind = 'class' AND r.target = ?1 GROUP BY f.path",
                bare,
            )?;
            add(UNDER, &format!("{bare}::%"))?;
        }
        // A package that only lists its class files names no namespace: by
        // convention it is the vendor's (`shuchkin/…` → `Shuchkin\…`).
        if p.namespaces.is_empty()
            && let Some((vendor, _)) = p.name.split_once('/')
        {
            add(UNDER, &format!("{vendor}\\%"))?;
        }
        // An alias Laravel registers (`PDF::loadView`), unless the project
        // has a class of that name itself.
        for alias in &p.aliases {
            if codemap_core::symbols::all_named(conn, alias)?
                .iter()
                .any(|s| s.kind.is_class_like())
            {
                continue;
            }
            add(
                "SELECT f.path, MIN(r.line) FROM refs r JOIN files f ON f.id = r.file_id
                 WHERE r.kind = 'class' AND r.target = ?1 GROUP BY f.path",
                alias,
            )?;
            add(
                "SELECT f.path, MIN(r.line) FROM refs r JOIN files f ON f.id = r.file_id
                 WHERE r.kind IN ('call', 'call_unknown') AND r.target LIKE ?1 GROUP BY f.path",
                &format!("{alias}::%"),
            )?;
        }
    }
    Ok(out.into_iter().collect())
}

/// A file outside the indexed code that may name a package.
struct Wiring {
    /// As shown: from the project's root (`../docker/web.conf` for a file
    /// of the repository around it).
    shown: String,
    /// Only these lines count (the `scripts` of a manifest); `None`: all.
    lines: Option<(usize, usize)>,
    text: String,
}

/// Folders that hold configuration and deployment files.
const WIRING_DIRS: &[&str] = &[
    "config",
    "docker",
    ".docker",
    "deploy",
    "deployment",
    "supervisor",
    "nginx",
    "scripts",
    "bin",
    "ops",
    "infra",
    "k8s",
    ".github",
    ".gitlab",
    ".circleci",
];

/// Whether a file of this name may wire a package up: configuration,
/// scripts and deployment files, never secrets, lock files or prose.
fn wiring_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    // `.env` holds secrets; its committed examples do not.
    if lower == ".env" || (lower.starts_with(".env.") && !lower.contains("example")) {
        return false;
    }
    if matches!(
        lower.as_str(),
        "composer.lock" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" | "cargo.lock"
    ) || lower.contains(".min.")
    {
        return false;
    }
    match lower.rsplit_once('.') {
        None => true, // Dockerfile, Makefile, Procfile
        Some((_, ext)) => matches!(
            ext,
            "yml"
                | "yaml"
                | "conf"
                | "cnf"
                | "ini"
                | "toml"
                | "json"
                | "sh"
                | "bash"
                | "service"
                | "xml"
                | "php"
                | "js"
                | "cjs"
                | "mjs"
                | "ts"
                | "mts"
                | "cts"
                | "example"
                | "sample"
                | "dist"
                | "tpl"
                | "j2"
                | "cfg"
                | "properties"
                | "prod"
                | "dev"
                | "local"
                | "staging"
                | "production"
        ),
    }
}

/// The lines of the top-level `"scripts"` object of a manifest.
fn scripts_block(text: &str) -> Option<(usize, usize)> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.trim_start().starts_with("\"scripts\""))?;
    let mut depth = 0i32;
    for (i, l) in lines.iter().enumerate().skip(start) {
        depth += l.matches('{').count() as i32 - l.matches('}').count() as i32;
        if depth <= 0 && (i > start || l.contains('}')) {
            return Some((start, i));
        }
    }
    Some((start, lines.len().saturating_sub(1)))
}

/// The repository's folder when the project sits inside one (a workspace
/// keeps Docker and deployment files beside its apps).
fn repo_root(root: &Path) -> Option<PathBuf> {
    root.ancestors()
        .skip(1)
        .take(3)
        .find(|d| d.join(".git").exists())
        .map(Path::to_path_buf)
}

fn wiring_files(root: &Path) -> Vec<Wiring> {
    const MAX_BYTES: u64 = 256 * 1024;
    const MAX_FILES: usize = 400;
    let mut out: Vec<Wiring> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut bases = vec![(root.to_path_buf(), String::new())];
    if let Some(outer) = repo_root(root)
        && let Ok(rel) = root.strip_prefix(&outer)
    {
        bases.push((outer, "../".repeat(rel.components().count())));
    }
    for (base, prefix) in bases {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&base)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        for dir in WIRING_DIRS {
            let dir = base.join(dir);
            if !dir.is_dir() {
                continue;
            }
            files.extend(
                ignore::WalkBuilder::new(&dir)
                    .max_depth(Some(4))
                    .hidden(false)
                    .require_git(false)
                    .build()
                    .flatten()
                    .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
                    .map(|e| e.into_path())
                    .take(MAX_FILES),
            );
        }
        files.sort();
        for path in files {
            if out.len() >= MAX_FILES {
                break;
            }
            let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                continue;
            };
            if !wiring_name(&name)
                || std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES)
                || !seen.insert(path.clone())
            {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            // A manifest lists every package: only its scripts wire one up.
            let lines = if matches!(name.as_str(), "package.json" | "composer.json") {
                match scripts_block(&text) {
                    Some(block) => Some(block),
                    None => continue,
                }
            } else {
                None
            };
            let rel = path
                .strip_prefix(&base)
                .unwrap_or(&path)
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            out.push(Wiring {
                shown: format!("{prefix}{rel}"),
                lines,
                text,
            });
        }
    }
    out
}

/// Whether `line` names `term` as a whole name: not inside a longer one
/// (`vue` in `vue-router`) and not as a file ending (`App.vue`).
fn names(line: &str, term: &str) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-');
    let mut from = 0;
    while let Some(i) = line[from..].find(term) {
        let at = from + i;
        let before = line[..at].chars().next_back();
        let after = line[at + term.len()..].chars().next();
        if before.is_none_or(|c| !word(c) && c != '.' && c != '@') && after.is_none_or(|c| !word(c))
        {
            return true;
        }
        from = at + term.len();
    }
    false
}

/// Where configuration, scripts and deployment files name the package or a
/// program it installs: `file:line  the line`.
fn wired_in(files: &[Wiring], p: &Package, cap: usize) -> Vec<String> {
    let mut terms: Vec<&str> = vec![p.name.as_str()];
    // `predis/predis`: a name of its own, which config files use bare
    // (`'client' => 'predis'`).
    if let Some((vendor, short)) = p.name.split_once('/')
        && vendor == short
    {
        terms.push(short);
    }
    terms.extend(
        p.bins
            .iter()
            .map(String::as_str)
            .filter(|b| b.len() >= 4 && *b != p.name),
    );
    terms.retain(|t| t.len() >= 3);
    let mut out = Vec::new();
    for f in files {
        for (i, line) in f.text.lines().enumerate() {
            if f.lines.is_some_and(|(a, b)| i < a || i > b) {
                continue;
            }
            if terms.iter().any(|t| names(line, t)) {
                let text: String = line.trim().chars().take(110).collect();
                out.push(format!("{}:{}  {text}", f.shown, i + 1));
                if out.len() >= cap {
                    return out;
                }
            }
        }
    }
    out
}

/// What is known about one package's use.
struct Use {
    files: Vec<(String, u32)>,
    wired: Vec<String>,
}

impl Use {
    fn referenced(&self, p: &Package, all: &[Package]) -> bool {
        !self.files.is_empty()
            || !self.wired.is_empty()
            || !p.providers.is_empty()
            || types_for(p, all).is_some()
    }
}

/// `@types/lodash` → `lodash`, when that package (or Node itself) is there.
fn types_for(p: &Package, all: &[Package]) -> Option<String> {
    let base = p.name.strip_prefix("@types/")?;
    let base = match base.split_once("__") {
        Some((scope, name)) => format!("@{scope}/{name}"),
        None => base.to_owned(),
    };
    (base == "node" || all.iter().any(|o| o.name == base)).then_some(base)
}

/// The first folders of the files, most used first: `app/Services (5), config`.
fn top_dirs(files: &[(String, u32)]) -> String {
    let mut by_dir: BTreeMap<String, usize> = BTreeMap::new();
    for (f, _) in files {
        let dir: Vec<&str> = f.split('/').take(2).collect();
        let dir = if f.matches('/').count() >= 2 {
            dir.join("/")
        } else {
            dir.first().copied().unwrap_or_default().to_owned()
        };
        *by_dir.entry(dir).or_default() += 1;
    }
    let mut dirs: Vec<(String, usize)> = by_dir.into_iter().collect();
    dirs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    dirs.iter()
        .take(3)
        .map(|(d, n)| {
            if *n > 1 {
                format!("{d} ({n})")
            } else {
                d.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn status(p: &Package, u: &Use, all: &[Package], js_known: bool) -> String {
    if !u.files.is_empty() {
        let n = u.files.len();
        return format!(
            "used in {n} file{} ({})",
            if n == 1 { "" } else { "s" },
            top_dirs(&u.files)
        );
    }
    if !u.wired.is_empty() {
        let files: Vec<&str> = u
            .wired
            .iter()
            .filter_map(|w| w.split(':').next())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .take(3)
            .collect();
        return format!("no import in code; named in {}", files.join(", "));
    }
    if !p.providers.is_empty() {
        return "no import in code; its service provider is discovered by Laravel".into();
    }
    if let Some(base) = types_for(p, all) {
        return format!("types for {base}");
    }
    if p.manager == "npm" && !js_known {
        return "use in code unknown (this app's JavaScript is not indexed); not named in config or scripts"
            .into();
    }
    // A program to run by hand or in CI leaves no trace in the code.
    if let Some(bin) = p.bins.first() {
        return format!("nothing references it; it installs the program `{bin}`");
    }
    "nothing references it".into()
}

fn line(p: &Package, status: &str) -> String {
    format!(
        "{}  {}  {}{}  → {status}",
        p.name,
        p.installed.as_deref().unwrap_or("not installed"),
        p.manager,
        if p.dev() { " dev" } else { "" },
    )
}

const NOTE: &str = "`nothing references it`: no import in the indexed code and no mention in config, scripts or deployment files. A package a tool loads by its name alone (a plugin, a preset, a compiler such as sass) reads the same, so check that before removing one.";

pub fn list_packages(store: &Store, project: &Project, p: PackagesQuery) -> R<Output> {
    let mut all = composer(&project.root);
    all.extend(npm(&project.root));
    let direct: Vec<&Package> = all.iter().filter(|x| x.declared.is_some()).collect();
    if direct.is_empty() {
        return Ok(json!({
            "found": false,
            "hint": "no composer.json or package.json with dependencies in this project",
        })
        .into());
    }
    // Whether imports of npm packages are known at all (a Laravel app with
    // a few scripts has a package.json but no indexed frontend).
    let js_known: bool = store.conn().query_row(
        "SELECT EXISTS(SELECT 1 FROM refs WHERE kind IN ('package', 'import'))",
        [],
        |r| r.get(0),
    )?;
    let files = wiring_files(&project.root);
    let usage = |pkg: &Package, cap: usize| -> R<Use> {
        Ok(Use {
            files: used_in(store, pkg)?,
            wired: wired_in(&files, pkg, cap),
        })
    };

    // One package by its exact name: everything about it.
    let wanted = p.name.as_deref().map(str::trim).filter(|n| !n.is_empty());
    if let Some(name) = wanted {
        let exact = all
            .iter()
            .find(|x| x.name.eq_ignore_ascii_case(name))
            .cloned()
            .or_else(|| npm_transitive(&project.root, name));
        if let Some(pkg) = exact {
            let u = usage(&pkg, 12)?;
            let (tests, code): (Vec<_>, Vec<_>) = u
                .files
                .iter()
                .partition(|(f, _)| codemap_core::scan::is_test_path(f));
            let at = |v: &[&(String, u32)]| -> Vec<String> {
                v.iter().take(30).map(|(f, l)| format!("{f}:{l}")).collect()
            };
            let mut v = json!({
                "found": true,
                "package": pkg.name,
                "manager": pkg.manager,
                "installed": pkg.installed,
                "status": status(&pkg, &u, &all, js_known),
            });
            match (&pkg.declared, pkg.section) {
                (Some(d), Some(section)) => {
                    v["declared"] = json!(format!(
                        "{d} ({} {section})",
                        if pkg.manager == "npm" {
                            "package.json"
                        } else {
                            "composer.json"
                        }
                    ));
                }
                _ => {
                    v["declared"] =
                        json!("not in the manifest: installed because another package needs it");
                }
            }
            if !pkg.required_by.is_empty() {
                v["required_by"] = json!(pkg.required_by);
            }
            if !code.is_empty() {
                v["used_by"] = json!(at(&code));
                v["used_by_total"] = json!(code.len());
            }
            if !tests.is_empty() {
                v["tests"] = json!(at(&tests));
            }
            if !u.wired.is_empty() {
                v["wired_in"] = json!(u.wired);
            }
            if !pkg.namespaces.is_empty() {
                v["namespaces"] = json!(pkg.namespaces);
            }
            if !pkg.bins.is_empty() {
                v["programs"] = json!(pkg.bins);
            }
            if !pkg.providers.is_empty() {
                v["discovered_by_laravel"] = json!(pkg.providers);
            }
            if !u.referenced(&pkg, &all) {
                v["note"] = json!(NOTE);
            }
            return Ok(v.into());
        }
    }

    // The direct dependencies, one line each.
    let needle = wanted.map(str::to_ascii_lowercase);
    let mut rows: Vec<(String, bool)> = Vec::new();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for pkg in &direct {
        if needle
            .as_deref()
            .is_some_and(|n| !pkg.name.to_ascii_lowercase().contains(n))
        {
            continue;
        }
        let u = usage(pkg, 4)?;
        let referenced = u.referenced(pkg, &all) || (pkg.manager == "npm" && !js_known);
        if p.unused && referenced {
            continue;
        }
        *counts.entry(pkg.manager).or_default() += 1;
        if !referenced {
            *counts.entry("not_referenced").or_default() += 1;
        }
        rows.push((line(pkg, &status(pkg, &u, &all, js_known)), referenced));
    }
    if rows.is_empty() {
        let mut v = json!({ "found": false });
        if let Some(name) = wanted {
            let near: Vec<&str> = {
                let mut scored: Vec<(f64, &str)> = all
                    .iter()
                    .map(|x| (strsim::jaro_winkler(&x.name, name), x.name.as_str()))
                    .filter(|(s, _)| *s > 0.8)
                    .collect();
                scored.sort_by(|a, b| b.0.total_cmp(&a.0));
                scored.into_iter().take(5).map(|(_, n)| n).collect()
            };
            v["hint"] = json!(format!(
                "no package named `{name}` in composer.json, package.json or their lock files"
            ));
            if !near.is_empty() {
                v["suggestions"] = json!(near);
            }
        } else {
            v["hint"] = json!("every direct dependency is referenced");
        }
        return Ok(v.into());
    }
    // A project has tens of direct dependencies, not hundreds: one answer.
    const MAX: usize = 200;
    let total = rows.len();
    let page: Vec<&str> = rows.iter().take(MAX).map(|(l, _)| l.as_str()).collect();
    let mut data = json!({
        "found": true,
        "total": total,
        "summary": counts,
        "packages": page,
    });
    if rows.iter().any(|(_, referenced)| !referenced) {
        data["note"] = json!(NOTE);
    }
    data["next"] = json!(
        "pass `package: \"name\"` for one package: the files that use it, where it is wired up, what needs it"
    );
    Ok(Output {
        data,
        truncated: total > MAX,
        next_cursor: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_counts_only_as_a_whole() {
        assert!(names(
            "command=laravel-echo-server start",
            "laravel-echo-server"
        ));
        assert!(names("  plugins: [vue()],", "vue"));
        assert!(names("node_modules/vue/dist/vue.js", "vue"));
        assert!(names("\"build\": \"vite build\"", "vite"));
        assert!(!names("import router from 'vue-router'", "vue"));
        assert!(!names("include: ['src/**/*.vue']", "vue"));
        assert!(!names("@vitejs/plugin-vue", "vue"));
        assert!(names("@nuxtjs/i18n", "@nuxtjs/i18n"));
    }

    #[test]
    fn versions_from_text_lock_files() {
        let yarn = "\"@scope/pkg@^1.0.0\":\n  version \"1.2.3\"\n\nvue@^3.4.0, vue@^3.5.0:\n  version \"3.5.12\"\n  resolved \"https://x\"\n";
        assert_eq!(locked_version(yarn, "vue").as_deref(), Some("3.5.12"));
        assert_eq!(locked_version(yarn, "@scope/pkg").as_deref(), Some("1.2.3"));
        assert_eq!(locked_version(yarn, "pkg"), None);
        let pnpm = "importers:\n  .:\n    dependencies:\n      vue:\n        specifier: ^3.5.12\n        version: 3.5.12(typescript@5.6.3)\n      '@nuxt/ui':\n        specifier: ^2\n        version: 2.18.7\n";
        assert_eq!(locked_version(pnpm, "vue").as_deref(), Some("3.5.12"));
        assert_eq!(locked_version(pnpm, "@nuxt/ui").as_deref(), Some("2.18.7"));
        let old = "dependencies:\n  axios: 1.7.7\n  vue: 3.2.0\n";
        assert_eq!(locked_version(old, "vue").as_deref(), Some("3.2.0"));
    }

    #[test]
    fn only_scripts_of_a_manifest_and_no_secrets() {
        let text = "{\n  \"name\": \"x\",\n  \"scripts\": {\n    \"dev\": \"vite\",\n    \"lint\": \"eslint .\"\n  },\n  \"dependencies\": { \"vite\": \"^5\" }\n}\n";
        assert_eq!(scripts_block(text), Some((2, 5)));
        assert!(wiring_name("docker-compose.yml"));
        assert!(wiring_name("Dockerfile"));
        assert!(wiring_name("supervisord.conf"));
        assert!(wiring_name(".env.example"));
        assert!(!wiring_name(".env"));
        assert!(!wiring_name(".env.production"));
        assert!(!wiring_name("composer.lock"));
        assert!(!wiring_name("README.md"));
    }
}
