//! Route tables for each fixture. Key routes are asserted by hand first, so a
//! wrong snapshot cannot simply be accepted.

mod common;

use codemap_laravel::routes::{ActionKind, RouteDef};
use common::{fixture, indexed, routes, table};

fn find<'a>(routes: &'a [RouteDef], methods: &str, uri: &str) -> &'a RouteDef {
    routes
        .iter()
        .find(|r| r.methods_str() == methods && r.uri == uri)
        .unwrap_or_else(|| panic!("no {methods} {uri}"))
}

#[test]
fn laravel10_legacy_routes() {
    let (store, report) = indexed(&fixture("laravel10-legacy"));
    eprintln!("{}", table(&store));
    let all = routes(&store);
    assert_eq!(report.laravel.routes.unresolved, 0);

    // String actions resolve through RouteServiceProvider::$namespace.
    let index = find(&all, "GET|HEAD", "orders");
    assert_eq!(index.name.as_deref(), Some("orders.index"));
    assert_eq!(
        index.controller_fqn.as_deref(),
        Some("App\\Http\\Controllers\\OrderController")
    );
    assert_eq!(index.controller_method.as_deref(), Some("index"));
    assert_eq!(index.middleware, ["web"]);
    assert_eq!(index.location().as_deref(), Some("routes/web.php:9"));

    // Invokable string action inside an auth group.
    let reports = find(&all, "GET|HEAD", "reports");
    assert_eq!(
        reports.controller_fqn.as_deref(),
        Some("App\\Http\\Controllers\\Admin\\ReportController")
    );
    assert_eq!(reports.controller_method.as_deref(), Some("__invoke"));
    assert_eq!(reports.middleware, ["web", "auth"]);

    // Closure.
    assert_eq!(find(&all, "GET|HEAD", "/").action_kind, ActionKind::Closure);

    // apiResource under the api prefix, without the web namespace.
    let update = find(&all, "PUT|PATCH", "api/orders/{order}");
    assert_eq!(update.name.as_deref(), Some("orders.update"));
    assert_eq!(update.middleware, ["api"]);
    assert_eq!(
        update.controller_fqn.as_deref(),
        Some("App\\Http\\Controllers\\OrderController")
    );
    assert!(all.iter().all(|r| r.uri != "api/orders/create"));
    assert_eq!(
        find(&all, "POST", "api/orders/{order}/pay").name.as_deref(),
        Some("orders.pay")
    );
    assert_eq!(all.len(), 11);

    insta::assert_snapshot!("laravel10-legacy", table(&store));
}

#[test]
fn laravel11_slim_routes() {
    let (store, report) = indexed(&fixture("laravel11-slim"));
    eprintln!("{}", table(&store));
    let all = routes(&store);
    assert_eq!(report.laravel.routes.unresolved, 0);

    // api routes come first (Laravel registers api, health, web).
    assert_eq!(all[0].uri, "api/orders");
    let only: Vec<String> = all
        .iter()
        .filter(|r| r.uri.starts_with("api/orders"))
        .map(|r| format!("{} {}", r.methods_str(), r.uri))
        .collect();
    assert_eq!(
        only,
        [
            "GET|HEAD api/orders",
            "POST api/orders",
            "GET|HEAD api/orders/{order}"
        ]
    );

    // resource()->except(create, edit, destroy) keeps index, store, show, update.
    let customers: Vec<&str> = all
        .iter()
        .filter(|r| r.uri.starts_with("api/customers"))
        .filter_map(|r| r.name.as_deref())
        .collect();
    assert_eq!(
        customers,
        [
            "customers.index",
            "customers.store",
            "customers.show",
            "customers.update"
        ]
    );

    let user = find(&all, "GET|HEAD", "api/user");
    assert_eq!(user.middleware, ["api", "auth:sanctum"]);
    assert_eq!(user.action_kind, ActionKind::Closure);

    let health = find(&all, "GET|HEAD", "up");
    assert_eq!(health.action_kind, ActionKind::Health);
    assert!(health.middleware.is_empty());

    // Route::controller()->prefix()->name()->group() with method-name actions.
    let show = find(&all, "GET|HEAD", "orders/{order}");
    assert_eq!(show.name.as_deref(), Some("orders.show"));
    assert_eq!(
        show.controller_fqn.as_deref(),
        Some("App\\Http\\Controllers\\OrderController")
    );
    assert_eq!(show.controller_method.as_deref(), Some("show"));
    assert_eq!(
        show.extra.wheres.get("order").map(String::as_str),
        Some("[0-9]+")
    );
    assert_eq!(
        find(&all, "GET|HEAD", "orders").name.as_deref(),
        Some("orders.index")
    );

    let about = find(&all, "GET|HEAD", "about");
    assert_eq!(about.action_kind, ActionKind::View);
    assert_eq!(about.extra.view.as_deref(), Some("about"));
    let home = find(&all, "GET|HEAD|POST|PUT|PATCH|DELETE|OPTIONS", "home");
    assert_eq!(home.action_kind, ActionKind::Redirect);
    assert_eq!(home.extra.redirect_to.as_deref(), Some("/"));
    assert_eq!(home.extra.status, Some(302));

    insta::assert_snapshot!("laravel11-slim", table(&store));
}

#[test]
fn laravel11_upgraded_routes() {
    let (store, report) = indexed(&fixture("laravel11-upgraded"));
    eprintln!("{}", table(&store));
    assert_eq!(report.laravel.version.as_deref(), Some("11.9.2"));
    assert_eq!(report.laravel.layout, Some(codemap_laravel::Layout::Legacy));
    let all = routes(&store);
    let show = find(&all, "GET|HEAD", "api/v1/invoices/{invoice}");
    assert_eq!(show.name.as_deref(), Some("v1.invoices.show"));
    assert_eq!(show.middleware, ["api"]);
    assert_eq!(show.location().as_deref(), Some("routes/api.php:8"));
    assert_eq!(all.len(), 3);
    insta::assert_snapshot!("laravel11-upgraded", table(&store));
}

#[test]
fn schema_dump_has_no_route_files() {
    let (store, report) = indexed(&fixture("schema-dump"));
    assert!(report.laravel.detected);
    assert!(routes(&store).is_empty());
}
