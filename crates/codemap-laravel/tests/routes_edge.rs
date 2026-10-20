//! Every static routing feature, on fixtures/routes-edge.

mod common;

use codemap_laravel::routes::{ActionKind, RouteDef};
use common::{fixture, indexed, routes};

fn routes_edge() -> (Vec<RouteDef>, codemap_core::Store) {
    let (store, _) = indexed(&fixture("routes-edge"));
    (routes(&store), store)
}

fn by_uri<'a>(all: &'a [RouteDef], uri: &str) -> Vec<&'a RouteDef> {
    all.iter().filter(|r| r.uri == uri).collect()
}

fn one<'a>(all: &'a [RouteDef], uri: &str) -> &'a RouteDef {
    let hits = by_uri(all, uri);
    assert_eq!(hits.len(), 1, "{uri}: {hits:#?}");
    hits[0]
}

#[test]
fn match_any_redirect_and_fallback() {
    let (all, _) = routes_edge();
    let search = one(&all, "search");
    assert_eq!(search.methods, ["GET", "POST", "HEAD"]);
    assert_eq!(search.name.as_deref(), Some("search"));
    assert_eq!(search.middleware, ["web"]);
    assert_eq!(one(&all, "ping").methods.len(), 7);
    let old = one(&all, "old");
    assert_eq!(old.action_kind, ActionKind::Redirect);
    assert_eq!(old.extra.status, Some(301));
    let fallback = one(&all, "{fallbackPlaceholder}");
    assert_eq!(fallback.action_kind, ActionKind::Fallback);
    assert_eq!(fallback.name, None);
    assert_eq!(fallback.extra.wheres["fallbackPlaceholder"], ".*");
}

#[test]
fn array_groups_namespaced_classes_and_requires() {
    let (all, _) = routes_edge();
    let dashboard = one(&all, "admin/dashboard");
    assert_eq!(dashboard.name.as_deref(), Some("admin.dashboard"));
    assert_eq!(dashboard.middleware, ["web", "auth", "can:admin"]);
    assert_eq!(
        dashboard.controller_fqn.as_deref(),
        Some("App\\Http\\Controllers\\Admin\\DashboardController")
    );
    assert_eq!(dashboard.controller_method.as_deref(), Some("__invoke"));

    // A required file inherits the group and keeps its own file:line.
    let users = one(&all, "admin/users");
    assert_eq!(users.name.as_deref(), Some("admin.users"));
    assert_eq!(users.middleware, ["web", "auth", "can:admin"]);
    assert_eq!(users.location().as_deref(), Some("routes/admin.php:5"));
}

#[test]
fn domains_and_then_callback() {
    let (all, _) = routes_edge();
    let home = all
        .iter()
        .find(|r| r.name.as_deref() == Some("account.home"))
        .unwrap();
    assert_eq!(home.domain.as_deref(), Some("{account}.example.com"));
    assert_eq!(home.uri, "/");

    let stripe = one(&all, "webhooks/stripe");
    assert_eq!(stripe.name.as_deref(), Some("webhooks.stripe"));
    assert_eq!(stripe.middleware, ["api"]);
    assert_eq!(stripe.location().as_deref(), Some("routes/webhooks.php:6"));
}

#[test]
fn resources_nested_shallow_names_parameters_singleton() {
    let (all, _) = routes_edge();
    assert_eq!(
        all.iter()
            .filter(|r| r.uri.starts_with("photos") && !r.uri.contains("comments"))
            .count(),
        7
    );
    assert_eq!(
        all.iter().filter(|r| r.uri.starts_with("videos")).count(),
        7
    );
    assert_eq!(
        one(&all, "videos/{video}/edit").controller_fqn.as_deref(),
        Some("App\\Http\\Controllers\\VideoController")
    );

    let comments: Vec<String> = all
        .iter()
        .filter(|r| r.name.as_deref().is_some_and(|n| n.contains("comments")))
        .map(|r| format!("{} {}", r.uri, r.name.as_deref().unwrap()))
        .collect();
    assert_eq!(
        comments,
        [
            "photos/{photo}/comments photos.comments.index",
            "comments/{comment} comments.show"
        ]
    );

    let profiles: Vec<String> = all
        .iter()
        .filter(|r| r.uri.starts_with("user-profiles"))
        .map(|r| format!("{} {}", r.uri, r.name.as_deref().unwrap()))
        .collect();
    assert_eq!(
        profiles,
        [
            "user-profiles profiles.list",
            "user-profiles/{profile} user-profiles.show"
        ]
    );

    let settings: Vec<String> = all
        .iter()
        .filter(|r| r.uri.starts_with("settings"))
        .map(|r| format!("{} {}", r.methods_str(), r.uri))
        .collect();
    assert_eq!(
        settings,
        [
            "GET|HEAD settings",
            "GET|HEAD settings/edit",
            "PUT|PATCH settings"
        ]
    );
}

#[test]
fn unresolved_cases_are_recorded_not_guessed() {
    let (all, store) = routes_edge();
    let legacy = one(&all, "legacy");
    assert_eq!(legacy.controller_fqn, None);
    assert!(
        legacy
            .unresolved_reason
            .as_deref()
            .unwrap()
            .contains("needs a controller namespace")
    );

    let in_loop: Vec<&RouteDef> = all
        .iter()
        .filter(|r| r.unresolved_reason.as_deref() == Some("defined inside a loop"))
        .collect();
    assert_eq!(in_loop.len(), 1);
    assert!(in_loop[0].uri.contains("letters"));

    // `$uri = '/dynamic'; Route::get($uri, …)`: the variable is followed.
    let dynamic = one(&all, "dynamic");
    assert_eq!(dynamic.unresolved_reason, None);

    let macro_route = one(&all, "macro");
    assert!(
        macro_route
            .unresolved_reason
            .as_deref()
            .unwrap()
            .contains("->customMacro()")
    );

    let debug = one(&all, "debug");
    assert!(debug.extra.conditional);
    assert!(debug.unresolved_reason.is_none());
    let warnings = store.warnings().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.message.contains("inside an `if`"))
    );

    assert_eq!(one(&all, "no-session").extra.without_middleware, ["web"]);
}

#[test]
fn edge_snapshot() {
    let (store, _) = indexed(&fixture("routes-edge"));
    insta::assert_snapshot!("routes-edge", common::table(&store));
}

#[test]
fn resource_where_constraints_apply_to_every_route() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    common::write(
        root,
        "composer.lock",
        r#"{"packages":[{"name":"laravel/framework","version":"v9.52.0"}]}"#,
    );
    common::write(
        root,
        "routes/api.php",
        "<?php\nuse App\\Http\\Controllers\\GroupController;\nRoute::apiResource('groups', GroupController::class)->where(['group' => '[0-9]+'])->whereNumber('x');\n",
    );
    let (store, report) = common::indexed(root);
    assert_eq!(report.laravel.routes.unresolved, 0);
    let all = routes(&store);
    assert_eq!(all.len(), 5);
    assert!(
        all.iter()
            .all(|r| r.extra.wheres.get("group").map(String::as_str) == Some("[0-9]+"))
    );
}
