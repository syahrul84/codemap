//! A Next.js (app router) + Supabase project: pages and layouts, route
//! handlers as endpoints, React components, table use and the SQL schema.

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
    let root = fixture("next-app");
    let project = codemap_mcp::Project {
        app: None,
        config: Config::load(&root).unwrap(),
        index_path: data.join("next").join("index.db"),
        root,
        id: "next".into(),
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
fn next_app_router_with_supabase() {
    let data = tempfile::tempdir().unwrap();
    let (store, project) = open(data.path());

    let o = call(&store, &project, "project_overview", json!({}));
    assert_eq!(o["frontend"]["framework"], "Next.js ^16.2.0");

    // Screens from the file system: groups dropped, params as :name, nearest layout.
    let pages = call(&store, &project, "list_pages", json!({ "detail": "full" }));
    let paths: Vec<&str> = pages["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["/:lang", "/:lang/rooms/:slug"]);
    assert_eq!(pages["pages"][1]["layout"], "app/(site)/[lang]/layout.tsx");

    // Route handlers are endpoints; the page's fetch matches one.
    let routes = call(&store, &project, "list_routes", json!({}));
    assert_eq!(routes["routes"][0]["uri"], "api/availability/{slug}");
    assert_eq!(routes["routes"][0]["methods"], "GET|HEAD");
    let api = call(
        &store,
        &project,
        "api_usage",
        json!({ "file": "app/(site)/[lang]/rooms/[slug]/page.tsx", "detail": "full" }),
    );
    assert_eq!(api["calls"][0]["route"], "GET|HEAD api/availability/{slug}");

    // React components: props from the type, children, who renders them.
    let card = call(
        &store,
        &project,
        "get_component",
        json!({ "name": "RoomCard" }),
    );
    assert_eq!(card["file"], "components/room-card.tsx");
    assert_eq!(
        card["props"],
        json!(["room: Room (required)", "compact: boolean"])
    );
    assert_eq!(card["used_by"], json!(["app/(site)/[lang]/page.tsx:6"]));

    // Supabase: tables used in code, schema from the SQL migrations.
    let rooms = call(
        &store,
        &project,
        "find_usages",
        json!({ "target": "rooms" }),
    );
    assert_eq!(rooms["resolved_as"], "table rooms");
    assert_eq!(rooms["usages"], json!(["lib/data.ts:8  select"]));
    let schema = call(
        &store,
        &project,
        "get_schema",
        json!({ "table": "bookings" }),
    );
    let cols: Vec<&str> = schema["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(cols, ["id", "room_id", "created_at"]);

    // Translations kept per text (`checkIn: { en, ms }`).
    assert_eq!(o["i18n"][0]["locales"], json!({ "en": 6, "ms": 6 }));
    assert_eq!(o["i18n"][0]["used_but_undefined"], 0);
}
