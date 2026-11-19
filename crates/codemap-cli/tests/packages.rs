//! The packages a project installs: version, which code uses each one,
//! where config and deployment files name it, and which nothing references.

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
    let root = fixture("packages");
    let project = codemap_mcp::Project {
        app: None,
        config: Config::load(&root).unwrap(),
        root,
        id: "packages".into(),
        index_path: data.join("packages").join("index.db"),
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

fn packages(store: &Store, project: &codemap_mcp::Project, args: Value) -> Value {
    codemap_mcp::call(
        store,
        project,
        "project_overview",
        args,
        codemap_mcp::Freshness::default(),
    )
    .unwrap()
    .data
}

#[test]
fn every_direct_dependency_with_what_uses_it() {
    let data = tempfile::tempdir().unwrap();
    let (store, project) = open(data.path());
    let all = packages(&store, &project, json!({ "packages": "all" }));
    assert_eq!(all["found"], true, "{all}");
    let lines: Vec<&str> = all["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect();
    let line = |name: &str| -> &str {
        lines
            .iter()
            .find(|l| l.starts_with(&format!("{name}  ")))
            .copied()
            .unwrap_or_else(|| panic!("{name} is not listed: {lines:#?}"))
    };
    // Platform requirements (`php`, `ext-json`) are not packages.
    assert!(
        lines
            .iter()
            .all(|l| !l.starts_with("php") && !l.starts_with("ext-"))
    );
    // By its namespace, and through the alias Laravel registers for it.
    assert_eq!(
        line("papyrus/pdf"),
        "papyrus/pdf  v2.1.4  composer  → used in 3 files (app/Services (2), tests/Unit)"
    );
    // A package that only lists its class files: the vendor's namespace.
    assert_eq!(
        line("sheetgen/sheetgen"),
        "sheetgen/sheetgen  1.5.17  composer  → used in 1 file (app/Services)"
    );
    assert_eq!(
        line("beacon/laravel-beacon"),
        "beacon/laravel-beacon  v3.0.1  composer  → no import in code; its service provider is discovered by Laravel"
    );
    // Named bare in a config file, as packages called like their vendor are.
    assert_eq!(
        line("quill/quill"),
        "quill/quill  v4.0.0  composer  → no import in code; named in config/queue.php"
    );
    assert_eq!(
        line("lantern/spare"),
        "lantern/spare  1.0.0  composer  → nothing references it"
    );
    // Its program, in a manifest's scripts.
    assert_eq!(
        line("swiftrun/swiftrun"),
        "swiftrun/swiftrun  v7.2.0  composer dev  → no import in code; named in composer.json"
    );
    assert_eq!(
        line("axios"),
        "axios  1.7.7  npm  → used in 1 file (resources/js)"
    );
    // Its program, in a process manager's file.
    assert_eq!(
        line("echo-relay"),
        "echo-relay  1.6.3  npm  → no import in code; named in docker/supervisor.conf"
    );
    assert_eq!(
        line("left-over"),
        "left-over  0.3.0  npm  → nothing references it"
    );
    assert_eq!(
        line("@types/node"),
        "@types/node  22.7.5  npm dev  → types for node"
    );
    assert_eq!(
        line("vite"),
        "vite  5.4.8  npm dev  → no import in code; named in package.json"
    );
    assert_eq!(
        all["summary"],
        json!({ "composer": 7, "not_referenced": 2, "npm": 6 }),
        "{all}"
    );

    let unused = packages(&store, &project, json!({ "packages": "unused" }));
    assert_eq!(
        unused["packages"],
        json!([
            "lantern/spare  1.0.0  composer  → nothing references it",
            "left-over  0.3.0  npm  → nothing references it"
        ]),
        "{unused}"
    );
}

#[test]
fn one_package_in_full() {
    let data = tempfile::tempdir().unwrap();
    let (store, project) = open(data.path());
    let pdf = packages(&store, &project, json!({ "package": "papyrus/pdf" }));
    assert_eq!(pdf["installed"], "v2.1.4");
    assert_eq!(pdf["declared"], "^2.1 (composer.json require)");
    assert_eq!(
        pdf["used_by"],
        json!([
            "app/Services/InvoicePrinter.php:9",
            "app/Services/ReportWriter.php:11"
        ])
    );
    assert_eq!(pdf["tests"], json!(["tests/Unit/ReportWriterTest.php:12"]));
    assert_eq!(pdf["namespaces"], json!(["Papyrus\\Pdf\\"]));

    let relay = packages(&store, &project, json!({ "package": "echo-relay" }));
    assert_eq!(
        relay["status"],
        "no import in code; named in docker/supervisor.conf"
    );
    assert_eq!(
        relay["wired_in"],
        json!(["docker/supervisor.conf:2  command=node_modules/.bin/echo-relay start --port 6001"])
    );
    assert_eq!(relay["programs"], json!(["echo-relay"]));

    // Installed only because another package needs it.
    let fonts = packages(&store, &project, json!({ "package": "papyrus/fonts" }));
    assert_eq!(fonts["installed"], "1.0.2");
    assert_eq!(fonts["required_by"], json!(["papyrus/pdf"]));
    assert!(
        fonts["declared"]
            .as_str()
            .unwrap()
            .starts_with("not in the manifest"),
        "{fonts}"
    );
    let redirects = packages(&store, &project, json!({ "package": "follow-redirects" }));
    assert_eq!(redirects["installed"], "1.15.9");
    assert_eq!(redirects["required_by"], json!(["axios"]));

    // Part of a name lists the matches; an unknown one says so.
    let part = packages(&store, &project, json!({ "package": "papyrus" }));
    assert_eq!(part["total"], 1, "{part}");
    let none = packages(&store, &project, json!({ "package": "socket.io" }));
    assert_eq!(none["found"], false);
    assert!(
        none["hint"].as_str().unwrap().contains("socket.io"),
        "{none}"
    );
}
