//! Artisan mode: `php artisan route:list --json` as the source of truth, with
//! file:line taken from the static extraction. This boots the project, i.e.
//! runs its PHP code, so it only runs when enabled in `.codemap.toml`.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use codemap_core::config::{ArtisanMode, LaravelConfig};
use serde::Deserialize;

use super::{ActionKind, RouteDef, RouteSource};

#[derive(Debug, Clone, Deserialize)]
pub struct ArtisanRoute {
    #[serde(default)]
    pub domain: Option<String>,
    pub method: String,
    pub uri: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default, deserialize_with = "middleware_list")]
    pub middleware: Vec<String>,
}

fn middleware_list<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum M {
        List(Vec<String>),
        One(String),
        Null,
    }
    Ok(match M::deserialize(d)? {
        M::List(l) => l,
        M::One(s) => s
            .lines()
            .map(str::to_owned)
            .filter(|s| !s.is_empty())
            .collect(),
        M::Null => Vec::new(),
    })
}

/// Why artisan mode does (not) run for this project.
pub enum Decision {
    Skip(&'static str),
    Run,
}

pub fn decide(cfg: &LaravelConfig, root: &Path) -> Decision {
    match cfg.artisan {
        ArtisanMode::Off => Decision::Skip("artisan mode is off"),
        ArtisanMode::On => Decision::Run,
        ArtisanMode::Auto if php_binary().is_none() => {
            Decision::Skip("auto: no php binary on PATH")
        }
        ArtisanMode::Auto if !root.join("vendor/autoload.php").is_file() => {
            Decision::Skip("auto: vendor/autoload.php not found")
        }
        ArtisanMode::Auto => Decision::Run,
    }
}

/// Path of a `php` executable on PATH.
pub fn php_binary() -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|p| p.join("php"))
            .find(|p| p.is_file())
    })
}

/// Runs `route:list --json`. Errors describe what failed (for a warning).
pub fn run(root: &Path, cfg: &LaravelConfig) -> Result<Vec<ArtisanRoute>, String> {
    let mut parts = cfg.artisan_command.split_whitespace();
    let program = parts.next().ok_or("artisan_command is empty")?;
    let cache = std::env::temp_dir().join(format!("codemap-artisan-{}", std::process::id()));
    std::fs::create_dir_all(&cache)
        .map_err(|e| format!("cannot create {}: {e}", cache.display()))?;
    let cache_file = |name: &str| cache.join(name).to_string_lossy().into_owned();

    let mut child = Command::new(program)
        .args(parts)
        .args(["route:list", "--json"])
        .current_dir(root)
        // Keep Laravel's cache writes out of a (possibly read-only) project.
        .env("APP_ROUTES_CACHE", cache_file("routes.php"))
        .env("APP_CONFIG_CACHE", cache_file("config.php"))
        .env("APP_SERVICES_CACHE", cache_file("services.php"))
        .env("APP_PACKAGES_CACHE", cache_file("packages.php"))
        .env("APP_EVENTS_CACHE", cache_file("events.php"))
        .env("VIEW_COMPILED_PATH", cache.to_string_lossy().into_owned())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run `{}`: {e}", cfg.artisan_command))?;

    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    let out_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });

    let timeout = Duration::from_millis(cfg.artisan_timeout_ms);
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_dir_all(&cache);
                return Err(format!("timed out after {} ms", cfg.artisan_timeout_ms));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(format!("waiting for artisan failed: {e}")),
        }
    };
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    let _ = std::fs::remove_dir_all(&cache);

    if !status.success() {
        return Err(format!("exited with {status}: {}", tail(&stderr, &stdout)));
    }
    // Deprecation notices may precede the JSON.
    let json = stdout.find('[').map_or("", |i| &stdout[i..]);
    serde_json::from_str(json)
        .map_err(|e| format!("output is not route JSON ({e}): {}", tail(&stdout, "")))
}

fn tail(primary: &str, fallback: &str) -> String {
    let s = if primary.trim().is_empty() {
        fallback
    } else {
        primary
    };
    let lines: Vec<&str> = s.trim().lines().collect();
    lines[lines.len().saturating_sub(3)..].join(" | ")
}

/// Artisan routes are authoritative; file:line (and view/redirect details)
/// come from the static route with the same methods + URI + domain.
/// Returns the merged routes and how many static routes artisan did not report.
pub fn merge(static_routes: &[RouteDef], artisan: Vec<ArtisanRoute>) -> (Vec<RouteDef>, usize) {
    let mut used = vec![false; static_routes.len()];
    let mut out = Vec::with_capacity(artisan.len());
    for a in artisan {
        let methods: Vec<String> = a.method.split('|').map(str::to_owned).collect();
        let uri = if a.uri == "/" {
            a.uri.clone()
        } else {
            a.uri.trim_matches('/').to_owned()
        };
        let matched = static_routes.iter().enumerate().position(|(i, s)| {
            !used[i] && s.methods == methods && s.uri == uri && s.domain == a.domain
        });
        let base = matched.map(|i| {
            used[i] = true;
            &static_routes[i]
        });
        let action = a.action.unwrap_or_default();
        let (kind, controller, method) = match action.as_str() {
            "Closure" => (ActionKind::Closure, None, None),
            a if a.ends_with("ViewController") => (ActionKind::View, None, None),
            a if a.ends_with("RedirectController") => (ActionKind::Redirect, None, None),
            a => match a.split_once('@') {
                Some((c, m)) => (
                    ActionKind::Controller,
                    Some(c.to_owned()),
                    Some(m.to_owned()),
                ),
                None if !a.is_empty() => (
                    ActionKind::Controller,
                    Some(a.to_owned()),
                    Some("__invoke".to_owned()),
                ),
                None => (ActionKind::Unresolved, None, None),
            },
        };
        out.push(RouteDef {
            methods,
            uri,
            name: a.name,
            action_kind: base
                .filter(|b| matches!(b.action_kind, ActionKind::Health | ActionKind::Fallback))
                .map_or(kind, |b| b.action_kind),
            action_raw: Some(action),
            controller_fqn: controller,
            controller_method: method,
            middleware: a.middleware,
            prefix: base.and_then(|b| b.prefix.clone()),
            domain: a.domain,
            file: base.and_then(|b| b.file.clone()),
            line: base.and_then(|b| b.line),
            source: RouteSource::Artisan,
            unresolved_reason: None,
            extra: base.map(|b| b.extra.clone()).unwrap_or_default(),
        });
    }
    let unmatched = used.iter().filter(|u| !**u).count();
    (out, unmatched)
}
