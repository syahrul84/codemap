#![allow(dead_code)]

use std::path::{Path, PathBuf};

use pidx_core::{Config, ScanOptions, Store};
use pidx_php::RefreshReport;

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
        .canonicalize()
        .unwrap()
}

pub fn indexed(root: &Path) -> (Store, RefreshReport) {
    let mut store = Store::open_in_memory().unwrap();
    let report =
        pidx_php::refresh(&mut store, root, &Config::default(), ScanOptions::default()).unwrap();
    (store, report)
}
