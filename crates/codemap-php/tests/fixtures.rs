//! Snapshot of every symbol extracted from each fixture project.

mod common;

use codemap_core::symbols;

fn snapshot(name: &str) {
    let (store, report) = common::indexed(&common::fixture(name));
    assert_eq!(report.php.failed, 0);
    let warnings = store.warnings().unwrap();
    assert!(warnings.is_empty(), "{name}: {warnings:#?}");
    let all = symbols::all(store.conn()).unwrap();
    insta::assert_json_snapshot!(name, all);
}

#[test]
fn laravel10_legacy() {
    snapshot("laravel10-legacy");
}

#[test]
fn laravel11_slim() {
    snapshot("laravel11-slim");
}

#[test]
fn laravel11_upgraded() {
    snapshot("laravel11-upgraded");
}

#[test]
fn schema_dump() {
    snapshot("schema-dump");
}
