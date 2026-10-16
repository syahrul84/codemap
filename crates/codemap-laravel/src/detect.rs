//! Laravel detection: version (composer.lock, else composer.json constraint),
//! layout (legacy Kernel/RouteServiceProvider vs 11+ `withRouting`), PSR-4 roots.
//! Every conclusion carries the reason it was reached.

use std::path::Path;

use codemap_core::symbols::{self, ClassRef};
use codemap_core::{Result, Store};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const HTTP_KERNEL: &str = "Illuminate\\Foundation\\Http\\Kernel";
pub const ROUTE_SERVICE_PROVIDER: &str =
    "Illuminate\\Foundation\\Support\\Providers\\RouteServiceProvider";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    /// `app/Http/Kernel.php` + `RouteServiceProvider` (Laravel ≤ 10, or upgraded apps).
    Legacy,
    /// `bootstrap/app.php` with `Application::configure()->withRouting()` (Laravel 11+).
    Slim,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Psr4Root {
    pub namespace: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dev: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Detection {
    pub laravel: bool,
    pub version: Option<String>,
    pub major: Option<u32>,
    /// `composer.lock` (exact) or `composer.json` (constraint, inferred).
    pub version_source: Option<String>,
    pub version_inferred: bool,
    pub layout: Layout,
    pub reasons: Vec<String>,
    pub warnings: Vec<String>,
    pub composer_name: Option<String>,
    pub php_constraint: Option<String>,
    pub psr4: Vec<Psr4Root>,
    /// `bootstrap/app.php` calls `->withRouting(`.
    pub with_routing: bool,
    pub kernels: Vec<ClassRef>,
    pub route_providers: Vec<ClassRef>,
}

impl Detection {
    /// What route extraction depends on, to decide whether to re-run it.
    pub fn routing_fingerprint(&self) -> String {
        let providers: Vec<&str> = self
            .route_providers
            .iter()
            .map(|p| p.file.as_str())
            .collect();
        format!(
            "{:?}|{}|{}",
            self.layout,
            self.with_routing,
            providers.join(",")
        )
    }
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

/// `v11.31.0` → `11.31.0`; `^10.10|^11.0` → `10.10` (first version-looking part).
fn version_from(s: &str) -> Option<String> {
    let start = s.find(|c: char| c.is_ascii_digit())?;
    let v: String = s[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let v = v.trim_end_matches('.').to_owned();
    (!v.is_empty()).then_some(v)
}

fn major(version: &str) -> Option<u32> {
    version.split('.').next()?.parse().ok()
}

pub fn detect(root: &Path, store: &Store) -> Result<Detection> {
    let mut d = Detection::default();
    let mut warnings = Vec::new();

    let lock = read_json(&root.join("composer.lock"), &mut warnings);
    let json = read_json(&root.join("composer.json"), &mut warnings);

    let locked = lock.as_ref().and_then(|lock| {
        ["packages", "packages-dev"].iter().find_map(|key| {
            lock.get(key)?.as_array()?.iter().find_map(|p| {
                (p.get("name")?.as_str()? == "laravel/framework")
                    .then(|| p.get("version")?.as_str().map(str::to_owned))
                    .flatten()
            })
        })
    });
    let constraint = json.as_ref().and_then(|j| {
        j.pointer("/require/laravel~1framework")?
            .as_str()
            .map(str::to_owned)
    });

    if let Some(v) = locked.as_deref().and_then(version_from) {
        d.reasons
            .push(format!("composer.lock pins laravel/framework {v}"));
        d.version_source = Some("composer.lock".into());
        d.major = major(&v);
        d.version = Some(v);
        d.laravel = true;
    } else if let Some(c) = &constraint {
        d.laravel = true;
        d.version_inferred = true;
        d.version_source = Some("composer.json".into());
        d.version = version_from(c);
        d.major = d.version.as_deref().and_then(major);
        d.reasons.push(format!(
            "no laravel/framework in composer.lock; composer.json requires {c} (version inferred)"
        ));
    }

    if let Some(j) = &json {
        d.composer_name = j.get("name").and_then(Value::as_str).map(str::to_owned);
        d.php_constraint = j
            .pointer("/require/php")
            .and_then(Value::as_str)
            .map(str::to_owned);
        for (key, dev) in [("autoload", false), ("autoload-dev", true)] {
            let Some(map) = j
                .pointer(&format!("/{key}/psr-4"))
                .and_then(Value::as_object)
            else {
                continue;
            };
            for (ns, paths) in map {
                let paths: Vec<&str> = match paths {
                    Value::String(p) => vec![p.as_str()],
                    Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
                    _ => Vec::new(),
                };
                for p in paths {
                    d.psr4.push(Psr4Root {
                        namespace: ns.trim_end_matches('\\').to_owned(),
                        path: p.trim_end_matches('/').to_owned(),
                        dev,
                    });
                }
            }
        }
    }

    if !d.laravel {
        d.reasons
            .push("laravel/framework is not in composer.lock or composer.json".into());
        d.warnings = warnings;
        return Ok(d);
    }

    let bootstrap = std::fs::read_to_string(root.join("bootstrap/app.php")).unwrap_or_default();
    d.with_routing = bootstrap.contains("->withRouting(");
    if d.with_routing {
        d.reasons
            .push("bootstrap/app.php calls ->withRouting() (Laravel 11+ structure)".into());
    } else if bootstrap.contains("Application::configure(") {
        d.reasons.push(
            "bootstrap/app.php calls Application::configure() but not ->withRouting()".into(),
        );
    }

    d.kernels = symbols::descendants(store.conn(), &[HTTP_KERNEL])?;
    for k in &d.kernels {
        d.reasons.push(format!(
            "{} extends the HTTP Kernel ({}:{})",
            k.fqn, k.file, k.line
        ));
    }
    d.route_providers = symbols::descendants(store.conn(), &[ROUTE_SERVICE_PROVIDER])?;
    for p in &d.route_providers {
        d.reasons.push(format!(
            "{} extends RouteServiceProvider ({}:{})",
            p.fqn, p.file, p.line
        ));
    }
    let legacy = !d.kernels.is_empty() || !d.route_providers.is_empty();

    d.layout = match (d.with_routing, legacy) {
        (true, false) => Layout::Slim,
        (true, true) => {
            warnings.push(
                "mixed layout: bootstrap/app.php uses withRouting() and a legacy Kernel or \
                 RouteServiceProvider exists; routes are read from both"
                    .into(),
            );
            Layout::Slim
        }
        (false, true) => Layout::Legacy,
        (false, false) => Layout::Unknown,
    };
    if d.layout == Layout::Legacy && d.major.is_some_and(|m| m >= 11) {
        d.reasons.push(format!(
            "Laravel {} with the legacy structure (upgraded app)",
            d.major.unwrap_or_default()
        ));
    }
    d.warnings = warnings;
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(version_from("v11.31.0").as_deref(), Some("11.31.0"));
        assert_eq!(version_from("^10.10|^11.0").as_deref(), Some("10.10"));
        assert_eq!(version_from("11.x-dev").as_deref(), Some("11"));
        assert_eq!(version_from("dev-master"), None);
        assert_eq!(major("10.48.4"), Some(10));
    }
}
