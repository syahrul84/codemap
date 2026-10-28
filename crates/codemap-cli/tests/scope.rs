//! What was searched: "not found" names the folders with code the scan
//! skips, and the app's own code ranks above test and fixture copies.

use codemap_core::{Config, ScanOptions, Store};
use serde_json::{Value, json};

fn project(
    files: &[(&str, &str)],
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    Store,
    codemap_mcp::Project,
) {
    let root = tempfile::tempdir().unwrap();
    for (path, text) in files {
        let p = root.path().join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    let data = tempfile::tempdir().unwrap();
    let path = root.path().canonicalize().unwrap();
    let project = codemap_mcp::Project {
        app: None,
        config: Config::load(&path).unwrap(),
        root: path,
        id: "s".into(),
        index_path: data.path().join("s").join("index.db"),
    };
    let mut store = Store::open(&project.index_path).unwrap();
    codemap_mcp::refresh(
        &mut store,
        &project.root,
        &project.config,
        ScanOptions::default(),
    )
    .unwrap();
    (root, data, store, project)
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

const FILES: &[(&str, &str)] = &[
    (
        "composer.json",
        r#"{"require": {"laravel/framework": "^11.0"}}"#,
    ),
    (
        "app/Services/Cart.php",
        "<?php\nnamespace App\\Services;\n\nclass Cart {}\n",
    ),
    (
        "tests/Fakes/Cart.php",
        "<?php\nnamespace Tests\\Fakes;\n\nclass Cart {}\n",
    ),
    (
        "modules/Billing/Invoice.php",
        "<?php\nnamespace Modules\\Billing;\n\nclass Invoice {}\n",
    ),
    ("docs/notes.json", "{}"),
    ("package.json", r#"{"dependencies": {"nuxt": "^3.0.0"}}"#),
    (
        "server/utils/price.ts",
        "export function formatPrice(cents: number) { return cents / 100 }\n",
    ),
];

#[test]
fn not_found_names_the_folders_the_scan_skips() {
    let (_root, _data, store, p) = project(FILES);
    let s = call(&store, &p, "search_symbol", json!({"query": "Invoice"}));
    assert_eq!(s["found"], false, "{s}");
    // `docs/` holds only data: not listed.
    assert_eq!(s["not_scanned"], json!(["modules/"]), "{s}");
    let v = call(
        &store,
        &p,
        "verify",
        json!({"claims": {"classes": ["Modules\\Billing\\Invoice"]}}),
    );
    assert_eq!(v["not_scanned"], json!(["modules/"]), "{v}");
    // Found: nothing to explain.
    let c = call(&store, &p, "search_symbol", json!({"query": "Cart"}));
    assert!(c.get("not_scanned").is_none(), "{c}");
    // Nuxt's `server/` is read by default.
    let f = call(&store, &p, "search_symbol", json!({"query": "formatPrice"}));
    assert_eq!(f["found"], true, "{f}");
}

#[test]
fn the_apps_own_code_ranks_above_test_copies() {
    let (_root, _data, store, p) = project(FILES);
    let c = call(&store, &p, "search_symbol", json!({"query": "Cart"}));
    assert_eq!(c["matches"][0]["fqn"], "App\\Services\\Cart", "{c}");
    assert_eq!(c["matches"][1]["fqn"], "Tests\\Fakes\\Cart", "{c}");
}

/// A workspace folder with a Laravel backend and an iOS app: both are apps.
#[test]
fn workspace_finds_mobile_and_rust_apps() {
    let root = tempfile::tempdir().unwrap();
    let put = |path: &str, text: &str| {
        let p = root.path().join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    put(
        "backend/composer.json",
        r#"{"require": {"laravel/framework": "^11.0"}}"#,
    );
    put("backend/artisan", "<?php\n");
    put("ios/Package.swift", "// swift-tools-version:5.9\n");
    put(
        "android/settings.gradle.kts",
        "rootProject.name = \"app\"\n",
    );
    put("tool/Cargo.toml", "[package]\nname = \"tool\"\n");
    put("docs/readme.md", "# docs\n");
    let apps: Vec<String> = codemap_mcp::workspace::discover(root.path(), &Config::default())
        .unwrap()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(apps, ["android", "backend", "ios", "tool"]);

    // Mobile apps alone: one project, as before.
    let mobile = tempfile::tempdir().unwrap();
    for (path, text) in [
        ("ios/Package.swift", "// swift-tools-version:5.9\n"),
        (
            "android/settings.gradle.kts",
            "rootProject.name = \"app\"\n",
        ),
    ] {
        let p = mobile.path().join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    assert!(codemap_mcp::workspace::discover(mobile.path(), &Config::default()).is_none());
}
