//! Incremental PHP extraction: only new/changed files (or files from an older
//! extractor version) are parsed.

mod common;

use std::path::Path;
use std::time::Instant;

use codemap_core::symbols::{self, SearchInput};
use codemap_core::{Config, ScanOptions, Store};
use filetime::{FileTime, set_file_mtime};

const T0: i64 = 1_760_000_000;

fn write(root: &Path, rel: &str, content: &str, mtime: i64) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, content).unwrap();
    set_file_mtime(&path, FileTime::from_unix_time(mtime, 0)).unwrap();
}

fn refresh(store: &mut Store, root: &Path) -> codemap_php::RefreshReport {
    codemap_php::refresh(store, root, &Config::default(), ScanOptions::default()).unwrap()
}

fn fqns(store: &Store) -> Vec<String> {
    symbols::all(store.conn())
        .unwrap()
        .into_iter()
        .map(|s| s.fqn)
        .collect()
}

fn found(store: &Store, query: &str) -> bool {
    let input = SearchInput {
        query: query.into(),
        ..SearchInput::default()
    };
    symbols::search(store.conn(), &input).unwrap().data["found"] == true
}

#[test]
fn only_changed_files_are_parsed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "app/A.php",
        "<?php\nnamespace App;\nclass A { function one() {} }\n",
        T0,
    );
    write(root, "app/B.php", "<?php\nnamespace App;\nclass B {}\n", T0);
    write(root, "composer.json", "{}", T0);
    let mut store = Store::open_in_memory().unwrap();

    let r = refresh(&mut store, root);
    assert_eq!((r.php.parsed, r.php.symbols), (2, 3));

    let r = refresh(&mut store, root);
    assert_eq!((r.scan.reparsed, r.php.parsed), (0, 0), "{r:?}");

    // Edit A: only A is parsed, and its old method is gone (also from FTS).
    write(
        root,
        "app/A.php",
        "<?php\nnamespace App;\nclass A { function two() {} }\n",
        T0 + 10,
    );
    let r = refresh(&mut store, root);
    assert_eq!(r.php.parsed, 1);
    assert_eq!(fqns(&store), ["App\\A", "App\\A::two", "App\\B"]);
    assert!(!found(&store, "App\\A::one"));
    assert!(found(&store, "App\\A::two"));

    // Delete B: its symbols cascade away.
    std::fs::remove_file(root.join("app/B.php")).unwrap();
    let r = refresh(&mut store, root);
    assert_eq!((r.scan.removed, r.php.parsed), (1, 0));
    assert_eq!(fqns(&store), ["App\\A", "App\\A::two"]);
    assert!(!found(&store, "App\\B"));
}

#[test]
fn older_extractor_version_is_reparsed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "app/A.php", "<?php\nclass A {}\n", T0);
    write(root, "app/B.php", "<?php\nclass B {}\n", T0);
    let mut store = Store::open_in_memory().unwrap();
    refresh(&mut store, root);

    // Simulate an index built by an older extractor.
    store
        .conn()
        .execute("UPDATE extractor_state SET version = version - 1", [])
        .unwrap();
    let r = refresh(&mut store, root);
    assert_eq!((r.scan.reparsed, r.php.parsed), (0, 2));
    // Re-parsing replaces symbols instead of duplicating them.
    assert_eq!(fqns(&store), ["A", "B"]);
}

#[test]
fn syntax_errors_become_warnings() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "app/Bad.php",
        "<?php\nclass Good {}\nclass Bad { function x( }\n",
        T0,
    );
    let (store, report) = common::indexed(root);
    assert_eq!(report.php.warnings, 1);
    let w = &store.warnings().unwrap()[0];
    assert_eq!(
        (w.adapter.as_str(), w.path.as_deref()),
        ("php", Some("app/Bad.php"))
    );
    assert!(fqns(&store).contains(&"Good".to_owned()));
}

#[test]
fn bulk_load_then_incremental_updates_stay_searchable() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // Enough symbols to take the bulk FTS path.
    let methods: String = (0..60).map(|i| format!("function m{i}() {{}}\n")).collect();
    for i in 0..40 {
        write(
            root,
            &format!("app/C{i}.php"),
            &format!("<?php\nnamespace App;\nclass Bulk{i} {{ {methods} }}\n"),
            T0,
        );
    }
    let mut store = Store::open_in_memory().unwrap();
    let r = refresh(&mut store, root);
    assert!(r.php.symbols >= codemap_core::symbols::BULK_FTS_THRESHOLD);
    assert!(found(&store, "Bulk17"));
    assert!(found(&store, "App\\Bulk3::m59"));

    // A small change afterwards goes through the restored per-row trigger.
    write(
        root,
        "app/Late.php",
        "<?php\nnamespace App;\nclass LateArrival {}\n",
        T0 + 5,
    );
    refresh(&mut store, root);
    assert!(found(&store, "LateArrival"));
    write(
        root,
        "app/C0.php",
        "<?php\nnamespace App;\nclass Renamed0 {}\n",
        T0 + 6,
    );
    refresh(&mut store, root);
    assert!(found(&store, "Renamed0"));
    assert!(!found(&store, "Bulk0"));
}

/// `cargo test --release -p codemap-php --test index -- --ignored --nocapture`
#[test]
#[ignore = "performance smoke test"]
fn perf_2000_classes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let methods: String = (0..40)
        .map(|i| {
            format!(
                "    /** Adds {i}. */\n    public function method{i}(int $a, ?Service $s = null): int {{ return $a + {i}; }}\n"
            )
        })
        .collect();
    for i in 0..2000 {
        let ns = format!("App\\Domain{}", i % 40);
        let src = format!(
            "<?php\nnamespace {ns};\n\nuse App\\Support\\Service;\n\nclass Service{i} extends Base implements Contract\n{{\n{methods}}}\n"
        );
        write(
            root,
            &format!("app/Domain{}/Service{i}.php", i % 40),
            &src,
            T0,
        );
    }
    let mut store = Store::open(&dir.path().join("index.db")).unwrap();

    let t = Instant::now();
    let cold = refresh(&mut store, root);
    let cold_ms = t.elapsed().as_millis();
    let t = Instant::now();
    let warm = refresh(&mut store, root);
    let warm_ms = t.elapsed().as_millis();
    let t = Instant::now();
    let input = SearchInput {
        query: "method3".into(),
        ..SearchInput::default()
    };
    let found = symbols::search(store.conn(), &input).unwrap();
    let search_ms = t.elapsed().as_millis();
    assert_eq!(
        found.data["found"], true,
        "bulk-loaded symbols are searchable"
    );
    eprintln!(
        "cold: {cold_ms} ms ({} files, {} symbols); no-op: {warm_ms} ms; search: {search_ms} ms",
        cold.php.parsed, cold.php.symbols
    );
    assert_eq!(cold.php.symbols, 2000 * 41);
    assert_eq!(warm.php.parsed, 0);
    assert!(cold_ms < 5000, "cold {cold_ms} ms");
    assert!(warm_ms < 300, "no-op {warm_ms} ms");
}
