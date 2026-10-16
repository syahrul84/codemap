#![allow(dead_code)]

use std::path::{Path, PathBuf};

use codemap_core::{Config, ScanOptions, Store};
use codemap_laravel::RefreshReport;
use codemap_laravel::routes::{self, RouteDef};

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
        .canonicalize()
        .unwrap()
}

pub fn indexed_with(root: &Path, config: &Config) -> (Store, RefreshReport) {
    let mut store = Store::open_in_memory().unwrap();
    let report =
        codemap_laravel::refresh(&mut store, root, config, ScanOptions::default()).unwrap();
    (store, report)
}

pub fn indexed(root: &Path) -> (Store, RefreshReport) {
    indexed_with(root, &Config::default())
}

pub fn routes(store: &Store) -> Vec<RouteDef> {
    routes::all(store.conn()).unwrap()
}

/// One line per route: `METHODS uri name action [middleware] file:line source`.
pub fn table(store: &Store) -> String {
    routes(store)
        .iter()
        .map(RouteDef::table_line)
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}
