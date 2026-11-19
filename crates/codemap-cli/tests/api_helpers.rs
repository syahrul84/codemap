//! HTTP calls behind helpers: the functions of an API object are names to
//! find, a screen's calls are followed through the helpers it uses, and a
//! call whose URL is built at run time is listed as one whose endpoint is
//! unknown, not left out.

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
    let root = fixture("api-helpers/frontend");
    let project = codemap_mcp::Project {
        app: None,
        config: Config::load(&root).unwrap(),
        root,
        id: "front".into(),
        index_path: data.join("front").join("index.db"),
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
fn functions_of_an_object_or_class_are_symbols() {
    let data = tempfile::tempdir().unwrap();
    let (store, project) = open(data.path());
    let found = call(
        &store,
        &project,
        "search_symbol",
        json!({ "query": "familyList" }),
    );
    assert_eq!(found["found"], true, "{found}");
    let line = found["matches"][0].as_str().unwrap();
    assert!(
        line.starts_with(
            "src/modules/assets.ts#assetsApi.familyList  method  src/modules/assets.ts:31"
        ),
        "{line}"
    );
    // By the object's name, the way the code writes it; an arrow function
    // counts, and so does a class's method.
    for (name, line) in [
        ("assetsApi.save", 21),
        ("assetsApi.categories", 30),
        ("AssetLabels.title", 37),
    ] {
        let got = call(&store, &project, "get_symbol", json!({ "name": name }));
        assert_eq!(got["found"], true, "{name}: {got}");
        assert_eq!(got["kind"], "method", "{name}");
        assert_eq!(got["line"], line, "{name}");
    }
}

#[test]
fn urls_built_at_run_time_and_urls_that_are_a_choice() {
    let data = tempfile::tempdir().unwrap();
    let (store, project) = open(data.path());
    let out = call(
        &store,
        &project,
        "api_usage",
        json!({ "file": "modules/assets.ts", "detail": "full" }),
    );
    assert_eq!(
        out["summary"],
        json!({ "matched": 4, "unresolved": 1 }),
        "{out}"
    );
    let calls = out["calls"].as_array().unwrap();
    let at = |line: u32| {
        calls
            .iter()
            .find(|c| c["at"] == format!("src/modules/assets.ts:{line}"))
            .cloned()
            .unwrap_or_else(|| panic!("no call at line {line}: {out}"))
    };
    assert_eq!(at(19)["route"], "GET|HEAD api/assets");
    // `id === 'new' ? base : `${base}/${id}``: both URLs, and the method the
    // body sets on some paths (a spread, or a verb handed to a helper).
    for line in [22, 28] {
        let save = at(line);
        assert_eq!(save["call"], "POST|PATCH /assets", "{save}");
        assert_eq!(save["status"], "matched", "{save}");
        assert!(save.get("variants_without_route").is_none(), "{save}");
    }
    // A value inside a constant object.
    assert_eq!(at(30)["route"], "GET|HEAD api/lookups/categories");
    // Built by a function: the call is known, its endpoint is not.
    let dynamic = at(32);
    assert_eq!(dynamic["status"], "unresolved");
    assert_eq!(dynamic["call"], "GET `familyPath(family, id)`");
    assert!(
        dynamic["why"]
            .as_str()
            .unwrap()
            .starts_with("the URL is built at run time by `familyPath(family, id)`"),
        "{dynamic}"
    );
    // `get` on something that is not an HTTP client is no call.
    let cache = call(
        &store,
        &project,
        "api_usage",
        json!({ "file": "modules/cache.ts" }),
    );
    assert_eq!(cache["calls"], json!([]), "{cache}");
}

#[test]
fn a_files_calls_are_followed_through_its_helpers() {
    let data = tempfile::tempdir().unwrap();
    let (store, project) = open(data.path());
    // The logic file calls two functions of an API object in another file.
    let logic = call(
        &store,
        &project,
        "api_usage",
        json!({ "file": "logic/assetHistory.ts" }),
    );
    let calls = logic["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 2, "{logic}");
    assert_eq!(calls[0]["at"], "src/modules/assets.ts:30");
    assert_eq!(calls[0]["status"], "matched");
    assert_eq!(
        calls[0]["through"],
        "assetsApi.categories (src/pages/logic/assetHistory.ts:5)"
    );
    assert_eq!(calls[1]["at"], "src/modules/assets.ts:32");
    assert_eq!(calls[1]["status"], "unresolved");
    assert_eq!(
        calls[1]["through"],
        "assetsApi.familyList (src/pages/logic/assetHistory.ts:4)"
    );
    // The page reaches the same calls one step further away; the calls of
    // the object's other functions are not its own.
    let page = call(
        &store,
        &project,
        "api_usage",
        json!({ "file": "AssetHistoryPage.vue" }),
    );
    let calls = page["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 2, "{page}");
    for c in calls {
        assert_eq!(
            c["through"], "useAssetHistory (src/pages/AssetHistoryPage.vue:4)",
            "{c}"
        );
    }
    assert_eq!(
        page["summary"],
        json!({ "matched": 1, "unresolved": 1 }),
        "{page}"
    );
}
