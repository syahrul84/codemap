//! Spawns `codemap serve` and talks MCP over stdio with rmcp's client.

use std::path::{Path, PathBuf};

use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::{Value, json};
use tokio::process::Command;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
        .canonicalize()
        .unwrap()
}

async fn connect(project: &Path, data: &Path) -> RunningService<RoleClient, ()> {
    let transport = TokioChildProcess::new(Command::new(env!("CARGO_BIN_EXE_codemap")).configure(
        |cmd| {
            cmd.env("CODEMAP_NO_UPDATE_CHECK", "1")
                .args(["serve", "--project"])
                .arg(project)
                .arg("--data-dir")
                .arg(data)
                .args(["--project-id", "mcp-test", "--log-level", "warn"]);
        },
    ))
    .unwrap();
    ().serve(transport).await.expect("MCP handshake")
}

async fn call(client: &RunningService<RoleClient, ()>, tool: &'static str, args: Value) -> Value {
    let result = client
        .call_tool(
            CallToolRequestParams::new(tool).with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .unwrap_or_else(|e| panic!("{tool}: {e}"));
    let text = &result.content[0].as_text().expect("text content").text;
    serde_json::from_str(text).expect("JSON envelope")
}

fn assert_envelope(v: &Value) {
    // Always present; the other fields only when not at their default.
    for key in ["project", "indexed_at", "git_commit"] {
        assert!(v["meta"].get(key).is_some(), "meta.{key} missing in {v}");
    }
    for key in ["warnings", "next_cursor"] {
        if let Some(x) = v["meta"].get(key) {
            assert!(
                !x.is_null() && x != &serde_json::json!([]),
                "meta.{key} at its default in {v}"
            );
        }
    }
    assert!(v.get("data").is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn all_tools_over_stdio() {
    let data = tempfile::tempdir().unwrap();
    let client = connect(&fixture("laravel11-slim"), data.path()).await;

    let tools: Vec<String> = client
        .list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    for name in [
        "project_overview",
        "search_symbol",
        "get_symbol",
        "list_routes",
        "get_schema",
    ] {
        assert!(
            tools.contains(&name.to_owned()),
            "{name} missing from {tools:?}"
        );
    }

    let overview = call(&client, "project_overview", json!({})).await;
    assert_envelope(&overview);
    assert_eq!(overview["meta"]["project"], "acme/slim-shop");
    assert!(overview["meta"].get("stale").is_none());
    assert_eq!(overview["data"]["framework"]["laravel"], "11.31.0");
    assert_eq!(overview["data"]["counts"]["routes"], 13);
    // brief stays well under ~1,500 tokens
    assert!(overview["data"].to_string().len() < 6000);

    let found = call(&client, "search_symbol", json!({ "query": "OrderStatus" })).await;
    assert_eq!(
        found["data"]["matches"][0]["fqn"],
        "App\\Enums\\OrderStatus"
    );
    let missing = call(&client, "search_symbol", json!({ "query": "OrderStatsu" })).await;
    assert_eq!(missing["data"]["found"], false);
    assert_eq!(missing["data"]["suggestions"][0]["name"], "OrderStatus");
    // A kind filter that rules out the only match falls back to other kinds.
    let other_kind = call(
        &client,
        "search_symbol",
        json!({ "query": "OrderStatus", "kind": "class" }),
    )
    .await;
    assert_eq!(other_kind["data"]["found"], true);
    assert_eq!(
        other_kind["data"]["matches"][0]["fqn"],
        "App\\Enums\\OrderStatus"
    );
    assert_eq!(
        other_kind["data"]["note"],
        "No class matches; showing other kinds."
    );
    // Same words in another order still find it.
    let words = call(
        &client,
        "search_symbol",
        json!({ "query": "StatusOfOrder" }),
    )
    .await;
    assert_eq!(words["data"]["found"], false);
    assert_eq!(words["data"]["suggestions"][0]["name"], "OrderStatus");

    let model = call(
        &client,
        "get_symbol",
        json!({ "name": "App\\Models\\Order" }),
    )
    .await;
    assert_eq!(model["data"]["model"]["table"], "orders");
    // Brief: `table_exists` only appears when the table is missing.
    assert!(model["data"]["model"].get("table_exists").is_none());
    assert!(
        model["data"]["model"]["relations"]
            .as_array()
            .unwrap()
            .len()
            >= 3
    );

    let controller = call(&client, "get_symbol", json!({ "name": "OrderController" })).await;
    assert!(controller["data"]["routes"].as_array().unwrap().len() >= 3);
    assert_eq!(
        controller["data"]["form_requests"][0]["request"],
        "App\\Http\\Requests\\StoreOrderRequest"
    );

    let method = call(
        &client,
        "get_symbol",
        json!({ "name": "OrderController::store", "include_source": true }),
    )
    .await;
    assert_eq!(
        method["data"]["form_request"]["class"],
        "App\\Http\\Requests\\StoreOrderRequest"
    );
    assert!(
        method["data"]["source"]["text"]
            .as_str()
            .unwrap()
            .contains("function store")
    );

    let request = call(
        &client,
        "get_symbol",
        json!({ "name": "StoreOrderRequest" }),
    )
    .await;
    assert_eq!(
        request["data"]["form_request"]["rules"]["total"][0],
        "required"
    );

    let ambiguous = call(&client, "get_symbol", json!({ "name": "Controller" })).await;
    assert_eq!(ambiguous["data"]["found"], true);
    assert!(
        ambiguous["data"].get("ambiguous").is_none(),
        "only one class named Controller"
    );
    let none = call(&client, "get_symbol", json!({ "name": "OrderServis" })).await;
    assert_eq!(none["data"]["found"], false);

    let page = call(&client, "list_routes", json!({ "limit": 5 })).await;
    assert_eq!(page["data"]["routes"].as_array().unwrap().len(), 5);
    assert_eq!(page["meta"]["truncated"], true);
    let cursor = page["meta"]["next_cursor"].as_str().unwrap().to_owned();
    let next = call(
        &client,
        "list_routes",
        json!({ "limit": 5, "cursor": cursor }),
    )
    .await;
    assert_ne!(next["data"]["routes"][0], page["data"]["routes"][0]);

    let table = call(&client, "get_schema", json!({ "table": "orders" })).await;
    let columns: Vec<&str> = table["data"]["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(columns.contains(&"customer_id") && columns.contains(&"total"));
    let no_table = call(&client, "get_schema", json!({ "table": "order" })).await;
    assert_eq!(no_table["data"]["found"], false);
    assert!(
        no_table["data"]["hint"]
            .as_str()
            .unwrap()
            .starts_with("Did you mean")
    );

    // Invalid arguments are a protocol error, not a crash.
    let bad = client
        .call_tool(
            CallToolRequestParams::new("search_symbol")
                .with_arguments(json!({ "limit": "x" }).as_object().unwrap().clone()),
        )
        .await;
    // Either a protocol error or a tool result flagged isError (rmcp validates
    // arguments and reports failures as tool errors, as the MCP spec suggests).
    assert!(bad.map_or(true, |r| r.is_error == Some(true)));

    let checked = call(
        &client,
        "verify",
        json!({ "claims": { "tables": ["orders", "ordres"] } }),
    )
    .await;
    assert_eq!(checked["data"]["summary"], "1 ok, 1 not_found");
    assert_eq!(checked["data"]["results"][1]["suggestions"][0], "orders");

    client.cancel().await.unwrap();
}

fn copy_dir(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&target).unwrap();
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn edits_are_picked_up_before_answering() {
    let project = tempfile::tempdir().unwrap();
    copy_dir(&fixture("laravel11-upgraded"), project.path());
    std::fs::write(
        project.path().join(".codemap.toml"),
        "[freshness]\ncheck_interval_ms = 0\n",
    )
    .unwrap();
    let data = tempfile::tempdir().unwrap();
    let client = connect(project.path(), data.path()).await;

    let before = call(
        &client,
        "search_symbol",
        json!({ "query": "RefundService" }),
    )
    .await;
    assert_eq!(before["data"]["found"], false);

    std::fs::create_dir_all(project.path().join("app/Services")).unwrap();
    std::fs::write(
        project.path().join("app/Services/RefundService.php"),
        "<?php\nnamespace App\\Services;\n\nclass RefundService\n{\n    public function refund(int $invoiceId): void {}\n}\n",
    )
    .unwrap();
    let after = call(
        &client,
        "search_symbol",
        json!({ "query": "RefundService" }),
    )
    .await;
    assert_eq!(after["data"]["found"], true);
    assert_eq!(after["meta"]["reindexed_files"], 1);
    assert!(after["meta"].get("stale").is_none());
    client.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn first_call_never_claims_fresh_while_warm_up_runs() {
    let project = tempfile::tempdir().unwrap();
    copy_dir(&fixture("laravel11-upgraded"), project.path());
    let data = tempfile::tempdir().unwrap();
    // An existing, now outdated index (default 2 s check interval).
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_codemap"))
        .env("CODEMAP_NO_UPDATE_CHECK", "1")
        .args(["scan", "--project"])
        .arg(project.path())
        .arg("--data-dir")
        .arg(data.path())
        .args(["--project-id", "mcp-test"])
        .output()
        .unwrap()
        .status;
    assert!(status.success());
    std::fs::write(
        project.path().join("app/Models/Payment.php"),
        "<?php\nnamespace App\\Models;\n\nuse Illuminate\\Database\\Eloquent\\Model;\n\nclass Payment extends Model {}\n",
    )
    .unwrap();

    let client = connect(project.path(), data.path()).await;
    let v = call(&client, "search_symbol", json!({ "query": "Payment" })).await;
    let found = v["data"]["found"] == true;
    let stale = v["meta"]["stale"] == true;
    assert!(
        found || stale,
        "answered not-found from an outdated index without stale: {v}"
    );
    client.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn vue_tools_over_stdio() {
    let data = tempfile::tempdir().unwrap();
    let client = connect(&fixture("nuxt3"), data.path()).await;
    let tools: Vec<String> = client
        .list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    assert!(tools.contains(&"get_component".to_owned()));
    assert!(tools.len() <= 10, "tool budget: {tools:?}");

    let overview = call(&client, "project_overview", json!({})).await;
    assert_envelope(&overview);
    let fe = &overview["data"]["frontend"];
    assert_eq!(
        fe["framework"].as_str().map(|s| s.starts_with("Nuxt")),
        Some(true)
    );
    assert_eq!(fe["counts"]["pages"], 3);

    let pages = call(&client, "list_pages", json!({ "path_contains": "orders" })).await;
    assert_eq!(
        pages["data"]["pages"],
        json!([
            "/orders/:id [orders-id] → pages/orders/[id].vue (layout default) (middleware auth)  pages/orders/[id].vue:1",
            "/orders/:id/invoice [order-invoice] → pages/orders/[id]/invoice.vue (layout default) (middleware auth)  pages/orders/[id]/invoice.vue:1"
        ])
    );
    assert_eq!(fe["unresolved_component_tags"], 1);

    let c = call(&client, "get_component", json!({ "name": "base-button" })).await;
    assert_envelope(&c);
    assert_eq!(c["data"]["file"], "components/base/Button.vue");
    assert_eq!(
        c["data"]["props"],
        json!(["label: string (required)", "disabled: boolean"])
    );

    let missing = call(
        &client,
        "get_component",
        json!({ "name": "OrderOrderSummary" }),
    )
    .await;
    assert_eq!(missing["data"]["found"], false);
    assert_eq!(missing["data"]["hint"], "Did you mean OrderSummary?");

    let s = call(
        &client,
        "search_symbol",
        json!({ "query": "cart", "kind": "store" }),
    )
    .await;
    assert_eq!(s["data"]["matches"][0]["name"], "useCartStore");

    let g = call(&client, "get_symbol", json!({ "name": "useCart" })).await;
    assert_eq!(g["data"]["kind"], "composable");
    assert_eq!(g["data"]["file"], "composables/useCart.ts");

    let v = call(
        &client,
        "verify",
        json!({ "claims": { "components": ["OrderSummary", "UButton", "NuxtLink", "OrderSumary"] } }),
    )
    .await;
    let statuses: Vec<(String, String)> = v["data"]["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["item"].as_str().unwrap().to_owned(),
                r["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        statuses,
        [
            ("OrderSummary".to_owned(), "exists".to_owned()),
            ("UButton".to_owned(), "external".to_owned()),
            ("NuxtLink".to_owned(), "external".to_owned()),
            ("OrderSumary".to_owned(), "not_found".to_owned()),
        ]
    );
    assert_eq!(
        v["data"]["results"][3]["suggestions"],
        json!(["OrderSummary"])
    );
    client.cancel().await.unwrap();
}
