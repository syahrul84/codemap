//! Routes that packages register themselves, which no route file of the
//! project shows: Passport, Sanctum, Broadcasting, Fortify, Horizon,
//! Telescope, Pulse, Cashier and Livewire. Each set is turned on by the same
//! signals Laravel uses (the package being installed, a `::routes()` call, a
//! config file, an `ignoreRoutes()` opt-out) and takes its path prefix from
//! the package's config. Added to the static route list only; with artisan
//! mode, `route:list` already reports them.

use std::path::Path;

use super::{ActionKind, RouteDef, RouteSource};

/// The installed major version of a package (`composer.lock`), else from the
/// constraint in `composer.json`; `None` when the project does not require it.
fn major(root: &Path, package: &str) -> Option<u64> {
    let json: serde_json::Value = std::fs::read_to_string(root.join("composer.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())?;
    let constraint = ["require", "require-dev"]
        .iter()
        .find_map(|k| json[*k][package].as_str())?
        .to_owned();
    let locked = std::fs::read_to_string(root.join("composer.lock"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|lock| {
            ["packages", "packages-dev"].iter().find_map(|k| {
                lock[*k]
                    .as_array()?
                    .iter()
                    .find(|p| p["name"] == package)
                    .and_then(|p| p["version"].as_str().map(str::to_owned))
            })
        });
    let version = locked.unwrap_or(constraint);
    let digits: String = version
        .trim_start_matches(|c: char| !c.is_ascii_digit())
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok().or(Some(0))
}

/// Text of the project's service providers and bootstrap, where packages are
/// told to register (or not register) their routes.
fn provider_text(root: &Path) -> String {
    let mut text = std::fs::read_to_string(root.join("bootstrap/app.php")).unwrap_or_default();
    let mut dirs = vec![root.join("app/Providers")];
    while let Some(dir) = dirs.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.extension().is_some_and(|x| x == "php") {
                text.push_str(&std::fs::read_to_string(&p).unwrap_or_default());
            }
        }
    }
    text
}

fn read(root: &Path, file: &str) -> String {
    std::fs::read_to_string(root.join(file)).unwrap_or_default()
}

/// A string setting in a config file: `'path' => env('HORIZON_PATH', 'horizon')`
/// or `'path' => 'horizon'` → `horizon`.
fn config_string(config: &str, key: &str) -> Option<String> {
    let at = config.find(&format!("'{key}' =>"))?;
    let rest = &config[at + key.len() + 5..];
    let line = rest.lines().next().unwrap_or("");
    // The last quoted string on the line: the env default, or the value.
    let quoted: Vec<&str> = line.split('\'').collect();
    (quoted.len() >= 3).then(|| quoted[quoted.len() - 2].trim_matches('/').to_owned())
}

/// `prefix/uri`, with an empty prefix allowed.
fn under(prefix: &str, uri: &str) -> String {
    let p = prefix.trim_matches('/');
    let u = uri.trim_matches('/');
    match (p.is_empty(), u.is_empty()) {
        (true, true) => "/".into(),
        (true, false) => u.to_owned(),
        (false, true) => p.to_owned(),
        (false, false) => format!("{p}/{u}"),
    }
}

struct Set<'a> {
    package: &'a str,
    prefix: String,
    out: &'a mut Vec<RouteDef>,
}

impl Set<'_> {
    /// `"GET|POST"`, `"user/profile"`, a route name.
    fn add(&mut self, methods: &str, uri: &str, name: &str) {
        let mut m: Vec<String> = methods.split('|').map(str::to_owned).collect();
        if m.iter().any(|x| x == "GET") && !m.iter().any(|x| x == "HEAD") {
            m.insert(1, "HEAD".into());
        }
        self.out.push(RouteDef {
            methods: m,
            uri: under(&self.prefix, uri),
            name: (!name.is_empty()).then(|| name.to_owned()),
            action_kind: ActionKind::Controller,
            action_raw: Some(format!("registered by {}", self.package)),
            source: RouteSource::Package,
            ..RouteDef::default()
        });
    }
}

/// Package routes the project gets without declaring them.
pub fn routes(root: &Path) -> Vec<RouteDef> {
    let mut out = Vec::new();
    let text = provider_text(root);
    let compact: String = text.split_whitespace().collect();
    let has = |needle: &str| compact.contains(needle);

    // Passport: automatic from 11 (unless ignoreRoutes), `Passport::routes()`
    // before that. JSON API routes until 12, or when asked for.
    if let Some(v) = major(root, "laravel/passport") {
        let registered = if v >= 11 {
            !has("Passport::ignoreRoutes()")
        } else {
            has("Passport::routes(")
        };
        if registered {
            let config = read(root, "config/passport.php");
            let prefix = config_string(&config, "path").unwrap_or_else(|| "oauth".into());
            let mut s = Set {
                package: "laravel/passport",
                prefix,
                out: &mut out,
            };
            s.add("GET", "authorize", "passport.authorizations.authorize");
            s.add("POST", "authorize", "passport.authorizations.approve");
            s.add("DELETE", "authorize", "passport.authorizations.deny");
            s.add("POST", "token", "passport.token");
            s.add("POST", "token/refresh", "passport.token.refresh");
            if v < 12 || has("registersJsonApiRoutes") {
                s.add("GET", "tokens", "passport.tokens.index");
                s.add("DELETE", "tokens/{token_id}", "passport.tokens.destroy");
                s.add("GET", "clients", "passport.clients.index");
                s.add("POST", "clients", "passport.clients.store");
                s.add("PUT", "clients/{client_id}", "passport.clients.update");
                s.add("DELETE", "clients/{client_id}", "passport.clients.destroy");
                s.add("GET", "scopes", "passport.scopes.index");
                s.add(
                    "GET",
                    "personal-access-tokens",
                    "passport.personal.tokens.index",
                );
                s.add(
                    "POST",
                    "personal-access-tokens",
                    "passport.personal.tokens.store",
                );
                s.add(
                    "DELETE",
                    "personal-access-tokens/{token_id}",
                    "passport.personal.tokens.destroy",
                );
            }
        }
    }

    // Sanctum: the SPA cookie route, unless turned off.
    if major(root, "laravel/sanctum").is_some() && !has("Sanctum::ignoreRoutes()") {
        let config = read(root, "config/sanctum.php");
        let off: String = config.split_whitespace().collect();
        if !off.contains("'routes'=>false") {
            let prefix = config_string(&config, "prefix").unwrap_or_else(|| "sanctum".into());
            Set {
                package: "laravel/sanctum",
                prefix,
                out: &mut out,
            }
            .add("GET", "csrf-cookie", "sanctum.csrf-cookie");
        }
    }

    // Broadcasting: `Broadcast::routes()` or `->withBroadcasting(...)`.
    if has("Broadcast::routes(") || has("->withBroadcasting(") {
        Set {
            package: "laravel/framework (broadcasting)",
            prefix: String::new(),
            out: &mut out,
        }
        .add("GET|POST", "broadcasting/auth", "");
    }

    // Fortify: its routes follow the features enabled in config/fortify.php.
    if major(root, "laravel/fortify").is_some() && !has("Fortify::ignoreRoutes()") {
        let config = read(root, "config/fortify.php");
        let prefix = config_string(&config, "prefix").unwrap_or_default();
        let views = !config
            .split_whitespace()
            .collect::<String>()
            .contains("'views'=>false");
        let on = |f: &str| config.is_empty() || config.contains(&format!("Features::{f}("));
        let mut s = Set {
            package: "laravel/fortify",
            prefix,
            out: &mut out,
        };
        if views {
            s.add("GET", "login", "login");
        }
        s.add("POST", "login", "login.store");
        s.add("POST", "logout", "logout");
        if on("resetPasswords") {
            if views {
                s.add("GET", "forgot-password", "password.request");
                s.add("GET", "reset-password/{token}", "password.reset");
            }
            s.add("POST", "forgot-password", "password.email");
            s.add("POST", "reset-password", "password.update");
        }
        if on("registration") {
            if views {
                s.add("GET", "register", "register");
            }
            s.add("POST", "register", "register.store");
        }
        if on("emailVerification") {
            if views {
                s.add("GET", "email/verify", "verification.notice");
            }
            s.add("GET", "email/verify/{id}/{hash}", "verification.verify");
            s.add(
                "POST",
                "email/verification-notification",
                "verification.send",
            );
        }
        if on("updateProfileInformation") {
            s.add(
                "PUT",
                "user/profile-information",
                "user-profile-information.update",
            );
        }
        if on("updatePasswords") {
            s.add("PUT", "user/password", "user-password.update");
        }
        if views {
            s.add("GET", "user/confirm-password", "password.confirm");
        }
        s.add(
            "GET",
            "user/confirmed-password-status",
            "password.confirmation",
        );
        s.add("POST", "user/confirm-password", "password.confirm.store");
        if on("twoFactorAuthentication") {
            if views {
                s.add("GET", "two-factor-challenge", "two-factor.login");
            }
            s.add("POST", "two-factor-challenge", "two-factor.login.store");
            s.add(
                "POST",
                "user/two-factor-authentication",
                "two-factor.enable",
            );
            s.add(
                "POST",
                "user/confirmed-two-factor-authentication",
                "two-factor.confirm",
            );
            s.add(
                "DELETE",
                "user/two-factor-authentication",
                "two-factor.disable",
            );
            s.add("GET", "user/two-factor-qr-code", "two-factor.qr-code");
            s.add("GET", "user/two-factor-secret-key", "two-factor.secret-key");
            s.add(
                "GET",
                "user/two-factor-recovery-codes",
                "two-factor.recovery-codes",
            );
            s.add(
                "POST",
                "user/two-factor-recovery-codes",
                "two-factor.regenerate-recovery-codes",
            );
        }
    }

    // Horizon: the dashboard and its JSON API, under config('horizon.path').
    if major(root, "laravel/horizon").is_some() {
        let prefix = config_string(&read(root, "config/horizon.php"), "path")
            .unwrap_or_else(|| "horizon".into());
        let mut s = Set {
            package: "laravel/horizon",
            prefix,
            out: &mut out,
        };
        for (m, u) in [
            ("GET", "api/stats"),
            ("GET", "api/workload"),
            ("GET", "api/masters"),
            ("GET", "api/monitoring"),
            ("POST", "api/monitoring"),
            ("GET", "api/monitoring/{tag}"),
            ("DELETE", "api/monitoring/{tag}"),
            ("GET", "api/metrics/jobs"),
            ("GET", "api/metrics/jobs/{id}"),
            ("GET", "api/metrics/queues"),
            ("GET", "api/metrics/queues/{id}"),
            ("GET", "api/batches"),
            ("GET", "api/batches/{id}"),
            ("POST", "api/batches/retry/{id}"),
            ("GET", "api/jobs/pending"),
            ("GET", "api/jobs/completed"),
            ("GET", "api/jobs/silenced"),
            ("GET", "api/jobs/failed"),
            ("GET", "api/jobs/failed/{id}"),
            ("POST", "api/jobs/retry/{id}"),
            ("GET", "api/jobs/{id}"),
            ("GET", "{view?}"),
        ] {
            s.add(m, u, "");
        }
    }

    // Telescope: the dashboard and its API, under config('telescope.path').
    if major(root, "laravel/telescope").is_some() {
        let prefix = config_string(&read(root, "config/telescope.php"), "path")
            .unwrap_or_else(|| "telescope".into());
        let mut s = Set {
            package: "laravel/telescope",
            prefix,
            out: &mut out,
        };
        s.add("POST", "telescope-api/{entryType}", "");
        s.add("GET", "telescope-api/{entryType}/{telescopeEntryId}", "");
        s.add("POST", "telescope-api/toggle-recording", "");
        s.add("DELETE", "telescope-api/entries", "");
        s.add("GET", "{view?}", "telescope");
    }

    // Pulse: one dashboard page under config('pulse.path').
    if major(root, "laravel/pulse").is_some() {
        let prefix = config_string(&read(root, "config/pulse.php"), "path")
            .unwrap_or_else(|| "pulse".into());
        Set {
            package: "laravel/pulse",
            prefix,
            out: &mut out,
        }
        .add("GET", "", "pulse");
    }

    // Cashier: the Stripe webhook, under config('cashier.path').
    if major(root, "laravel/cashier").is_some() && !has("Cashier::ignoreRoutes()") {
        let prefix = config_string(&read(root, "config/cashier.php"), "path")
            .unwrap_or_else(|| "stripe".into());
        Set {
            package: "laravel/cashier",
            prefix,
            out: &mut out,
        }
        .add("POST", "webhook", "cashier.webhook");
    }

    // Livewire: its update and upload endpoints (v3; v2 used `message/{name}`).
    if let Some(v) = major(root, "livewire/livewire") {
        let mut s = Set {
            package: "livewire/livewire",
            prefix: "livewire".into(),
            out: &mut out,
        };
        if v >= 3 {
            s.add("POST", "update", "livewire.update");
        } else {
            s.add("POST", "message/{name}", "livewire.message");
        }
        s.add("POST", "upload-file", "livewire.upload-file");
        s.add("GET", "preview-file/{filename}", "livewire.preview-file");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(composer: &str, provider: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("composer.json"), composer).unwrap();
        std::fs::create_dir_all(dir.path().join("app/Providers")).unwrap();
        std::fs::write(
            dir.path().join("app/Providers/AuthServiceProvider.php"),
            provider,
        )
        .unwrap();
        dir
    }

    fn uris(dir: &tempfile::TempDir) -> Vec<String> {
        routes(dir.path())
            .iter()
            .map(|r| format!("{} {}", r.methods_str(), r.uri))
            .collect()
    }

    #[test]
    fn passport_and_sanctum_routes() {
        let p = project(
            r#"{"require":{"laravel/framework":"^11.0","laravel/passport":"^12.0","laravel/sanctum":"^4.0"}}"#,
            "<?php class AuthServiceProvider {}",
        );
        let u = uris(&p);
        assert!(u.contains(&"POST oauth/token".to_owned()), "{u:?}");
        assert!(u.contains(&"GET|HEAD sanctum/csrf-cookie".to_owned()));
        assert!(
            !u.contains(&"GET|HEAD oauth/clients".to_owned()),
            "JSON API routes are off in 12"
        );

        // Older Passport only with Passport::routes(); newer can opt out.
        let old = project(
            r#"{"require":{"laravel/passport":"^10.0"}}"#,
            "<?php Passport::routes();",
        );
        assert!(routes(old.path()).iter().any(|r| r.uri == "oauth/clients"));
        let off = project(
            r#"{"require":{"laravel/passport":"^12.0"}}"#,
            "<?php Passport::ignoreRoutes();",
        );
        assert!(routes(off.path()).is_empty());
        let none = project(r#"{"require":{"laravel/framework":"^11.0"}}"#, "<?php");
        assert!(routes(none.path()).is_empty());
    }

    #[test]
    fn other_package_routes_with_their_config() {
        let p = project(
            r#"{"require":{"laravel/fortify":"^1.0","laravel/horizon":"^5.0","laravel/cashier":"^15.0","livewire/livewire":"^3.0"}}"#,
            "<?php Broadcast::routes();",
        );
        std::fs::create_dir_all(p.path().join("config")).unwrap();
        std::fs::write(
            p.path().join("config/horizon.php"),
            "<?php return [\n    'path' => env('HORIZON_PATH', 'admin/queues'),\n];",
        )
        .unwrap();
        std::fs::write(
            p.path().join("config/fortify.php"),
            "<?php return [\n    'prefix' => 'auth',\n    'features' => [Features::registration()],\n];",
        )
        .unwrap();
        let u = uris(&p);
        for want in [
            "GET|HEAD|POST broadcasting/auth",
            "POST auth/login",
            "POST auth/register",
            "GET|HEAD admin/queues/api/stats",
            "POST stripe/webhook",
            "POST livewire/update",
        ] {
            assert!(u.contains(&want.to_owned()), "{want} not in {u:?}");
        }
        // Fortify features that are off add nothing.
        assert!(!u.iter().any(|x| x.contains("two-factor")), "{u:?}");
    }
}
