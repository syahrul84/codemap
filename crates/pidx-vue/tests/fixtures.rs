//! Indexes the hand-written Vue / Nuxt fixtures and snapshots what was
//! understood. Key expectations are also asserted explicitly.

use std::path::{Path, PathBuf};

use pidx_core::envelope::Detail;
use pidx_core::{Config, ScanOptions, Store};
use serde_json::{Value, json};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
        .canonicalize()
        .unwrap()
}

fn indexed(root: &Path) -> Store {
    let mut store = Store::open_in_memory().unwrap();
    let scan = pidx_core::scan::scan(&mut store, root, &Config::default(), ScanOptions::default())
        .unwrap();
    let report = pidx_vue::refresh(&mut store, root, &scan, false).unwrap();
    assert!(report.detected);
    assert_eq!(report.js.warnings, 0, "parse warnings in fixture");
    store
}

fn rows(store: &Store, sql: &str) -> Vec<Value> {
    let mut stmt = store.conn().prepare(sql).unwrap();
    let n = stmt.column_count();
    stmt.query_map([], |r| {
        let mut out = Vec::new();
        for i in 0..n {
            let v: rusqlite::types::Value = r.get(i)?;
            out.push(match v {
                rusqlite::types::Value::Null => Value::Null,
                rusqlite::types::Value::Integer(i) => json!(i),
                rusqlite::types::Value::Text(t) => json!(t),
                other => json!(format!("{other:?}")),
            });
        }
        Ok(Value::Array(out))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

fn summary(store: &Store) -> Value {
    let d = pidx_vue::stored_detection(store).unwrap().unwrap();
    json!({
        "detection": {
            "vue_major": d.vue_major,
            "nuxt": d.nuxt.as_ref().map(|n| (n.major, n.auto_import_components)),
            "src_dir": d.src_dir,
            "build": d.build,
            "aliases": d.aliases,
            "component_dirs": d.component_dirs,
            "libraries": d.libraries.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
            "ui": d.ui.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
        },
        "modules": rows(store, "SELECT path, kind, name FROM js_modules ORDER BY path"),
        "uses": rows(store, "SELECT from_path, tag, line, to_path, via, package FROM component_uses ORDER BY from_path, line, tag"),
        "symbols": rows(store, "SELECT s.kind, s.name, s.fqn, s.signature, s.doc_summary FROM symbols s ORDER BY s.fqn"),
        "pages": rows(store, "SELECT route_path, name, file, source, defined_in, parent, layout, middleware_json, redirect, note FROM pages ORDER BY route_path, defined_in"),
    })
}

fn uses(store: &Store, from: &str) -> Vec<(String, Option<String>, String)> {
    let mut stmt = store
        .conn()
        .prepare(
            "SELECT tag, to_path, via FROM component_uses WHERE from_path = ?1 ORDER BY line, tag",
        )
        .unwrap();
    stmt.query_map([from], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn component(store: &Store, name: &str, major: Option<u32>) -> Value {
    pidx_vue::component::get_component(store, name, Detail::Brief, major).unwrap()
}

#[test]
fn vue2_cli() {
    let store = indexed(&fixture("vue2-cli"));
    insta::assert_json_snapshot!("vue2-cli", summary(&store));
    let u = uses(&store, "src/views/Orders.vue");
    let s = |t: &str| u.iter().find(|x| x.0 == t).cloned().unwrap();
    // Global registration through an alias import.
    assert_eq!(
        s("app-card"),
        (
            "app-card".into(),
            Some("src/components/AppCard.vue".into()),
            "global".into()
        )
    );
    // require.context base components.
    assert_eq!(
        s("BaseButton").1.as_deref(),
        Some("src/components/base/BaseButton.vue")
    );
    assert_eq!(s("BaseButton").2, "glob");
    assert_eq!(s("order-row").2, "local");
    assert_eq!(s("x-help").2, "inline");
    assert_eq!(s("v-btn").2, "external");
    assert_eq!(s("VueSignaturePad").2, "external");
    // A real mistake: the import points at a file that does not exist.
    assert_eq!(s("missing-widget").2, "unresolved");
    assert_eq!(s("ghost-panel").2, "unresolved");

    let c = component(&store, "app-card", Some(2));
    assert_eq!(
        c["props"],
        json!(["heading: String = ''", "closeable: Boolean (required)"])
    );
    assert_eq!(c["emits"], json!(["close"]));
    assert_eq!(c["slots"], json!(["default", "footer"]));
    assert_eq!(
        c["used_by"],
        json!(["src/views/Orders.vue:2", "src/views/admin/Dashboard.vue:2"])
    );
    let routes = rows(
        &store,
        "SELECT route_path, name, file, note FROM pages ORDER BY defined_in",
    );
    assert_eq!(
        routes,
        [
            json!(["/admin", null, "src/views/admin/AdminLayout.vue", null]),
            json!([
                "/admin",
                "admin.home",
                "src/views/admin/Dashboard.vue",
                null
            ]),
            json!([
                "/admin/users/:id",
                "admin.user",
                null,
                "component file not found: src/views/admin/Missing.vue"
            ]),
            json!(["/", null, null, null]),
            json!(["/orders", "orders", "src/views/Orders.vue", null]),
            json!([
                "(dynamic)",
                null,
                null,
                "built at runtime: helperRoute('/reports')"
            ]),
        ]
    );
    let row = component(&store, "OrderRow", Some(2));
    assert_eq!(
        row["v_model"],
        json!(["v-model (prop `value`, event `input`)"])
    );
    let orders = component(&store, "src/views/Orders.vue", Some(2));
    assert_eq!(orders["name"], "OrdersPage");
    assert_eq!(
        orders["stores"],
        json!([
            "vuex state cart/items :28",
            "vuex getter cart/count :29",
            "vuex action cart/load :33"
        ])
    );
    assert_eq!(orders["mixins"], json!(["formMixin (../mixins/form)"]));
}

#[test]
fn vue3_vite() {
    let store = indexed(&fixture("vue3-vite"));
    insta::assert_json_snapshot!("vue3-vite", summary(&store));
    let u = uses(&store, "src/views/OrdersView.vue");
    let via: Vec<(&str, &str)> = u.iter().map(|x| (x.0.as_str(), x.2.as_str())).collect();
    assert_eq!(
        via,
        [
            ("DefineRow", "local-binding"),
            ("OrderCard", "local"),
            ("i18n-t", "external"),
            ("ReuseRow", "local-binding"),
            ("OrdersView", "self"),
            ("UnknownThing", "unresolved"),
        ]
    );
    let routes = rows(
        &store,
        "SELECT route_path, name, file, parent FROM pages ORDER BY route_path",
    );
    assert_eq!(
        routes,
        [
            json!(["/", "orders", "src/views/OrdersView.vue", null]),
            json!(["/account", null, "src/views/OrdersView.vue", null]),
            json!([
                "/account/orders/:id",
                "account.order",
                "src/components/OrderCard.vue",
                "/account"
            ]),
        ]
    );
    let c = component(&store, "OrderCard", Some(3));
    assert_eq!(
        c["props"],
        json!([
            "order: Order (required)",
            "compact: boolean = false",
            "size: 'sm' | 'lg' = 'sm'",
            "quantity: number = 1"
        ])
    );
    assert_eq!(c["emits"], json!(["select", "remove"]));
    assert_eq!(c["v_model"], json!(["v-model:quantity"]));
    assert_eq!(c["slots"], json!(["default", "actions"]));
    let view = component(&store, "OrdersView", Some(3));
    assert_eq!(
        view["stores"],
        json!(["useCartStore → src/stores/cart.ts:4"])
    );
    assert_eq!(
        view["composables"],
        json!(["useApi :9 → src/composables/useApi.ts:2", "useI18n :10"])
    );
}

#[test]
fn nuxt3() {
    let store = indexed(&fixture("nuxt3"));
    insta::assert_json_snapshot!("nuxt3", summary(&store));
    let u = uses(&store, "pages/index.vue");
    assert_eq!(
        u,
        [
            (
                "LazyAppHeader".into(),
                Some("components/AppHeader.vue".into()),
                "auto".into()
            ),
            // Nuxt names `components/order/OrderSummary.vue` `OrderSummary`, so
            // `<OrderOrderSummary>` would fail at runtime too.
            ("OrderOrderSummary".into(), None, "unresolved".into()),
        ]
    );
    let page = uses(&store, "pages/orders/[id].vue");
    assert_eq!(
        page[0].1.as_deref(),
        Some("components/order/OrderSummary.vue")
    );
    let b = component(&store, "BaseButton", Some(3));
    assert_eq!(b["file"], "components/base/Button.vue");
    assert_eq!(b["renders"], json!(["<UButton> :7 → external: @nuxt/ui"]));
    assert_eq!(b["used_by"], json!(["components/AppHeader.vue:2"]));
    let nested = rows(
        &store,
        "SELECT route_path, name, parent, middleware_json FROM pages WHERE file LIKE '%invoice%'",
    );
    assert_eq!(
        nested,
        [json!([
            "/orders/:id/invoice",
            "order-invoice",
            "/orders/:id",
            "[\"auth\"]"
        ])]
    );
    let p = component(&store, "pages/orders/[id].vue", Some(3));
    assert_eq!(p["kind"], "page");
    assert_eq!(
        p["page_meta"],
        json!({ "layout": "default", "middleware": "['auth']" })
    );
}

#[test]
fn nuxt2() {
    let store = indexed(&fixture("nuxt2"));
    insta::assert_json_snapshot!("nuxt2", summary(&store));
    assert_eq!(
        uses(&store, "pages/orders/_id.vue")[0],
        (
            "BaseCard".into(),
            Some("components/Base/Card.vue".into()),
            "auto".into()
        )
    );
    let page = component(&store, "pages/index.vue", Some(2));
    assert_eq!(
        page["page"],
        json!({ "layout": "admin", "middleware": ["auth"] })
    );
    assert_eq!(page["stores"], json!(["vuex action orders/fetch :10"]));
    let ns = rows(
        &store,
        "SELECT name, fqn FROM symbols WHERE kind = 'store' ORDER BY fqn",
    );
    assert_eq!(
        ns,
        [
            json!(["root", "store/index.js"]),
            json!(["orders", "store/orders.js"])
        ]
    );
}
