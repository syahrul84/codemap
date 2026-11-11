//! Which files register routes: `bootstrap/app.php` `withRouting()` (11+),
//! RouteServiceProvider subclasses (legacy), or the conventional files.

use std::collections::BTreeSet;
use std::path::Path;

use super::RouteDef;
use super::eval::{Evaluator, Group, Warning};
use crate::detect::Detection;

#[derive(Debug, Clone, Default)]
pub struct StaticRoutes {
    pub routes: Vec<RouteDef>,
    pub warnings: Vec<Warning>,
    /// Files read while evaluating; a change to any of them re-runs extraction.
    pub files: BTreeSet<String>,
    /// Human-readable list of where routes came from, in order.
    pub sources: Vec<String>,
}

pub fn extract_static(root: &Path, detection: &Detection) -> StaticRoutes {
    let mut ev = Evaluator::new(root);
    let mut sources = Vec::new();

    if detection.with_routing && ev.bootstrap("bootstrap/app.php") {
        sources.push("bootstrap/app.php ->withRouting()".to_owned());
    }
    for provider in &detection.route_providers {
        ev.provider(&provider.file, &provider.fqn);
        sources.push(format!("{} ({})", provider.fqn, provider.file));
    }
    // Any other service provider may register routes too: a group of its
    // own, a package's route file it requires, a route file it maps.
    let before = ev.routes.len();
    for provider in &detection.service_providers {
        let had = ev.routes.len();
        ev.provider(&provider.file, &provider.fqn);
        if ev.routes.len() > had {
            sources.push(format!("{} ({})", provider.fqn, provider.file));
        }
    }
    // A provider that maps the app's route files stands in for the
    // conventional ones; one that only adds a few routes does not.
    let maps_route_files = ev.routes[before..]
        .iter()
        .any(|r| r.file.as_deref().is_some_and(|f| f.starts_with("routes/")));
    let declared = detection.with_routing && sources.iter().any(|s| s.contains("withRouting"))
        || !detection.route_providers.is_empty();
    if !declared && !maps_route_files {
        let mut found = false;
        if root.join("routes/api.php").is_file() {
            let api = Group {
                prefix: "api".into(),
                middleware: vec!["api".into()],
                ..Group::default()
            };
            ev.route_file("routes/api.php", &api);
            sources.push("routes/api.php (convention: prefix api, middleware api)".into());
            found = true;
        }
        if root.join("routes/web.php").is_file() {
            ev.route_file("routes/web.php", &Group::with_middleware("web"));
            sources.push("routes/web.php (convention: middleware web)".into());
            found = true;
        }
        if found {
            ev.warnings.push(Warning {
                file: None,
                line: None,
                message: "no withRouting() or RouteServiceProvider found; route files were \
                          found by convention"
                    .into(),
            });
        }
    }

    // Laravel MCP loads `routes/ai.php` itself, where `Mcp::web('/mcp/x', …)`
    // registers the servers a client reaches over HTTP.
    if root.join("routes/ai.php").is_file()
        && super::packages::major(root, "laravel/mcp").is_some()
        && !ev.files.contains("routes/ai.php")
    {
        ev.route_file("routes/ai.php", &Group::default());
        sources.push("routes/ai.php (loaded by laravel/mcp)".to_owned());
    }

    // A file reached from two sources (mixed layout) registers its routes once.
    let mut seen = BTreeSet::new();
    let mut routes = Vec::new();
    for r in ev.routes {
        let key = (
            r.methods_str(),
            r.uri.clone(),
            r.domain.clone(),
            r.file.clone(),
            r.line,
            r.name.clone(),
        );
        if seen.insert(key) {
            routes.push(r);
        }
    }
    StaticRoutes {
        routes,
        warnings: ev.warnings,
        files: ev.files,
        sources,
    }
}
