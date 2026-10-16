//! Translation health for a Vue app (vue-i18n with a `message` namespace) and
//! a Laravel app (lang/ PHP files, `__()` / `@lang` in Blade).

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
        id: "t".into(),
        index_path: data.join("t").join("index.db"),
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
fn vue_i18n_health() {
    let data = tempfile::tempdir().unwrap();
    let (store, project) = open("vue3-vite", data.path());
    let h = codemap_mcp::i18n::health_json(store.conn(), &[]).unwrap();
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
    // Missing in a shipped language: not there for its users.
    assert_eq!(r[1]["status"], "not_found");
    assert_eq!(r[1]["detail"], "vue: missing in ms (defined in en)");
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
    // The problems are named, so no second lookup is needed.
    assert_eq!(
        o["i18n"][0]["missing_keys"],
        json!({ "ms": ["orders.empty"] })
    );
    assert_eq!(
        o["i18n"][0]["undefined_keys"],
        json!(["src/views/OrdersView.vue:13 message.orders.refunded"])
    );
}

#[test]
fn laravel_lang_health() {
    let data = tempfile::tempdir().unwrap();
    let (store, _) = open("api-link/backend", data.path());
    let h = codemap_mcp::i18n::health_json(store.conn(), &[]).unwrap();
    let l = &h[0];
    assert_eq!(l["source"], "laravel");
    assert_eq!(l["locales"], json!({ "en": 3, "ms": 3 }));
    // ms has a third of the keys: not shipped unless `[i18n] locales` says so.
    assert_eq!(l["missing"], json!({}));
    assert_eq!(l["partial"], json!({ "ms": "33%" }));
    assert_eq!(l["shipped_from"], "coverage");
    let shipped =
        codemap_mcp::i18n::health_json(store.conn(), &["en".into(), "ms".into()]).unwrap();
    assert_eq!(shipped[0]["missing"], json!({ "ms": 2 }));
    assert_eq!(shipped[0]["shipped_from"], "config");
    let en_only = codemap_mcp::i18n::health_json(store.conn(), &["en".into()]).unwrap();
    assert_eq!(en_only[0]["missing"], json!({}));
    assert_eq!(en_only[0]["partial"], json!({ "ms": "33%" }));
    assert_eq!(
        l["undefined"],
        json!(["resources/views/receipt.blade.php:3 orders.shipped"])
    );
    // Defined in ms only: English users see the key.
    assert!(
        shipped[0]["used_missing"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u
                .as_str()
                .unwrap()
                .contains("validation.attributes.price (missing in en; only ms has it")),
        "{}",
        shipped[0]
    );
    // `__('pending')`, `__('Closed for the day')` and one more fall back to themselves.
    assert_eq!(l["literal_text_uses"], 3);
    // Shipping ms: the one-word key shows English to Malay users.
    assert!(
        shipped[0]["undefined"].as_array().unwrap().iter().any(|u| u
            .as_str()
            .unwrap()
            .ends_with("pending (no ms translation: shows the word as written)")),
        "{}",
        shipped[0]["undefined"]
    );
}
