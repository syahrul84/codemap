//! Re-indexing after edits: only changed files are re-parsed, the registry
//! pass runs only when needed, and removed or renamed components show up.

use std::path::Path;

use pidx_core::envelope::Detail;
use pidx_core::{Config, ScanOptions, Store};
use pidx_vue::VueReport;
use serde_json::json;

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

fn refresh(store: &mut Store, root: &Path) -> VueReport {
    let scan =
        pidx_core::scan::scan(store, root, &Config::default(), ScanOptions::default()).unwrap();
    pidx_vue::refresh(store, root, &scan, false).unwrap()
}

/// Rewrites a file with a later mtime, so the scanner sees the change.
fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::write(&p, text).unwrap();
    let later = filetime::FileTime::from_unix_time(4_000_000_000, 0);
    filetime::set_file_mtime(&p, later).unwrap();
}

#[test]
fn edits_deletes_and_detection_changes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/vue3-vite");
    copy_dir(&fixture, root);
    let mut store = Store::open_in_memory().unwrap();

    let first = refresh(&mut store, root);
    assert!(first.detected && first.registry_ran);
    assert_eq!(first.js.parsed, 11);

    // Nothing changed: no parsing, no registry pass.
    let again = refresh(&mut store, root);
    assert_eq!((again.js.parsed, again.registry_ran), (0, false));
    assert_eq!(again.components, first.components);

    // A new prop shows up after one re-parse.
    let card = std::fs::read_to_string(root.join("src/components/OrderCard.vue")).unwrap();
    write(
        root,
        "src/components/OrderCard.vue",
        &card.replace(
            "compact?: boolean",
            "compact?: boolean\n  highlight?: boolean",
        ),
    );
    let r = refresh(&mut store, root);
    assert_eq!((r.js.parsed, r.registry_ran), (1, true));
    let c =
        pidx_vue::component::get_component(&store, "OrderCard", Detail::Brief, Some(3)).unwrap();
    assert!(
        c["props"]
            .as_array()
            .unwrap()
            .contains(&json!("highlight: boolean")),
        "{c}"
    );

    // Deleting the component: its user now points at a missing import.
    std::fs::remove_file(root.join("src/components/OrderCard.vue")).unwrap();
    let r = refresh(&mut store, root);
    assert!(r.registry_ran);
    let (via, note): (String, Option<String>) = store
        .conn()
        .query_row(
            "SELECT via, package FROM component_uses WHERE from_path = 'src/views/OrdersView.vue' AND tag = 'OrderCard'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(via, "unresolved");
    assert_eq!(
        note.as_deref(),
        Some("import not found: src/components/OrderCard.vue")
    );
    let gone =
        pidx_vue::component::get_component(&store, "OrderCard", Detail::Brief, Some(3)).unwrap();
    assert_eq!(gone["found"], false);

    // No longer a Vue project: JS facts are dropped.
    write(
        root,
        "package.json",
        r#"{ "name": "plain", "dependencies": {} }"#,
    );
    let r = refresh(&mut store, root);
    assert!(!r.detected);
    let left: i64 = store
        .conn()
        .query_row("SELECT COUNT(*) FROM js_modules", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 0);
    let syms: i64 = store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM symbols WHERE kind IN ('component', 'composable', 'store')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(syms, 0);
}
