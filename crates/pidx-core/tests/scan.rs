//! Scanner behaviour on a generated Laravel-shaped tree. Mtimes are set
//! explicitly so results never depend on timing.

use std::path::Path;
use std::time::Instant;

use filetime::{FileTime, set_file_mtime};
use pidx_core::scan::{MAX_FILE_BYTES, SCANNER, scan};
use pidx_core::{Config, ScanOptions, ScanReport, Store};

const T0: i64 = 1_760_000_000;

/// Files that must be indexed.
const INDEXED: &[(&str, &str)] = &[
    (
        "composer.json",
        r#"{"autoload":{"psr-4":{"App\\":"app/"}}}"#,
    ),
    ("composer.lock", r#"{"packages":[]}"#),
    ("artisan", "#!/usr/bin/env php\n<?php\n"),
    (
        "app/Models/User.php",
        "<?php\nnamespace App\\Models;\nclass User {}\n",
    ),
    (
        "app/Http/Controllers/UserController.php",
        "<?php\nnamespace App\\Http\\Controllers;\nclass UserController {}\n",
    ),
    ("routes/web.php", "<?php\nRoute::get('/', fn () => 'ok');\n"),
    (
        "routes/api.php",
        "<?php\nRoute::get('/users', [UserController::class, 'index']);\n",
    ),
    (
        "database/migrations/2024_01_01_000000_create_users_table.php",
        "<?php\nreturn new class extends Migration {};\n",
    ),
    (
        "database/schema/mysql-schema.sql",
        "CREATE TABLE users (id bigint);\n",
    ),
    ("config/app.php", "<?php\nreturn ['name' => 'demo'];\n"),
    // Blade views: scanned for translation keys.
    (
        "resources/views/welcome.blade.php",
        "<p>{{ __('hi') }}</p>\n",
    ),
    (
        "bootstrap/app.php",
        "<?php\nreturn Application::configure();\n",
    ),
];

/// Files that must never be indexed.
const NOT_INDEXED: &[(&str, &str)] = &[
    (".env", "APP_KEY=base64:secret\n"),
    ("app/.env.php", "<?php // secret\n"),
    ("vendor/laravel/framework/src/Foo.php", "<?php\n"),
    ("app/node_modules/pkg/index.json", "{}"),
    ("storage/framework/views/abc.php", "<?php\n"),
    ("bootstrap/cache/packages.php", "<?php\n"),
    ("public/index.php", "<?php\n"),
    ("resources/css/app.css", "body {}\n"),
    ("app/.Hidden.php", "<?php\n"),
    ("app/Generated/Stub.php", "<?php\n"),
    ("app/notes.txt", "not code\n"),
    (".gitignore", "/app/Generated/\n"),
];

fn write(root: &Path, rel: &str, content: &[u8], mtime: i64) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, content).unwrap();
    set_file_mtime(&path, FileTime::from_unix_time(mtime, 0)).unwrap();
}

fn make_tree(root: &Path) {
    for (rel, content) in INDEXED.iter().chain(NOT_INDEXED) {
        write(root, rel, content.as_bytes(), T0);
    }
}

fn run(store: &mut Store, root: &Path, full: bool) -> ScanReport {
    scan(store, root, &Config::default(), ScanOptions { full }).unwrap()
}

#[test]
fn incremental_rescans_only_changed_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    make_tree(root);
    let mut store = Store::open_in_memory().unwrap();
    let n = INDEXED.len();

    // Cold scan: everything is new.
    let r = run(&mut store, root, false);
    assert_eq!((r.walked, r.added, r.reparsed), (n, n, n));
    assert_eq!(
        (r.unchanged, r.modified, r.removed, r.touched),
        (0, 0, 0, 0)
    );

    // No changes: nothing is read or re-parsed.
    let r = run(&mut store, root, false);
    assert_eq!(r.reparsed, 0, "{r:?}");
    assert_eq!(r.unchanged, n);
    assert!(r.changed_paths.is_empty());

    // Edit one file.
    write(
        root,
        "app/Models/User.php",
        b"<?php\nclass User { use HasFactory; }\n",
        T0 + 10,
    );
    let r = run(&mut store, root, false);
    assert_eq!(
        (r.modified, r.reparsed, r.unchanged),
        (1, 1, n - 1),
        "{r:?}"
    );
    assert_eq!(r.changed_paths, ["app/Models/User.php"]);

    // Touch without changing content: re-hashed, not re-parsed.
    set_file_mtime(
        root.join("routes/web.php"),
        FileTime::from_unix_time(T0 + 20, 0),
    )
    .unwrap();
    let r = run(&mut store, root, false);
    assert_eq!((r.touched, r.reparsed), (1, 0), "{r:?}");
    // ...and the new mtime is stored, so the next scan does not re-hash it.
    let r = run(&mut store, root, false);
    assert_eq!((r.touched, r.unchanged), (0, n));

    // Delete one file.
    std::fs::remove_file(root.join("routes/api.php")).unwrap();
    let r = run(&mut store, root, false);
    assert_eq!((r.removed, r.reparsed, r.walked), (1, 0, n - 1), "{r:?}");
    assert_eq!(r.removed_paths, ["routes/api.php"]);

    // Add one file.
    write(
        root,
        "app/helpers.php",
        b"<?php\nfunction money() {}\n",
        T0 + 30,
    );
    let r = run(&mut store, root, false);
    assert_eq!((r.added, r.reparsed), (1, 1), "{r:?}");

    // --full re-parses everything.
    let r = run(&mut store, root, true);
    assert_eq!((r.reparsed, r.modified, r.added), (n, n, 0), "{r:?}");
}

#[test]
fn modified_file_gets_a_new_row_so_facts_cascade() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    make_tree(root);
    let mut store = Store::open_in_memory().unwrap();
    run(&mut store, root, false);

    let id_of = |store: &Store| -> i64 {
        store
            .conn()
            .query_row(
                "SELECT id FROM files WHERE path = 'app/Models/User.php'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    let old_id = id_of(&store);
    store
        .conn()
        .execute(
            "INSERT INTO warnings (file_id, adapter, message) VALUES (?1, 'php', 'old fact')",
            [old_id],
        )
        .unwrap();

    write(
        root,
        "app/Models/User.php",
        b"<?php\nclass User {}\n// v2\n",
        T0 + 10,
    );
    run(&mut store, root, false);
    assert_ne!(id_of(&store), old_id);
    assert!(
        store
            .warnings()
            .unwrap()
            .iter()
            .all(|w| w.message != "old fact")
    );
}

#[test]
fn excluded_and_secret_files_are_never_indexed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    make_tree(root);
    write(
        root,
        "app/Huge.php",
        &vec![b'x'; MAX_FILE_BYTES as usize + 1],
        T0,
    );
    let mut store = Store::open_in_memory().unwrap();
    let r = run(&mut store, root, false);

    let paths: Vec<String> = store
        .list_files()
        .unwrap()
        .into_iter()
        .map(|f| f.path)
        .collect();
    let mut expected: Vec<&str> = INDEXED.iter().map(|(p, _)| *p).collect();
    expected.sort();
    assert_eq!(paths, expected);

    assert_eq!((r.skipped, r.warnings), (1, 1));
    let warnings = store.warnings().unwrap();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].adapter, SCANNER);
    assert_eq!(warnings[0].path.as_deref(), Some("app/Huge.php"));
    assert!(warnings[0].message.contains("over the"));

    // Scanner warnings are replaced, not accumulated, on each scan.
    run(&mut store, root, false);
    assert_eq!(store.warnings().unwrap().len(), 1);
}

#[test]
fn config_include_and_exclude_are_respected() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    make_tree(root);
    let config = Config::parse("[scan]\ninclude = [\"app\"]\nexclude = [\"app/Http\"]\n").unwrap();
    let mut store = Store::open_in_memory().unwrap();
    scan(&mut store, root, &config, ScanOptions::default()).unwrap();
    let paths: Vec<String> = store
        .list_files()
        .unwrap()
        .into_iter()
        .map(|f| f.path)
        .collect();
    assert_eq!(
        paths,
        [
            "app/Models/User.php",
            "artisan",
            "composer.json",
            "composer.lock"
        ]
    );
}

#[test]
fn files_table_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    make_tree(dir.path());
    let mut store = Store::open_in_memory().unwrap();
    run(&mut store, dir.path(), false);
    insta::assert_json_snapshot!(store.list_files().unwrap());
}

#[test]
fn stats_reflect_last_scan() {
    let dir = tempfile::tempdir().unwrap();
    make_tree(dir.path());
    let mut store = Store::open_in_memory().unwrap();
    run(&mut store, dir.path(), false);
    let stats = store.stats().unwrap();
    assert_eq!(stats.files_total, INDEXED.len() as u64);
    assert_eq!(stats.files_by_lang["php"], 9); // incl. the Blade view
    assert_eq!(stats.files_by_lang["json"], 2); // composer.json + composer.lock
    assert_eq!(stats.files_by_lang["sql"], 1);
    assert!(stats.indexed_at.is_some());
    // Not a git repo.
    assert_eq!((stats.git_commit, stats.git_dirty), (None, None));
    let last = stats.last_scan.unwrap();
    assert_eq!(last.reparsed, INDEXED.len());
    assert!(
        last.changed_paths.is_empty(),
        "paths are not stored in meta"
    );
}

/// `cargo test -p pidx-core --test scan -- --ignored --nocapture`
#[test]
#[ignore = "performance smoke test"]
fn perf_2000_php_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let body: String = (0..40)
        .map(|i| format!("    public function method{i}(int $a): int {{ return $a + {i}; }}\n"))
        .collect();
    for i in 0..2000 {
        let rel = format!("app/Domain{}/Service{i}.php", i % 40);
        let src = format!(
            "<?php\nnamespace App\\Domain{};\n\nclass Service{i}\n{{\n{body}}}\n",
            i % 40
        );
        write(root, &rel, src.as_bytes(), T0);
    }
    let db = dir.path().join("index.db");
    let mut store = Store::open(&db).unwrap();

    let t = Instant::now();
    let cold = run(&mut store, root, false);
    let cold_ms = t.elapsed().as_millis();
    let t = Instant::now();
    let warm = run(&mut store, root, false);
    let warm_ms = t.elapsed().as_millis();
    eprintln!(
        "cold scan: {cold_ms} ms ({} files); no-op rescan: {warm_ms} ms",
        cold.walked
    );

    assert_eq!(cold.reparsed, 2000);
    assert_eq!(warm.reparsed, 0);
    assert!(cold_ms < 5000, "cold scan took {cold_ms} ms");
    assert!(warm_ms < 300, "no-op rescan took {warm_ms} ms");
}

#[test]
fn default_excludes_are_anchored_to_the_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (rel, text) in [
        ("package.json", r#"{"dependencies":{"vue":"^3"}}"#),
        ("src/App.vue", "<template><div/></template>"),
        // Source folders that share a name with an excluded root folder.
        (
            "src/views/vendor/VendorList.vue",
            "<template><div/></template>",
        ),
        ("src/public/api.ts", "export const x = 1"),
        ("vendor/acme/lib/src/Thing.php", "<?php class Thing {}"),
        ("app/Models/User.php", "<?php class User {}"),
        ("node_modules/vue/index.js", "module.exports = {}"),
        ("src/node_modules/x.js", "module.exports = {}"),
        ("dist/app.js", "x"),
        ("src/lib/jquery.min.js", "x"),
        ("src/env.d.ts", "declare const x: number"),
    ] {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    let mut store = Store::open_in_memory().unwrap();
    scan(&mut store, root, &Config::default(), ScanOptions::default()).unwrap();
    let mut paths: Vec<String> = store
        .list_files()
        .unwrap()
        .into_iter()
        .map(|f| f.path)
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        [
            "app/Models/User.php",
            "package.json",
            "src/App.vue",
            "src/public/api.ts",
            "src/views/vendor/VendorList.vue",
        ]
    );
}
