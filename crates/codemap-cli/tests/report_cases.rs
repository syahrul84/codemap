//! Every folder in `fixtures/report-cases/`: a wrong answer reported once,
//! with its made-up example and the check of the right answer. Each must
//! still pass, so a fixed bug cannot come back.

use std::path::Path;

use codemap_core::{Config, ScanOptions, Store};
use serde_json::Value;

#[test]
fn every_report_case_still_passes() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/report-cases");
    let mut cases: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter(|e| e.path().join("case.json").is_file())
        .map(|e| e.path())
        .collect();
    cases.sort();
    assert!(!cases.is_empty(), "no report cases in {}", dir.display());
    let mut failed = Vec::new();
    for case_dir in &cases {
        let case: Value =
            serde_json::from_str(&std::fs::read_to_string(case_dir.join("case.json")).unwrap())
                .unwrap();
        let root = case_dir.join("project").canonicalize().unwrap();
        let data = tempfile::tempdir().unwrap();
        let project = codemap_mcp::Project {
            app: None,
            config: Config::load(&root).unwrap(),
            root: root.clone(),
            id: "case".into(),
            index_path: data.path().join("case").join("index.db"),
        };
        let mut store = Store::open(&project.index_path).unwrap();
        codemap_mcp::refresh(&mut store, &root, &project.config, ScanOptions::default()).unwrap();
        let ok = codemap_mcp::reports::check(&[(&store, &project)], &case["expect"], None);
        if ok != Some(true) {
            failed.push(format!(
                "{}: {} (was wrong: {})",
                case_dir.file_name().unwrap().to_string_lossy(),
                case["right"],
                case["wrong"]
            ));
        }
    }
    assert!(
        failed.is_empty(),
        "report cases failing again:\n{}",
        failed.join("\n")
    );
}
