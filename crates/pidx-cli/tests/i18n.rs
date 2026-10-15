//! Translation health for a Vue app (vue-i18n with a `message` namespace) and
//! a Laravel app (lang/ PHP files, `__()` / `@lang` in Blade).

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

fn open(rel: &str, data: &Path) -> (Store, pidx_mcp::Project) {
    let root = fixture(rel);
    let project = pidx_mcp::Project {
        app: None,
        config: Config::load(&root).unwrap(),
        root,
        id: "t".into(),
        index_path: data.join("t").join("index.db"),
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
fn vue_i18n_health() {
    let data = tempfile::tempdir().unwrap();
    let (store, project) = open("vue3-vite", data.path());
    let h = pidx_mcp::i18n::health_json(store.conn()).unwrap();
    let vue = &h[0];
    assert_eq!(vue["source"], "vue");
    assert_eq!(vue["base"], "en");
    assert_eq!(vue["wrapper"], "message");
    assert_eq!(vue["locales"], json!({ "en": 5, "ms": 4 }));
    assert_eq!(vue["missing"], json!({ "ms": 1 }));
    assert_eq!(vue["missing_samples"]["ms"], json!(["orders.empty"]));
    assert_eq!(vue["undefined_total"], 1);
    assert_eq!(
        vue["undefined"],
        json!(["src/views/OrdersView.vue:13 message.orders.refunded"])
    );

    let v = call(
        &store,
        &project,
        "verify",
        json!({ "claims": { "i18n_keys": ["message.orders.title", "message.orders.empty", "message.orders.titel", "message.orders.status."] }, "detail": "full" }),
    );
    let r = v["results"].as_array().unwrap();
    assert_eq!(r[0]["status"], "exists");
    assert_eq!(r[1]["status"], "exists");
    assert_eq!(r[1]["detail"], "vue: missing in ms");
    assert_eq!(r[2]["status"], "not_found");
    assert_eq!(r[2]["suggestions"][0], "message.orders.title");
    assert_eq!(r[3]["status"], "exists");

    let c = call(
        &store,
        &project,
        "get_component",
        json!({ "name": "OrdersView" }),
    );
    assert_eq!(
        c["i18n"]["undefined"],
        json!(["message.orders.refunded :13"])
    );
    let o = call(&store, &project, "project_overview", json!({}));
    assert_eq!(o["i18n"][0]["missing_per_locale"], json!({ "ms": 1 }));
}

#[test]
fn laravel_lang_health() {
    let data = tempfile::tempdir().unwrap();
    let (store, _) = open("api-link/backend", data.path());
    let h = pidx_mcp::i18n::health_json(store.conn()).unwrap();
    let l = &h[0];
    assert_eq!(l["source"], "laravel");
    assert_eq!(l["locales"], json!({ "en": 3, "ms": 1 }));
    assert_eq!(l["missing"], json!({ "ms": 2 }));
    assert_eq!(
        l["undefined"],
        json!(["resources/views/receipt.blade.php:3 orders.shipped"])
    );
    assert_eq!(l["literal_text_uses"], 1);
}
