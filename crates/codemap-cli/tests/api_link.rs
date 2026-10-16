//! A Nuxt frontend linked to a Laravel backend: calls are matched to routes,
//! missing endpoints and fields the frontend reads but never receives are found.

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

fn open(data: &Path) -> (Store, codemap_mcp::Project) {
    let root = fixture("api-link/frontend");
    let index_path = data.join("front").join("index.db");
    let project = codemap_mcp::Project {
        app: None,
        config: Config::load(&root).unwrap(),
        root,
        id: "front".into(),
        index_path,
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

fn call(store: &Store, project: &codemap_mcp::Project, tool: &str, args: Value) -> Value {
    codemap_mcp::call(
        store,
        project,
        tool,
        args,
        codemap_mcp::Freshness::default(),
    )
    .unwrap()
    .data
}

#[test]
fn frontend_calls_are_linked_to_backend_routes() {
    let data = tempfile::tempdir().unwrap();
    let (store, project) = open(data.path());

    let all = call(&store, &project, "api_usage", json!({ "detail": "full" }));
    assert_eq!(all["backends"][0]["name"], "backend");
    assert_eq!(all["backends"][0]["api_base"], "/api/v1");
    assert_eq!(
        all["summary"],
        json!({ "fields_not_returned": 4, "matched": 10, "method_mismatch": 1, "missing": 2, "typed_not_returned": 1 })
    );
    let calls = all["calls"].as_array().unwrap();
    let by_at = |at: &str| {
        calls
            .iter()
            .find(|c| c["at"] == at)
            .cloned()
            .unwrap_or(Value::Null)
    };

    // Mall cards: the list reads `name`, the resource sends `title`.
    let list = by_at("pages/malls/index.vue:3");
    assert_eq!(list["route"], "GET|HEAD api/v1/malls");
    assert_eq!(list["read_but_not_returned"], json!(["name"]));
    // Quantity: the cart line reads `max_quantity`; the resource sends `max_per_order`.
    let cart = by_at("components/CartLine.vue:5");
    assert_eq!(cart["route"], "POST api/v1/carts/{merchant}/items");
    assert_eq!(cart["read_but_not_returned"], json!(["max_quantity"]));
    // Cards rendered by a list, with the data fetched through a helper
    // function: what the card reads is followed back to the helper's call
    // (card prop → list state → helper's return value → response).
    let helper = by_at("composables/api/malls.ts:10");
    assert_eq!(helper["route"], "GET|HEAD api/v1/malls");
    assert_eq!(
        helper["fields_read"],
        json!(["city", "id", "meta.total", "name", "slug"])
    );
    assert_eq!(helper["read_but_not_returned"], json!(["name"]));
    // Its TypeScript type (`{ data: Mall[] }`, `Mall` from the same file)
    // also requires `name`: the type promises a field the endpoint never sends.
    assert_eq!(helper["typed_but_not_returned"], json!(["name"]));
    // A list built in PHP step by step (helper, cache, filter, `$c + [...]`):
    // each item's keys are known, and `name` is not one of them.
    let nearby = by_at("pages/malls/nearby.vue:2");
    assert_eq!(nearby["route"], "GET|HEAD api/v1/nearby-malls");
    assert_eq!(nearby["read_but_not_returned"], json!(["data.name"]));
    // A bundle of helpers (`const { add } = useCartApi()`): the names picked
    // are functions, not response fields; `url(merchant)` is a URL builder.
    let calls_of = |file: &str| -> Vec<Value> {
        let v = call(
            &store,
            &project,
            "api_usage",
            json!({ "file": file, "detail": "full" }),
        );
        v["calls"].as_array().cloned().unwrap_or_default()
    };
    let bundle = calls_of("composables/useCartApi.ts");
    assert_eq!(bundle.len(), 4);
    assert_eq!(bundle[0]["route"], "POST api/v1/carts/{merchant}/items");
    assert_eq!(bundle[1]["route"], "GET|HEAD api/v1/carts");
    // `url(merchant, `/${item}`)`: the argument fills the helper's `suffix`.
    assert_eq!(
        bundle[2]["route"],
        "PATCH api/v1/carts/{merchant}/items/{item}"
    );
    // `${url(merchant)}/${item}`: a helper inside a template string.
    assert_eq!(
        bundle[3]["route"],
        "DELETE api/v1/carts/{merchant}/items/{item}"
    );
    assert!(bundle.iter().all(|c| c.get("fields_read").is_none()));
    // A wrapper method that adds to the URL it is given:
    // `recordsApi.get('/malls', slug, '/offers')` calls `/malls/{slug}/offers`,
    // not the mall list (whose fields would not include `price`).
    let wrapped = &calls_of("pages/malls/offers.vue")[0];
    assert_eq!(wrapped["at"], "pages/malls/offers.vue:6");
    assert_eq!(wrapped["route"], "GET|HEAD api/v1/malls/{mall:slug}/offers");
    assert!(wrapped.get("read_but_not_returned").is_none());
    // A broken call inside a function nothing uses: dead code.
    let dead = by_at("composables/api/orders.ts:3");
    assert_eq!(dead["status"], "missing");
    assert_eq!(dead["in_unused_function"], "reserveListing");
    // A call to an endpoint that does not exist, and one with the wrong method.
    assert_eq!(by_at("pages/malls/[slug].vue:4")["status"], "missing");
    let wrong = by_at("pages/malls/[slug].vue:6");
    assert_eq!(wrong["status"], "method_mismatch");
    assert_eq!(wrong["path_exists_as"], json!(["GET|HEAD api/v1/malls"]));

    // The detail page reads only fields the resource returns.
    let file = call(
        &store,
        &project,
        "api_usage",
        json!({ "file": "pages/malls/[slug].vue" }),
    );
    let show = &file["calls"][0];
    assert_eq!(show["route"], "GET|HEAD api/v1/malls/{mall:slug}");
    assert_eq!(
        show["fields_read"],
        json!(["offers.id", "offers.price", "title"])
    );
    assert!(show.get("read_but_not_returned").is_none());

    // One endpoint: its response shape and callers.
    let ep = call(
        &store,
        &project,
        "api_usage",
        json!({ "endpoint": "GET api/v1/malls/{mall}" }),
    );
    let e = &ep["endpoints"][0];
    assert_eq!(e["returns"]["kind"], "resource");
    assert_eq!(
        e["returns"]["resource"],
        "App\\Http\\Resources\\MallResource"
    );
    assert_eq!(e["returns"]["complete"], true);
    let names: Vec<&str> = e["returns"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["id", "slug", "title", "city", "offers"]);
    assert_eq!(e["callers"][0]["at"], "pages/malls/[slug].vue:3");

    // verify endpoints the frontend relies on.
    let v = call(
        &store,
        &project,
        "verify",
        json!({ "claims": { "endpoints": ["GET /malls/{slug}", "POST /carts/{id}/items", "DELETE /malls/{slug}", "GET /orders/history"] } }),
    );
    let statuses: Vec<&str> = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["exists", "exists", "not_found", "not_found"]);
    assert_eq!(
        v["results"][2]["suggestions"],
        json!(["GET|HEAD api/v1/malls/{mall:slug}"])
    );
}

#[test]
fn change_impact_names_browser_tests() {
    let data = tempfile::tempdir().unwrap();
    let (store, project) = open(data.path());

    // A screen: the browser test that opens it (through a page object).
    let page = call(
        &store,
        &project,
        "find_usages",
        json!({ "changed_files": ["pages/malls/[slug].vue"] }),
    );
    assert_eq!(page["screens"], json!(["/malls/:slug"]));
    assert_eq!(
        page["browser_tests"],
        json!(["tests/e2e/mall-detail.spec.ts  (opens /malls/{} (screen /malls/:slug))"])
    );
    // The project uses pnpm.
    assert_eq!(
        page["run"],
        json!(["pnpm exec playwright test tests/e2e/mall-detail.spec.ts"])
    );

    // A component: browser tests looking for the test ids it sets.
    let card = call(
        &store,
        &project,
        "find_usages",
        json!({ "changed_files": ["components/MallCard.vue"] }),
    );
    assert_eq!(
        card["browser_tests"],
        json!([
            "tests/e2e/mall-cards.spec.ts  (looks for test id mall-card (set in the changed code as mall-card))"
        ])
    );

    // Unused code exists; it is not "not found".
    let unused = call(
        &store,
        &project,
        "find_usages",
        json!({ "target": "MallList" }),
    );
    assert_eq!(unused["found"], true);
    assert_eq!(unused["unused"], true);
    let reserve = call(
        &store,
        &project,
        "find_usages",
        json!({ "target": "reserveListing" }),
    );
    assert_eq!(reserve["found"], true);
    assert_eq!(reserve["unused"], true);
}

#[test]
fn code_map_of_a_frontend_includes_its_linked_backend() {
    let data = tempfile::tempdir().unwrap();
    let (store, project) = open(data.path());
    let mut map = codemap_mcp::map::data(&[(&store, &project)], "front").unwrap();
    codemap_mcp::map::prune(&mut map);
    let count = |k: &str| {
        map["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|n| n["kind"] == k && n["stub"] != true)
            .count()
    };
    assert!(count("screen") > 0);
    assert!(
        count("endpoint") >= 8,
        "the backend's endpoints are mapped, not stubs"
    );
    assert!(count("controller") > 0 && count("model") > 0);
    assert!(
        map["apps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["name"] == "backend" && a["linked"] == true)
    );
}
