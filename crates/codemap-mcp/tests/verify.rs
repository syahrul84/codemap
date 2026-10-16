//! verify: every status, for claims and php_snippet.

use std::path::{Path, PathBuf};

use codemap_core::{Config, ScanOptions, Store};
use codemap_mcp::{Freshness, Project, call};
use serde_json::{Value, json};

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

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

struct Fx {
    _dir: tempfile::TempDir,
    store: Store,
    project: Project,
}

fn setup() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/laravel11-slim");
    copy_dir(&fixture, &root);
    write(
        &root,
        "vendor/composer/autoload_psr4.php",
        "<?php\n\n$vendorDir = dirname(__DIR__);\n$baseDir = dirname($vendorDir);\n\nreturn array(\n    'Illuminate\\\\' => array($vendorDir . '/laravel/framework/src/Illuminate'),\n    'App\\\\' => array($baseDir . '/app'),\n);\n",
    );
    write(
        &root,
        "vendor/laravel/framework/src/Illuminate/Database/Eloquent/Model.php",
        "<?php\n",
    );
    write(
        &root,
        "app/Support/Helper.php",
        "<?php\nnamespace App\\Support;\nclass Helper {}\n",
    );
    write(
        &root,
        "app/Other/Helper.php",
        "<?php\nnamespace App\\Other;\nclass Helper {}\n",
    );
    write(
        &root,
        "app/Services/Legacy.php",
        "<?php\nnamespace App\\Services;\nclass Legacy extends \\Some\\Unknown\\Base {}\n",
    );
    let mut store = Store::open_in_memory().unwrap();
    codemap_laravel::refresh(
        &mut store,
        &root,
        &Config::default(),
        ScanOptions::default(),
    )
    .unwrap();
    let project = Project {
        app: None,
        root,
        config: Config::default(),
        id: "t".into(),
        index_path: PathBuf::new(),
    };
    Fx {
        _dir: dir,
        store,
        project,
    }
}

fn verify(fx: &Fx, args: Value) -> Value {
    let env = call(&fx.store, &fx.project, "verify", args, Freshness::default()).unwrap();
    serde_json::to_value(env).unwrap()["data"].clone()
}

fn status<'a>(data: &'a Value, item: &str) -> &'a Value {
    data["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["item"] == item)
        .unwrap_or_else(|| panic!("{item} not in {data:#}"))
}

#[test]
fn classes_all_statuses() {
    let fx = setup();
    let d = verify(
        &fx,
        json!({ "claims": { "classes": [
            "App\\Models\\Order", "Order", "App\\Models\\Ordr", "Illuminate\\Database\\Eloquent\\Model",
            "Illuminate\\Support\\Fluentish", "Exception", "Helper"
        ] }, "detail": "full" }),
    );
    assert_eq!(status(&d, "App\\Models\\Order")["status"], "exists");
    assert_eq!(
        status(&d, "App\\Models\\Order")["file"],
        "app/Models/Order.php:15"
    );
    assert_eq!(
        status(&d, "Order")["detail"],
        "resolved to App\\Models\\Order"
    );
    let ordr = status(&d, "App\\Models\\Ordr");
    assert_eq!(ordr["status"], "not_found");
    assert_eq!(ordr["suggestions"][0], "App\\Models\\Order");
    let model = status(&d, "Illuminate\\Database\\Eloquent\\Model");
    assert_eq!(model["status"], "external");
    assert!(
        model["detail"]
            .as_str()
            .unwrap()
            .contains("vendor/laravel/framework/src/Illuminate/Database/Eloquent/Model.php")
    );
    // vendor/ exists, but the class file does not: a made-up vendor class.
    assert_eq!(
        status(&d, "Illuminate\\Support\\Fluentish")["status"],
        "not_found"
    );
    assert_eq!(status(&d, "Exception")["status"], "external");
    let helper = status(&d, "Helper");
    assert_eq!(helper["status"], "unresolved");
    assert_eq!(helper["suggestions"].as_array().unwrap().len(), 2);
    assert_eq!(d["summary"], "2 ok, 2 not_found, 2 external, 1 unresolved");
    assert_eq!(d["all_exist"], false);
}

#[test]
fn methods_functions_routes_tables_columns_relations() {
    let fx = setup();
    let d = verify(
        &fx,
        json!({ "claims": {
            "methods": [
                "App\\Models\\Order::customer", "Order::paid", "App\\Models\\Order::where",
                "OrderController::archive", "App\\Services\\Legacy::run"
            ],
            "functions": ["now", "array_map", "formatMoney"],
            "routes": ["orders.index", "orders.archive", "POST api/orders", "GET api/orders/5", "DELETE api/orders/5"],
            "tables": ["orders", "order"],
            "columns": ["orders.total", "orders.totl", "invoices.id"],
            "relations": ["App\\Models\\Order::items", "Order::lines", "Customer::orders"]
        } }),
    );
    assert_eq!(
        status(&d, "App\\Models\\Order::customer")["status"],
        "exists"
    );
    assert_eq!(status(&d, "Order::paid")["status"], "exists");
    assert_eq!(
        status(&d, "App\\Models\\Order::where")["status"],
        "external"
    );
    assert_eq!(
        status(&d, "OrderController::archive")["status"],
        "not_found"
    );
    assert_eq!(
        status(&d, "App\\Services\\Legacy::run")["status"],
        "unresolved"
    );

    assert_eq!(status(&d, "now")["status"], "external");
    assert_eq!(status(&d, "array_map")["status"], "external");
    assert_eq!(status(&d, "formatMoney")["status"], "not_found");

    assert_eq!(status(&d, "orders.index")["status"], "exists");
    let missing_route = status(&d, "orders.archive");
    assert_eq!(missing_route["status"], "not_found");
    assert!(
        missing_route["suggestions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s == "orders.index")
    );
    assert_eq!(status(&d, "POST api/orders")["status"], "exists");
    assert_eq!(status(&d, "GET api/orders/5")["status"], "exists");
    // apiResource()->only(index, store, show): there is no destroy route.
    assert_eq!(status(&d, "DELETE api/orders/5")["status"], "not_found");

    assert_eq!(status(&d, "orders")["status"], "exists");
    let order = status(&d, "order");
    assert_eq!(order["status"], "not_found");
    assert_eq!(order["suggestions"][0], "orders");
    assert_eq!(status(&d, "orders.total")["status"], "exists");
    assert_eq!(status(&d, "orders.totl")["suggestions"][0], "orders.total");
    assert_eq!(status(&d, "invoices.id")["status"], "not_found");

    assert_eq!(status(&d, "App\\Models\\Order::items")["status"], "exists");
    assert_eq!(status(&d, "Order::lines")["status"], "not_found");
    assert_eq!(status(&d, "Customer::orders")["status"], "exists");
}

#[test]
fn php_snippet_mode() {
    let fx = setup();
    let d = verify(
        &fx,
        json!({ "php_snippet": r#"<?php
namespace App\Http\Controllers;

use App\Models\Order;
use App\Models\Ghost;
use Illuminate\Support\Facades\DB;

class ShipController extends Controller
{
    public function __invoke(Order $order)
    {
        $order->update(['status' => 'shipped']);
        DB::table('shipments')->insert(['order_id' => $order->id]);
        Ghost::boo();
        return to_route('orders.show', $order);
    }
}
"# }),
    );
    assert_eq!(status(&d, "App\\Models\\Order")["status"], "exists");
    assert_eq!(status(&d, "App\\Models\\Ghost")["status"], "not_found");
    assert_eq!(status(&d, "App\\Models\\Ghost::boo")["status"], "not_found");
    assert_eq!(
        status(&d, "App\\Http\\Controllers\\Controller")["status"],
        "exists"
    );
    assert_eq!(status(&d, "shipments")["status"], "not_found");
    assert_eq!(status(&d, "orders.show")["status"], "exists");
    // brief: existing items carry no file details
    assert!(status(&d, "App\\Models\\Order").get("file").is_none());
}

#[test]
fn verify_needs_input() {
    let fx = setup();
    let err = call(
        &fx.store,
        &fx.project,
        "verify",
        json!({}),
        Freshness::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("claims"));
}

#[test]
fn php_snippet_checks_columns_on_queries_and_raw_sql() {
    let fx = setup();
    let d = verify(
        &fx,
        json!({ "php_snippet": r#"<?php
use App\Models\Order;
use Illuminate\Support\Facades\DB;

$orders = Order::query()
    ->whereNotNull('statsu')
    ->where('total', '>', 10)
    ->orderByRaw('orders.totl desc, o.whatever')
    ->get();
DB::table('order_items')->where('sku', 'x')->pluck('quantty');
"#, "detail": "full" }),
    );
    let typo = status(&d, "Order.statsu");
    assert_eq!(typo["status"], "not_found");
    assert_eq!(typo["suggestions"][0], "orders.status");
    assert!(typo["detail"].as_str().unwrap().contains("table orders"));
    assert_eq!(status(&d, "Order.total")["status"], "exists");
    assert_eq!(status(&d, "orders.totl")["status"], "not_found");
    assert_eq!(status(&d, "order_items.sku")["status"], "exists");
    assert_eq!(
        status(&d, "order_items.quantty")["suggestions"][0],
        "order_items.quantity"
    );
    // `o` is an alias, not a table: not reported.
    assert!(
        d["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["item"] != "o.whatever")
    );
    assert_eq!(d["all_exist"], false);
}
