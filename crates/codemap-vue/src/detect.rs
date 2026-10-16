//! Vue / Nuxt detection from `package.json`, installed packages or lockfiles,
//! and the build config (`nuxt.config`, `vite.config`, `vue.config.js`,
//! `tsconfig`/`jsconfig`). Every conclusion carries its reason.

use std::path::Path;

use codemap_js::literal;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A dependency with its declared range and resolved version.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    /// Declared in package.json, e.g. `^2.7.13`.
    pub declared: String,
    /// Installed (node_modules) or locked version, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl Package {
    /// Exact version when known, else the declared range.
    pub fn display(&self) -> String {
        self.version
            .clone()
            .unwrap_or_else(|| self.declared.clone())
    }

    pub fn major(&self) -> Option<u32> {
        let v = self.version.as_deref().unwrap_or(&self.declared);
        let digits: String = v
            .trim_start_matches(|c: char| !c.is_ascii_digit())
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().ok()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentDir {
    /// Relative to the project root, no trailing slash (`app/components`).
    pub path: String,
    /// Nuxt `pathPrefix` (default true): `components/base/Button.vue` → `BaseButton`.
    pub path_prefix: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

/// A Next.js app: which routers it uses.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextInfo {
    pub package: Package,
    /// `app` / `src/app` when the app router is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_dir: Option<String>,
    /// `pages` / `src/pages` when the pages router is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages_dir: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NuxtInfo {
    pub package: Package,
    pub major: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_file: Option<String>,
    /// `true` when components are auto-imported (Nuxt 3+, or Nuxt 2 with `components: true`).
    pub auto_import_components: bool,
    /// `extends` layers that live inside the project.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VueDetection {
    /// The JS frontend adapter is on: Vue, Nuxt, or React / Next.js.
    pub vue: bool,
    /// React (without Vue); `next` is set for Next.js.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub react: Option<Package>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<NextInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vue_package: Option<Package>,
    pub vue_major: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nuxt: Option<NuxtInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_name: Option<String>,
    /// Source root relative to the project (`src`, `app`, ``, `resources/js`).
    pub src_dir: String,
    /// `vite`, `vue-cli`, `nuxt`, `laravel-mix`, `webpack`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    pub typescript: bool,
    /// vuex / pinia / vue-router / vue-i18n / @nuxtjs/i18n.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub libraries: Vec<Package>,
    /// Component libraries (vuetify, @nuxt/ui, element-plus, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ui: Vec<Package>,
    /// Import aliases: `(alias, dir relative to root)`, longest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub component_dirs: Vec<ComponentDir>,
    pub reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl VueDetection {
    pub fn library(&self, name: &str) -> Option<&Package> {
        self.libraries
            .iter()
            .chain(&self.ui)
            .find(|p| p.name == name)
    }

    pub fn is_nuxt(&self) -> bool {
        self.nuxt.is_some()
    }

    /// React or Next.js rather than Vue.
    pub fn is_react(&self) -> bool {
        self.react.is_some() || self.next.is_some()
    }

    /// `src_dir` joined with `rel` (`pages` → `app/pages`).
    pub fn in_src(&self, rel: &str) -> String {
        if self.src_dir.is_empty() {
            rel.to_owned()
        } else {
            format!("{}/{rel}", self.src_dir)
        }
    }
}

const LIBRARIES: &[&str] = &[
    "vuex",
    "pinia",
    "vue-router",
    "vue-i18n",
    "@nuxtjs/i18n",
    "nuxt-i18n",
    "@pinia/nuxt",
    "@inertiajs/vue2",
    "@inertiajs/vue3",
    "@inertiajs/inertia-vue",
    "axios",
    "@nuxtjs/axios",
];

/// Component libraries and the tag prefix their components use.
pub const UI_LIBRARIES: &[(&str, &str)] = &[
    ("vuetify", "V"),
    ("@nuxt/ui", "U"),
    ("@nuxt/ui-pro", "U"),
    ("element-ui", "El"),
    ("element-plus", "El"),
    ("quasar", "Q"),
    ("bootstrap-vue", "B"),
    ("bootstrap-vue-next", "B"),
    ("ant-design-vue", "A"),
    ("naive-ui", "N"),
    ("@ionic/vue", "Ion"),
    ("primevue", ""),
    ("vue-select", ""),
    ("@headlessui/vue", ""),
    ("@nuxt/icon", "Icon"),
    ("@nuxt/image", "Nuxt"),
];

pub fn detect(root: &Path) -> VueDetection {
    let mut d = VueDetection::default();
    let Some(pkg) = read_json(&root.join("package.json"), &mut d.warnings) else {
        return d;
    };
    d.package_name = pkg["name"].as_str().map(str::to_owned);
    let declared = |name: &str| -> Option<String> {
        ["dependencies", "devDependencies", "peerDependencies"]
            .iter()
            .find_map(|k| pkg[*k][name].as_str())
            .map(str::to_owned)
    };
    let resolve = |name: &str| -> Option<Package> {
        let declared = declared(name)?;
        let version = installed_version(root, name).or_else(|| locked_version(root, name));
        Some(Package {
            name: name.to_owned(),
            declared,
            version,
        })
    };

    let nuxt = resolve("nuxt")
        .or_else(|| resolve("nuxt3"))
        .or_else(|| resolve("nuxt-edge"));
    let vue = resolve("vue");
    if vue.is_none() && nuxt.is_none() {
        // React / Next.js.
        let next = resolve("next");
        let react = resolve("react");
        if next.is_none() && react.is_none() {
            return d;
        }
        d.vue = true;
        d.typescript = declared("typescript").is_some() || root.join("tsconfig.json").is_file();
        let src = root.join("src/app").is_dir() || root.join("src/pages").is_dir();
        d.src_dir = if src { "src".into() } else { String::new() };
        if let Some(r) = &react {
            d.reasons
                .push(format!("package.json depends on react {}", r.declared));
        }
        if let Some(n) = next {
            let dir = |name: &str| {
                let rel = d.in_src(name);
                root.join(&rel).is_dir().then_some(rel)
            };
            let app_dir = dir("app");
            let pages_dir = dir("pages");
            d.reasons.push(format!(
                "Next.js {}: {}",
                n.display(),
                match (&app_dir, &pages_dir) {
                    (Some(a), Some(p)) => format!("app router ({a}) and pages router ({p})"),
                    (Some(a), None) => format!("app router ({a})"),
                    (None, Some(p)) => format!("pages router ({p})"),
                    (None, None) => "no app/ or pages/ directory found".into(),
                }
            ));
            d.build = Some("next".into());
            d.next = Some(NextInfo {
                package: n,
                app_dir,
                pages_dir,
            });
        } else {
            d.build = declared("vite").map(|_| "vite".to_owned());
        }
        d.react = react;
        d.aliases = build_aliases(root, &d.src_dir, &mut d.reasons);
        d.aliases
            .sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        d.aliases.dedup_by(|a, b| a.0 == b.0);
        return d;
    }
    d.vue = true;
    let vue_major = vue.as_ref().and_then(Package::major);
    d.vue_major = vue_major.or_else(|| {
        // Nuxt brings its own Vue: 2 → Vue 2, 3+ → Vue 3.
        nuxt.as_ref()
            .and_then(Package::major)
            .map(|n| if n >= 3 { 3 } else { 2 })
    });
    match &vue {
        Some(v) => d.reasons.push(format!(
            "package.json depends on vue {}{}",
            v.declared,
            v.version
                .as_ref()
                .map(|x| format!(" (resolved {x})"))
                .unwrap_or_default()
        )),
        None => d
            .reasons
            .push("vue comes from nuxt (not listed in package.json)".into()),
    }
    d.vue_package = vue;
    d.typescript =
        declared("typescript").is_some() || root.join("tsconfig.json").is_file() || nuxt.is_some();
    for name in LIBRARIES {
        if let Some(p) = resolve(name) {
            d.libraries.push(p);
        }
    }
    for (name, _) in UI_LIBRARIES {
        if let Some(p) = resolve(name) {
            d.ui.push(p);
        }
    }

    let nuxt_config = ["nuxt.config.ts", "nuxt.config.js", "nuxt.config.mjs"]
        .iter()
        .find(|f| root.join(f).is_file())
        .map(|f| (*f).to_owned());
    if let Some(n) = nuxt {
        d.build = Some("nuxt".into());
        let major = n.major();
        let config = nuxt_config.as_ref().and_then(|f| {
            let src = std::fs::read_to_string(root.join(f)).ok()?;
            literal::default_export(&src, f.rsplit('.').next().unwrap_or("ts"))
        });
        let config = config.unwrap_or(Value::Null);
        d.reasons.push(format!(
            "Nuxt {} ({}){}",
            n.display(),
            if n.version.is_some() {
                "installed/locked"
            } else {
                "declared range"
            },
            nuxt_config
                .as_ref()
                .map(|f| format!(", config {f}"))
                .unwrap_or_default()
        ));
        d.src_dir = nuxt_src_dir(root, major, &config, &mut d.reasons);
        let mut auto = major.is_some_and(|m| m >= 3);
        match &config["components"] {
            Value::Bool(b) => auto = *b,
            Value::Null => {}
            _ => auto = true,
        }
        d.component_dirs = if auto {
            component_dirs(&d.src_dir, &config["components"])
        } else {
            Vec::new()
        };
        if !auto {
            d.reasons.push(
                "components are not auto-imported (Nuxt 2 without `components: true`)".into(),
            );
        }
        let mut layers = Vec::new();
        for l in config["extends"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| vec![config["extends"].clone()])
        {
            let Some(l) = l.as_str() else { continue };
            let rel = l.trim_start_matches("./");
            if rel.starts_with("..") || l.contains(':') || l.starts_with('@') {
                d.warnings.push(format!(
                    "layer `{l}` is outside the project or a package; its components are not indexed"
                ));
            } else if root.join(rel).is_dir() {
                layers.push(rel.trim_end_matches('/').to_owned());
            }
        }
        for layer in &layers {
            d.component_dirs.push(ComponentDir {
                path: format!("{layer}/components"),
                path_prefix: true,
                prefix: None,
            });
        }
        d.aliases = nuxt_aliases(&d.src_dir, &config);
        d.nuxt = Some(NuxtInfo {
            package: n,
            major,
            config_file: nuxt_config,
            auto_import_components: auto,
            layers,
        });
    } else {
        let is_laravel = root.join("artisan").is_file();
        d.src_dir = if root.join("src").is_dir() {
            "src".into()
        } else if is_laravel && root.join("resources/js").is_dir() {
            "resources/js".into()
        } else {
            String::new()
        };
        d.build = if declared("vite").is_some() {
            Some("vite".into())
        } else if declared("@vue/cli-service").is_some() {
            Some("vue-cli".into())
        } else if declared("laravel-mix").is_some() {
            Some("laravel-mix".into())
        } else if declared("webpack").is_some() {
            Some("webpack".into())
        } else {
            None
        };
        d.reasons.push(format!(
            "source dir `{}` ({})",
            if d.src_dir.is_empty() {
                "."
            } else {
                &d.src_dir
            },
            d.build.as_deref().unwrap_or("unknown build tool")
        ));
        d.aliases = build_aliases(root, &d.src_dir, &mut d.reasons);
    }
    d.aliases
        .sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
    d.aliases.dedup_by(|a, b| a.0 == b.0);
    d
}

fn read_json(path: &Path, warnings: &mut Vec<String>) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            warnings.push(format!(
                "{} is not valid JSON: {e}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));
            None
        }
    }
}

fn installed_version(root: &Path, name: &str) -> Option<String> {
    let text =
        std::fs::read_to_string(root.join("node_modules").join(name).join("package.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v["version"].as_str().map(str::to_owned)
}

/// From `package-lock.json`, `pnpm-lock.yaml` or `yarn.lock`.
fn locked_version(root: &Path, name: &str) -> Option<String> {
    if let Ok(text) = std::fs::read_to_string(root.join("package-lock.json"))
        && let Ok(v) = serde_json::from_str::<Value>(&text)
    {
        let key = format!("node_modules/{name}");
        if let Some(ver) = v["packages"][&key]["version"]
            .as_str()
            .or_else(|| v["dependencies"][name]["version"].as_str())
        {
            return Some(ver.to_owned());
        }
    }
    if let Ok(text) = std::fs::read_to_string(root.join("pnpm-lock.yaml")) {
        // importers: .: dependencies: <name>: { specifier, version: 3.5.13(...) }
        let header = format!("      {name}:");
        let quoted = format!("      '{name}':");
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if *line == header || *line == quoted {
                for next in lines.iter().skip(i + 1).take(3) {
                    if let Some(v) = next.trim().strip_prefix("version: ") {
                        let v = v.split('(').next().unwrap_or(v).trim_matches(['\'', '"']);
                        return Some(v.to_owned());
                    }
                }
            }
        }
    }
    if let Ok(text) = std::fs::read_to_string(root.join("yarn.lock")) {
        let mut in_entry = false;
        for line in text.lines() {
            if !line.starts_with(' ') {
                let entry = line.trim_end_matches(':').trim_matches('"');
                in_entry = entry.split(", ").any(|e| {
                    e.trim_matches('"')
                        .rsplit_once('@')
                        .is_some_and(|(n, _)| n == name)
                });
            } else if in_entry && let Some(v) = line.trim().strip_prefix("version ") {
                return Some(v.trim_matches('"').to_owned());
            }
        }
    }
    None
}

fn nuxt_src_dir(
    root: &Path,
    major: Option<u32>,
    config: &Value,
    reasons: &mut Vec<String>,
) -> String {
    if let Some(s) = config["srcDir"].as_str() {
        let s = normalize_dir(s);
        reasons.push(format!(
            "srcDir `{}` from nuxt.config",
            if s.is_empty() { "." } else { &s }
        ));
        return s;
    }
    // Nuxt 4 uses `app/` when it exists (and falls back to the root otherwise).
    if major.is_some_and(|m| m >= 4) {
        let app = root.join("app");
        if app.is_dir()
            && ["pages", "components", "layouts", "app.vue"]
                .iter()
                .any(|p| app.join(p).exists())
        {
            reasons.push("srcDir `app` (Nuxt 4 default)".into());
            return "app".into();
        }
    }
    reasons.push("srcDir `.` (project root)".into());
    String::new()
}

/// `./app/` → `app`, `.` → ``.
fn normalize_dir(s: &str) -> String {
    let s = s.trim().trim_start_matches("./").trim_end_matches('/');
    if s == "." {
        String::new()
    } else {
        s.to_owned()
    }
}

/// Nuxt aliases `~`/`@` → srcDir and `~~`/`@@` → root, then `alias` from the config.
fn resolve_nuxt_path(src_dir: &str, p: &str) -> String {
    let join = |base: &str, rest: &str| -> String {
        let rest = rest.trim_start_matches('/');
        match (base.is_empty(), rest.is_empty()) {
            (true, _) => rest.to_owned(),
            (false, true) => base.to_owned(),
            _ => format!("{base}/{rest}"),
        }
    };
    if let Some(rest) = p.strip_prefix("~~").or_else(|| p.strip_prefix("@@")) {
        return normalize_dir(rest.trim_start_matches('/'));
    }
    if let Some(rest) = p.strip_prefix('~').or_else(|| p.strip_prefix('@')) {
        return join(src_dir, rest).trim_end_matches('/').to_owned();
    }
    join(src_dir, &normalize_dir(p))
}

fn component_dirs(src_dir: &str, config: &Value) -> Vec<ComponentDir> {
    let entries: Vec<Value> = match config {
        Value::Array(a) => a.clone(),
        Value::Object(o) => match o.get("dirs") {
            Some(Value::Array(a)) => a.clone(),
            _ => vec![Value::String("~/components".into())],
        },
        _ => vec![Value::String("~/components".into())],
    };
    entries
        .iter()
        .filter_map(|e| {
            let (path, path_prefix, prefix) = match e {
                Value::String(s) => (s.clone(), true, None),
                Value::Object(o) => (
                    o.get("path")?.as_str()?.to_owned(),
                    o.get("pathPrefix").and_then(Value::as_bool).unwrap_or(true),
                    o.get("prefix").and_then(Value::as_str).map(str::to_owned),
                ),
                _ => return None,
            };
            Some(ComponentDir {
                path: resolve_nuxt_path(src_dir, &path),
                path_prefix,
                prefix: prefix.filter(|p| !p.is_empty()),
            })
        })
        .collect()
}

fn nuxt_aliases(src_dir: &str, config: &Value) -> Vec<(String, String)> {
    let mut out = vec![
        ("~~".to_owned(), String::new()),
        ("@@".to_owned(), String::new()),
        ("~".to_owned(), src_dir.to_owned()),
        ("@".to_owned(), src_dir.to_owned()),
    ];
    if let Some(alias) = config["alias"].as_object() {
        for (k, v) in alias {
            if let Some(path) = v.as_str().or_else(|| last_string_in_expr(v)) {
                out.push((k.clone(), resolve_nuxt_path(src_dir, path)));
            }
        }
    }
    out
}

/// `@` → src for Vue CLI / Vite, plus aliases from `vite.config`, `vue.config.js`
/// (`configureWebpack.resolve.alias`) and `tsconfig`/`jsconfig` `paths`.
fn build_aliases(root: &Path, src_dir: &str, reasons: &mut Vec<String>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if !src_dir.is_empty() {
        out.push(("@".to_owned(), src_dir.to_owned()));
    }
    let add = |alias: &str, target: &str, out: &mut Vec<(String, String)>| {
        // `./*` (the project root), `./src/*`, `src`: the folder the alias points to.
        let target = target.trim_end_matches('*').trim_end_matches('/');
        let dir = normalize_dir(target.trim_start_matches(&*root.to_string_lossy()));
        let dir = dir.trim_end_matches("/*").trim_end_matches('/');
        // `.` is the project root; `../shared` stays as written.
        let dir = if dir == "." {
            String::new()
        } else {
            dir.to_owned()
        };
        let alias = alias.trim_end_matches("/*").trim_end_matches('/');
        if alias.is_empty() || alias == "*" {
            return;
        }
        out.retain(|(a, _)| a != alias);
        out.push((alias.to_owned(), dir));
    };
    for (file, path) in [
        ("vite.config.ts", &["resolve", "alias"][..]),
        ("vite.config.js", &["resolve", "alias"][..]),
        ("vite.config.mjs", &["resolve", "alias"][..]),
        (
            "vue.config.js",
            &["configureWebpack", "resolve", "alias"][..],
        ),
        ("webpack.mix.js", &["resolve", "alias"][..]),
    ] {
        let Ok(src) = std::fs::read_to_string(root.join(file)) else {
            continue;
        };
        let Some(mut v) = literal::default_export(&src, file.rsplit('.').next().unwrap_or("js"))
        else {
            continue;
        };
        for key in path {
            v = v[*key].clone();
        }
        let before = out.len();
        match &v {
            Value::Object(o) => {
                for (k, t) in o {
                    if let Some(t) = t
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| last_string_in_expr(t).map(str::to_owned))
                    {
                        add(k, &t, &mut out);
                    }
                }
            }
            // [{ find: '@', replacement: '/src' }]
            Value::Array(a) => {
                for e in a {
                    if let (Some(k), Some(t)) = (
                        e["find"].as_str(),
                        e["replacement"]
                            .as_str()
                            .or_else(|| last_string_in_expr(&e["replacement"])),
                    ) {
                        add(k, t, &mut out);
                    }
                }
            }
            _ => {}
        }
        if out.len() > before {
            reasons.push(format!("import aliases read from {file}"));
        }
    }
    for file in ["tsconfig.json", "jsconfig.json", "tsconfig.app.json"] {
        let Ok(text) = std::fs::read_to_string(root.join(file)) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&strip_json_comments(&text)) else {
            continue;
        };
        let base = normalize_dir(v["compilerOptions"]["baseUrl"].as_str().unwrap_or("."));
        if let Some(paths) = v["compilerOptions"]["paths"].as_object() {
            for (k, targets) in paths {
                if let Some(t) = targets.get(0).and_then(Value::as_str) {
                    let t = normalize_dir(t);
                    let t = if base.is_empty() {
                        t
                    } else {
                        format!("{base}/{t}")
                    };
                    if !out.iter().any(|(a, _)| a == k.trim_end_matches("/*")) {
                        add(k, &t, &mut out);
                    }
                }
            }
        }
    }
    out
}

/// `path.resolve(__dirname, 'src/api/')` → `src/api/`; `new URL('./src', import.meta.url)` → `./src`.
fn last_string_in_expr(v: &Value) -> Option<&str> {
    let text = v.get("$expr")?.as_str()?;
    let end = text.rfind(['\'', '"', '`'])?;
    let quote = text.as_bytes()[end] as char;
    let start = text[..end].rfind(quote)?;
    Some(&text[start + 1..end])
}

/// Removes `//` and `/* */` comments outside strings (tsconfig allows them).
fn strip_json_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c as char);
            if c == b'\\' && i + 1 < b.len() {
                out.push(b[i + 1] as char);
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == b'"' {
            in_str = true;
        } else if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i = s[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
            continue;
        }
        out.push(c as char);
        i += 1;
    }
    // Trailing commas are common too: drop a `,` whose next non-space is `}` / `]`.
    let chars: Vec<char> = out.chars().collect();
    let mut cleaned = String::with_capacity(out.len());
    let mut in_str = false;
    for (i, &c) in chars.iter().enumerate() {
        if c == '"' && (i == 0 || chars[i - 1] != '\\') {
            in_str = !in_str;
        }
        if c == ',' && !in_str {
            let next = chars[i + 1..].iter().find(|n| !n.is_whitespace());
            if matches!(next, Some('}' | ']')) {
                continue;
            }
        }
        cleaned.push(c);
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn not_vue() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!detect(dir.path()).vue);
        write(
            dir.path(),
            "package.json",
            r#"{"dependencies":{"axios":"^1"}}"#,
        );
        assert!(!detect(dir.path()).vue);
    }

    #[test]
    fn vue2_cli_with_aliases_and_lockfile() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "package.json",
            r#"{"name":"web","dependencies":{"vue":"^2.7.13","vuex":"^3.6.2","vuetify":"^1.5.24"},"devDependencies":{"@vue/cli-service":"^3.12.1"}}"#,
        );
        write(
            root,
            "package-lock.json",
            r#"{"packages":{"node_modules/vue":{"version":"2.7.16"}}}"#,
        );
        write(
            root,
            "vue.config.js",
            "const path = require('path')\nmodule.exports = { configureWebpack: { resolve: { alias: { Components: path.resolve(__dirname, 'src/components/'), Api: path.resolve(__dirname, 'src/api/') } } } }\n",
        );
        write(root, "src/main.js", "");
        let d = detect(root);
        assert!(d.vue && !d.is_nuxt());
        assert_eq!(d.vue_major, Some(2));
        assert_eq!(
            d.vue_package.as_ref().unwrap().version.as_deref(),
            Some("2.7.16")
        );
        assert_eq!(d.src_dir, "src");
        assert_eq!(d.build.as_deref(), Some("vue-cli"));
        assert_eq!(
            d.aliases,
            [
                ("Components".to_owned(), "src/components".to_owned()),
                ("Api".to_owned(), "src/api".to_owned()),
                ("@".to_owned(), "src".to_owned())
            ]
        );
        assert_eq!(d.ui[0].name, "vuetify");
        assert_eq!(d.library("vuex").unwrap().declared, "^3.6.2");
    }

    #[test]
    fn nuxt4_app_dir_and_component_options() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "package.json",
            r#"{"dependencies":{"nuxt":"^4.4.2","vue":"^3.5.30","@nuxt/ui":"^4"}}"#,
        );
        write(
            root,
            "pnpm-lock.yaml",
            "importers:\n  .:\n    dependencies:\n      nuxt:\n        specifier: ^4.4.2\n        version: 4.4.5(@types/node@22)\n",
        );
        write(
            root,
            "nuxt.config.ts",
            "export default defineNuxtConfig({ components: [{ path: '~/components', pathPrefix: false }, '~/widgets'], extends: ['./core', '../shared'] })",
        );
        write(root, "app/pages/index.vue", "");
        write(root, "core/components/X.vue", "");
        let d = detect(root);
        let n = d.nuxt.as_ref().unwrap();
        assert_eq!(n.package.version.as_deref(), Some("4.4.5"));
        assert_eq!(n.major, Some(4));
        assert_eq!(d.src_dir, "app");
        assert_eq!(d.vue_major, Some(3));
        assert_eq!(
            d.component_dirs,
            [
                ComponentDir {
                    path: "app/components".into(),
                    path_prefix: false,
                    prefix: None
                },
                ComponentDir {
                    path: "app/widgets".into(),
                    path_prefix: true,
                    prefix: None
                },
                ComponentDir {
                    path: "core/components".into(),
                    path_prefix: true,
                    prefix: None
                },
            ]
        );
        assert_eq!(n.layers, ["core"]);
        assert!(d.warnings.iter().any(|w| w.contains("../shared")));
        assert!(d.aliases.contains(&("~".to_owned(), "app".to_owned())));
        assert!(d.aliases.contains(&("~~".to_owned(), String::new())));
    }

    #[test]
    fn nuxt_root_src_dir_and_nuxt2_without_components() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "package.json",
            r#"{"dependencies":{"nuxt":"^4.3.1"}}"#,
        );
        write(
            root,
            "nuxt.config.ts",
            "export default defineNuxtConfig({ srcDir: '.' })",
        );
        write(root, "app/x.ts", "");
        let d = detect(root);
        assert_eq!(d.src_dir, "");
        assert_eq!(d.component_dirs[0].path, "components");

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "package.json",
            r#"{"dependencies":{"nuxt":"^2.15.8"}}"#,
        );
        write(root, "nuxt.config.js", "export default { ssr: false }");
        let d = detect(root);
        assert_eq!(d.vue_major, Some(2));
        assert!(!d.nuxt.as_ref().unwrap().auto_import_components);
        assert!(d.component_dirs.is_empty());

        write(
            root,
            "nuxt.config.js",
            "export default { components: true }",
        );
        let d = detect(root);
        assert_eq!(d.component_dirs[0].path, "components");
    }

    #[test]
    fn vite_and_tsconfig_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "package.json",
            r#"{"dependencies":{"vue":"3.5.39"},"devDependencies":{"vite":"^7"}}"#,
        );
        write(
            root,
            "vite.config.ts",
            "export default defineConfig({ resolve: { alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) } } })",
        );
        write(
            root,
            "tsconfig.json",
            "{\n  // comment\n  \"compilerOptions\": { \"baseUrl\": \".\", \"paths\": { \"@/*\": [\"src/*\"], \"#types/*\": [\"types/*\"], }, }\n}",
        );
        write(root, "src/main.ts", "");
        let d = detect(root);
        assert_eq!(d.vue_major, Some(3));
        assert!(d.aliases.contains(&("@".to_owned(), "src".to_owned())));
        assert!(
            d.aliases
                .contains(&("#types".to_owned(), "types".to_owned()))
        );
    }
}
