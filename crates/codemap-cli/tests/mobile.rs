//! Swift, Android and Rust fixtures indexed end to end: screens, views and
//! composables, usages and translations.

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
        id: "m".into(),
        index_path: data.join("m").join("index.db"),
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
fn swift_app() {
    let data = tempfile::tempdir().unwrap();
    let (store, p) = open("swift-app", data.path());
    let o = call(&store, &p, "project_overview", json!({}));
    assert_eq!(o["swift"]["apps"][0]["name"], "DemoApp", "{o}");
    assert_eq!(o["swift"]["views"], 7, "{o}");
    let pages = call(&store, &p, "list_pages", json!({}))["pages"].to_string();
    for want in [
        "window [RootView]",
        "settings [SettingsView]",
        "tab [ItemListView]",
        "navigation [ItemDetailView]",
        "sheet [AddItemView]",
    ] {
        assert!(pages.contains(want), "{want} missing in {pages}");
    }
    let v = call(&store, &p, "get_component", json!({"name": "ItemListView"}));
    assert_eq!(v["kind"], "swiftui_view", "{v}");
    assert_eq!(v["state"][0]["name"], "items", "{v}");
    assert!(v["renders"].to_string().contains("ItemDetailView"), "{v}");
    assert!(v["opened_as"].to_string().contains("\"tab\""), "{v}");
    let comp = call(
        &store,
        &p,
        "verify",
        json!({"claims": {"components": ["ItemRow", "NopeView"]}}),
    );
    assert_eq!(comp["results"][0]["status"], "exists", "{comp}");
    assert_eq!(comp["results"][1]["status"], "not_found", "{comp}");
    let unused = call(&store, &p, "get_component", json!({"name": "UnusedView"}));
    assert!(
        unused["hint"].as_str().unwrap().contains("unused"),
        "{unused}"
    );
    let u = call(&store, &p, "find_usages", json!({"target": "Item"}));
    assert_eq!(u["resolved_as"], "struct Item", "{u}");
    assert!(u["total"].as_u64().unwrap() >= 4, "{u}");
    let m = call(
        &store,
        &p,
        "find_usages",
        json!({"target": "ItemStore::load"}),
    );
    assert_eq!(m["resolved_as"], "method ItemStore.load", "{m}");
    let ver = call(
        &store,
        &p,
        "verify",
        json!({"claims": {"classes": ["ItemRow", "NopeView"], "methods": ["FileItemStore::save"], "i18n_keys": ["item.detail", "nope.key"]}}),
    );
    let statuses: Vec<&str> = ver["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["status"].as_str().unwrap())
        .collect();
    assert_eq!(
        statuses,
        ["exists", "not_found", "exists", "exists", "not_found"],
        "{ver}"
    );
    // Settings is used in code but only `en` has it: missing in `ms`.
    let i18n = &o["i18n"][0];
    assert_eq!(i18n["source"], "swift", "{o}");
    assert_eq!(i18n["missing_per_locale"]["ms"], 1, "{o}");
}

#[test]
fn android_app() {
    let data = tempfile::tempdir().unwrap();
    let (store, p) = open("android-app", data.path());
    let o = call(&store, &p, "project_overview", json!({}));
    assert_eq!(o["android"]["manifest"][0]["launcher"], true, "{o}");
    assert_eq!(o["android"]["composables"], 8, "{o}");
    let pages = call(&store, &p, "list_pages", json!({}))["pages"].to_string();
    for want in [
        "activity [MainActivity]",
        "receiver [AlertReceiver]",
        "switch [DetailScreen]",
        "switch [ItemsScreen]",
        "sheet [FilterSheet]",
    ] {
        assert!(pages.contains(want), "{want} missing in {pages}");
    }
    let c = call(&store, &p, "get_component", json!({"name": "Root"}));
    assert_eq!(c["kind"], "composable", "{c}");
    assert_eq!(c["state"][0]["via"], "rememberSaveable", "{c}");
    assert!(c["used_in"].to_string().contains("MainActivity"), "{c}");
    let comp = call(
        &store,
        &p,
        "verify",
        json!({"claims": {"components": ["FilterSheet", "NopeScreen"]}}),
    );
    assert_eq!(comp["results"][0]["status"], "exists", "{comp}");
    assert_eq!(comp["results"][1]["status"], "not_found", "{comp}");
    let items = call(&store, &p, "get_component", json!({"name": "ItemsScreen"}));
    assert_eq!(items["params"][1]["callback"], true, "{items}");
    assert!(items["renders"].to_string().contains("ItemRow"), "{items}");
    let u = call(&store, &p, "find_usages", json!({"target": "AppState"}));
    assert_eq!(u["resolved_as"], "class com.example.demo.AppState", "{u}");
    assert!(u["tests"].to_string().contains("AppStateTest.kt"), "{u}");
    let k = call(&store, &p, "find_usages", json!({"target": "items_title"}));
    assert_eq!(k["resolved_as"], "translation key items_title", "{k}");
    let i18n = &o["i18n"][0];
    assert_eq!(i18n["base_locale"], "default", "{o}");
    // `unused_text` has no `ms`; `internal_id` is translatable="false" so it is not missing.
    assert_eq!(i18n["missing_per_locale"]["ms"], 1, "{o}");
}

#[test]
fn rust_crate() {
    let data = tempfile::tempdir().unwrap();
    let (store, p) = open("rust-crate", data.path());
    let o = call(&store, &p, "project_overview", json!({}));
    assert_eq!(o["rust"]["crates"][0]["name"], "demo-tool", "{o}");
    assert_eq!(o["rust"]["crates"][0]["bins"][0], "demo-tool", "{o}");
    let pages = call(&store, &p, "list_pages", json!({}))["pages"].to_string();
    assert!(
        pages.contains("binary [demo-tool]")
            && pages.contains("command [list]")
            && pages.contains("command [add]"),
        "{pages}"
    );
    let s = call(&store, &p, "get_symbol", json!({"name": "Store"}));
    assert_eq!(s["fqn"], "demo_tool::store::Store", "{s}");
    assert!(s["members"].as_str().unwrap().contains("add()"), "{s}");
    let u = call(&store, &p, "find_usages", json!({"target": "MAX_ITEMS"}));
    assert_eq!(
        u["usages"],
        json!(["src/store/mod.rs:1  call", "src/store/mod.rs:12  call"]),
        "{u}"
    );
    let l = call(&store, &p, "find_usages", json!({"target": "label"}));
    assert_eq!(
        l["usages"],
        json!(["src/main.rs:1  call", "src/main.rs:13  call"]),
        "{l}"
    );
    assert_eq!(l["tests"], json!(["src/lib.rs:29  call"]), "{l}");
    // `fancy::label(..)` is another crate's; `item.label()` a method.
    assert!(!l.to_string().contains("main.rs:24  call"), "{l}");
    let r = call(&store, &p, "find_usages", json!({"target": "store::Store"}));
    assert_eq!(r["resolved_as"], "struct demo_tool::store::Store", "{r}");
    let n = call(
        &store,
        &p,
        "search_symbol",
        json!({"query": "load", "namespace_prefix": "demo_tool::store"}),
    );
    assert_eq!(n["found"], true, "{n}");
    assert_eq!(
        n["matches"][0]["fqn"], "demo_tool::store::Store::load",
        "{n}"
    );
    let ver = call(
        &store,
        &p,
        "verify",
        json!({"claims": {"classes": ["Store", "HashMap"], "methods": ["Store::add", "Store::nope"], "functions": ["label"]}}),
    );
    let statuses: Vec<&str> = ver["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["status"].as_str().unwrap())
        .collect();
    assert_eq!(
        statuses,
        ["exists", "not_found", "exists", "not_found", "exists"],
        "{ver}"
    );
}

/// Change impact for Swift, Kotlin and Rust files: who uses the changed
/// file's types, and the right test commands.
#[test]
fn change_impact_outside_php_and_js() {
    let data = tempfile::tempdir().unwrap();
    let (store, p) = open("rust-crate", data.path());
    let ch = call(
        &store,
        &p,
        "find_usages",
        json!({"changed_files": ["src/lib.rs"]}),
    );
    // Its own tests, and the integration test that uses the library.
    assert_eq!(
        ch["run"],
        json!(["cargo test -p demo-tool --lib --test store"]),
        "{ch}"
    );
    assert!(
        ch["changed"][0]["used_in"]
            .to_string()
            .contains("src/store/mod.rs"),
        "{ch}"
    );
    assert!(
        !ch["changed"][0]["is"]
            .as_str()
            .unwrap()
            .contains("enum_case"),
        "{ch}"
    );
    assert!(
        !ch.to_string().contains("of the project's 1 test files"),
        "{ch}"
    );
    // main.rs is a binary: its in-file tests run with --bins.
    let ch = call(
        &store,
        &p,
        "find_usages",
        json!({"changed_files": ["src/main.rs"]}),
    );
    assert_eq!(ch["run"], json!(["cargo test -p demo-tool --bins"]), "{ch}");

    let data = tempfile::tempdir().unwrap();
    let (store, p) = open("android-app", data.path());
    let ch = call(
        &store,
        &p,
        "find_usages",
        json!({"changed_files": ["app/src/main/java/com/example/demo/AppState.kt"]}),
    );
    assert_eq!(
        ch["run"],
        json!([
            "gradle :app:test --tests com.example.demo.AppStateTest",
            "gradle :app:connectedAndroidTest -Pandroid.testInstrumentationRunnerArguments.class=com.example.demo.AppStateDeviceTest"
        ]),
        "{ch}"
    );
    assert!(
        ch["changed"][0]["used_in"]
            .to_string()
            .contains("ui/Root.kt"),
        "{ch}"
    );

    let data = tempfile::tempdir().unwrap();
    let (store, p) = open("swift-app", data.path());
    let ch = call(
        &store,
        &p,
        "find_usages",
        json!({"changed_files": ["Sources/Demo/Model.swift"]}),
    );
    assert!(
        ch["changed"][0]["used_in"]
            .to_string()
            .contains("RootView.swift"),
        "{ch}"
    );
}

/// The shared view model: one table for every framework's views.
#[test]
fn shared_view_model() {
    for (fixture, framework, view, shown_by) in [
        ("swift-app", "swiftui", "ItemRow", "RootView.swift"),
        ("android-app", "compose", "ItemRow", "ui/Root.kt"),
        ("vue3-vite", "vue", "OrderCard", "OrdersView.vue"),
    ] {
        let data = tempfile::tempdir().unwrap();
        let (store, _) = open(fixture, data.path());
        let found = codemap_core::views::find(store.conn(), view).unwrap();
        assert_eq!(found.len(), 1, "{fixture}: {found:?}");
        assert_eq!(found[0].framework, framework);
        let from = codemap_core::views::shown_from(store.conn(), &found[0].path).unwrap();
        assert!(
            from.iter().any(|(p, _, _)| p.ends_with(shown_by)) || found[0].path.ends_with(shown_by),
            "{fixture}: {from:?}"
        );
    }
}

/// The release check reports SwiftUI views and composables nothing shows.
#[test]
fn unused_views_in_the_release_check() {
    for (fixture, unused, used) in [
        (
            "swift-app",
            "SwiftUI view UnusedView is not shown anywhere",
            "ItemRow is not shown",
        ),
        (
            "android-app",
            "composable UnusedScreen is not shown anywhere",
            "ItemRow is not shown",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let root = dir.path().join("p");
        copy_dir(&fixture_path(fixture), &root);
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .args([
                        "-c",
                        "user.name=t",
                        "-c",
                        "user.email=t@t",
                        "-c",
                        "init.defaultBranch=main"
                    ])
                    .args(args)
                    .current_dir(&root)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        };
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "base"]);
        git(&["checkout", "-q", "-b", "feature"]);
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "all"]);
        let project = codemap_mcp::Project {
            app: None,
            config: Config::load(&root).unwrap(),
            root: root.clone(),
            id: "r".into(),
            index_path: data.path().join("r").join("index.db"),
        };
        let mut store = Store::open(&project.index_path).unwrap();
        codemap_mcp::refresh(&mut store, &root, &project.config, ScanOptions::default()).unwrap();
        let v = call(
            &store,
            &project,
            "verify",
            json!({"release_check": true, "base": "main"}),
        );
        let text = v.to_string();
        assert!(text.contains(unused), "{fixture}: {v}");
        assert!(!text.contains(used), "{fixture}: {v}");
    }
}

fn fixture_path(rel: &str) -> PathBuf {
    fixture(rel)
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(e.file_name());
        if e.path().is_dir() {
            copy_dir(&e.path(), &target);
        } else {
            std::fs::copy(e.path(), target).unwrap();
        }
    }
}

/// With an `index.scip`, "possible" uses by name are settled: the one the
/// index lists is kept, the unrelated `.add()` is dropped.
#[test]
fn index_scip_settles_possible_uses() {
    let dir = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let root = dir.path().join("p");
    copy_dir(&fixture_path("rust-crate"), &root);
    std::fs::write(
        root.join("src/other.rs"),
        "pub fn unrelated(v: &mut Vec<u32>) {\n    v.push(1);\n    let mut s = std::collections::HashSet::new();\n    s.add_one();\n    s.add(1);\n}\n",
    )
    .unwrap();
    let project = codemap_mcp::Project {
        app: None,
        config: Config::load(&root).unwrap(),
        root: root.clone(),
        id: "s".into(),
        index_path: data.path().join("s").join("index.db"),
    };
    let mut store = Store::open(&project.index_path).unwrap();
    codemap_mcp::refresh(&mut store, &root, &project.config, ScanOptions::default()).unwrap();

    // Without the index: both `.add(` calls outside store/mod.rs are possible uses.
    let before = call(
        &store,
        &project,
        "find_usages",
        json!({"target": "Store::add"}),
    );
    let possible = before["possible"].to_string();
    assert!(
        possible.contains("tests/store.rs:6") && possible.contains("src/other.rs:5"),
        "{before}"
    );

    // The index (as rust-analyzer would write it) knows which one it is.
    let occ = |line: i32, symbol: &str, def: bool| {
        let mut o = scip::types::Occurrence::new();
        o.range = vec![line, 4, 7];
        o.symbol = symbol.to_owned();
        o.symbol_roles = if def {
            scip::types::SymbolRole::Definition as i32
        } else {
            0
        };
        o
    };
    let add = "rust-analyzer cargo demo-tool 0.1.0 store/Store#add().";
    let mut store_doc = scip::types::Document::new();
    store_doc.relative_path = "src/store/mod.rs".into();
    store_doc.occurrences = vec![occ(10, add, true)];
    let mut test_doc = scip::types::Document::new();
    test_doc.relative_path = "tests/store.rs".into();
    test_doc.occurrences = vec![occ(5, add, false)];
    let mut index = scip::types::Index::new();
    index.documents = vec![store_doc, test_doc];
    scip::write_message_to_file(root.join("index.scip"), index).unwrap();

    let after = call(
        &store,
        &project,
        "find_usages",
        json!({"target": "Store::add"}),
    );
    assert_eq!(
        after["tests"],
        json!(["tests/store.rs:6  member"]),
        "{after}"
    );
    assert!(
        after.get("possible").is_none() || after["possible"] == json!([]),
        "{after}"
    );
    assert!(
        after["notes"]
            .to_string()
            .contains("1 of 2 possible uses by name confirmed"),
        "{after}"
    );
}
