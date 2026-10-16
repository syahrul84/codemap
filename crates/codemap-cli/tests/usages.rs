//! find_usages across PHP, Vue / Nuxt, translations and tests.

use std::path::{Path, PathBuf};

use codemap_core::{Config, ScanOptions, Store};
use serde_json::{Value, json};

fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(rel)
        .canonicalize()
        .unwrap()
}

fn open(rel: &str, data: &Path) -> (Store, codemap_mcp::Project) {
    let root = fixture(rel);
    let project = codemap_mcp::Project {
        app: None,
        config: Config::load(&root).unwrap(),
        root,
        id: "u".into(),
        index_path: data.join("u").join("index.db"),
    };
    let mut store = Store::open(&project.index_path).unwrap();
    codemap_mcp::refresh(
        &mut store,
        &project.root,
        &project.config,
        ScanOptions::default(),
    )
    .unwrap();
    (store, project)
}

fn usages(store: &Store, project: &codemap_mcp::Project, target: &str) -> Value {
    codemap_mcp::call(
        store,
        project,
        "find_usages",
        json!({ "target": target }),
        codemap_mcp::Freshness::default(),
    )
    .unwrap()
    .data
}

#[test]
fn laravel_usages_and_tests() {
    let data = tempfile::tempdir().unwrap();
    let (store, project) = open("api-link/backend", data.path());

    let show = usages(&store, &project, "MallController::show");
    assert_eq!(
        show["resolved_as"],
        "method App\\Http\\Controllers\\MallController::show"
    );
    assert_eq!(
        show["routes"],
        json!(["GET|HEAD api/v1/malls/{mall:slug} [malls.show]"])
    );
    assert_eq!(
        show["tests"],
        json!([
            "tests/Feature/MallTest.php:18  GET /api/v1/malls/{}",
            "tests/Feature/MallTest.php:19  route('malls.show')"
        ])
    );

    let model = usages(&store, &project, "Mall");
    assert_eq!(model["resolved_as"], "class App\\Models\\Mall");
    assert!(
        model["usages"].as_array().unwrap().contains(&json!(
            "app/Http/Controllers/MallController.php:13  query()"
        )),
        "{model}"
    );
    assert_eq!(model["test_files"], json!(["tests/Feature/MallTest.php"]));

    let resource = usages(&store, &project, "App\\Http\\Resources\\OfferResource");
    assert_eq!(
        resource["usages"],
        json!([
            "app/Http/Controllers/MallController.php:23  collection()",
            "app/Http/Resources/MallResource.php:17  collection()"
        ])
    );

    let route = usages(&store, &project, "malls.index");
    assert_eq!(
        route["resolved_as"],
        "route malls.index (GET|HEAD api/v1/malls)"
    );
    assert_eq!(
        route["tests"],
        json!(["tests/Feature/MallTest.php:12  GET /api/v1/malls"])
    );
}

#[test]
fn vue_usages_and_tests() {
    let data = tempfile::tempdir().unwrap();
    let (store, project) = open("vue3-vite", data.path());

    let card = usages(&store, &project, "OrderCard");
    assert_eq!(
        card["resolved_as"],
        "component OrderCard (src/components/OrderCard.vue)"
    );
    let u: Vec<&str> = card["usages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(u.contains(&"src/views/OrdersView.vue:3  import"), "{u:?}");
    assert!(
        u.contains(&"src/views/OrdersView.vue:18  <OrderCard>"),
        "{u:?}"
    );
    assert!(u.contains(&"src/router/account.ts:7  import"), "{u:?}");
    assert_eq!(card["tests"], json!(["tests/OrderCard.spec.ts:2  import"]));
    assert_eq!(
        card["routes"],
        json!(["/account/orders/:id [account.order]"])
    );

    let api = usages(&store, &project, "useApi");
    assert_eq!(
        api["usages"],
        json!([
            "src/views/OrdersView.vue:4  import",
            "src/views/OrdersView.vue:9  useApi()"
        ])
    );
    let store_ = usages(&store, &project, "useCartStore");
    let s: Vec<&str> = store_["usages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        s.contains(&"src/views/OrdersView.vue:5  import")
            && s.contains(&"src/views/OrdersView.vue:8  useCartStore()"),
        "{s:?}"
    );

    let key = usages(&store, &project, "message.orders.title");
    assert_eq!(key["resolved_as"], "translation key message.orders.title");
    assert_eq!(
        key["usages"],
        json!(["src/views/OrdersView.vue:11  message.orders.title"])
    );

    let missing = usages(&store, &project, "NoSuchThing");
    assert_eq!(missing["found"], false);
}

#[test]
fn nuxt_auto_imported_composable() {
    let data = tempfile::tempdir().unwrap();
    let (store, project) = open("nuxt3", data.path());
    let c = usages(&store, &project, "useCart");
    assert_eq!(c["usages"], json!(["pages/orders/[id].vue:4  useCart()"]));
}

#[test]
fn response_fields_in_symbols_and_routes() {
    let data = tempfile::tempdir().unwrap();
    let (store, project) = open("api-link/backend", data.path());
    let call = |tool: &str, args: Value| {
        codemap_mcp::call(
            &store,
            &project,
            tool,
            args,
            codemap_mcp::Freshness::default(),
        )
        .unwrap()
        .data
    };
    let show = call("get_symbol", json!({ "name": "MallController::show" }));
    assert_eq!(
        show["returns"],
        "resource MallResource → data: id, slug, title, city, offers?{id, price}"
    );
    let cart = call("get_symbol", json!({ "name": "CartController::index" }));
    assert_eq!(cart["returns"], "json → data, count");
    let routes = call(
        "list_routes",
        json!({ "uri_contains": "malls", "detail": "full" }),
    );
    let r = routes["routes"].as_array().unwrap();
    assert!(
        r.iter().any(|x| x["returns"]
            == "collection MallResource → data: id, slug, title, city, offers?{id, price}"),
        "{routes}"
    );
}

#[test]
fn change_impact_in_a_vue_app() {
    let data = tempfile::tempdir().unwrap();
    let (store, project) = open("vue3-vite", data.path());
    let r = codemap_mcp::call(
        &store,
        &project,
        "find_usages",
        json!({ "changed_files": ["src/components/OrderCard.vue", "tests/OrderCard.spec.ts", "src/gone.ts"] }),
        codemap_mcp::Freshness::default(),
    )
    .unwrap()
    .data;
    assert_eq!(r["tests"], json!(["tests/OrderCard.spec.ts"]));
    assert_eq!(r["run"], json!(["npx vitest run tests/OrderCard.spec.ts"]));
    assert_eq!(r["changed"][0]["is"], "component OrderCard");
    assert_eq!(
        r["changed"][0]["used_in"],
        json!(["src/router/account.ts", "src/views/OrdersView.vue"])
    );
    assert_eq!(
        r["changed"][1]["is"],
        "not indexed (deleted, or not a code file codemap reads)"
    );
    assert_eq!(r["changed"][2]["is"], "a test");
}

#[test]
fn filament_admin_pages() {
    let data = tempfile::tempdir().unwrap();
    let (store, project) = open("api-link/backend", data.path());
    let r = codemap_mcp::call(
        &store,
        &project,
        "list_pages",
        json!({}),
        codemap_mcp::Freshness::default(),
    )
    .unwrap()
    .data;
    assert_eq!(
        r["pages"],
        json!([
            "/backoffice/malls [filament.admin.resources.malls.index] → app/Filament/Resources/Malls/Pages/ListMalls.php — resource MallResource · model Mall · form MallForm: name, city · table table(): name, city  app/Filament/Resources/Malls/MallResource.php:32",
            "/backoffice/malls/create [filament.admin.resources.malls.create] → app/Filament/Resources/Malls/Pages/CreateMall.php — resource MallResource  app/Filament/Resources/Malls/MallResource.php:33",
            "/backoffice/malls/{record}/edit [filament.admin.resources.malls.edit] → app/Filament/Resources/Malls/Pages/EditMall.php — resource MallResource  app/Filament/Resources/Malls/MallResource.php:34",
            "/backoffice/settings/general [filament.admin.pages.settings.general] → app/Filament/Pages/GeneralSettings.php — page \"General\" · fields: site_name, support_email  app/Filament/Pages/GeneralSettings.php:9"
        ])
    );
}

#[test]
fn settings_with_defaults_and_admin_pages() {
    let data = tempfile::tempdir().unwrap();
    let (store, project) = open("api-link/backend", data.path());
    let r = codemap_mcp::call(
        &store,
        &project,
        "get_schema",
        json!({ "settings": true }),
        codemap_mcp::Freshness::default(),
    )
    .unwrap()
    .data;
    assert_eq!(
        r["settings"],
        json!([
            "shop.open_hour = 9 (ShopHours::DEFAULT_OPEN) · read app/Support/ShopHours.php:15 · written app/Support/ShopHours.php:22 · admin /backoffice/settings/general"
        ])
    );
    assert_eq!(
        r["config"],
        json!([
            "shop.currency = 'MYR' (env SHOP_CURRENCY) config/shop.php:4",
            "shop.payments.timeout = 30 (env SHOP_PAYMENT_TIMEOUT) config/shop.php:6"
        ])
    );
}
