//! A Nuxt frontend linked to a Laravel backend: calls are matched to routes,
//! missing endpoints and fields the frontend reads but never receives are found.

use std::path::{Path, PathBuf};

use pidx_core::{Config, ScanOptions, Store};
use serde_json::{Value, json};

fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(rel)
        .canonicalize()
        .unwrap()
}

fn open(data: &Path) -> (Store, pidx_mcp::Project) {
    let root = fixture("api-link/frontend");
    let index_path = data.join("front").join("index.db");
    let project = pidx_mcp::Project {
        app: None,
        config: Config::load(&root).unwrap(),
        root,
        id: "front".into(),
        index_path,
    };
    let mut store = Store::open(&project.index_path).unwrap();
    pidx_mcp::refresh(
        &mut store,
        &project.root,
        &project.config,
        ScanOptions::default(),
    )
    .unwrap();
    (store, project)
}

fn call(store: &Store, project: &pidx_mcp::Project, tool: &str, args: Value) -> Value {
    pidx_mcp::call(store, project, tool, args, pidx_mcp::Freshness::default())
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
        json!({ "fields_not_returned": 2, "matched": 3, "method_mismatch": 1, "missing": 1 })
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
