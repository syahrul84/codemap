//! A generated large project (thousands of routes, hundreds of frontend
//! modules sharing prefix helpers): failures that only show at scale, such as
//! variable URL prefixes left unresolved because their tail fits many routes.

use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;

use codemap_core::{Config, ScanOptions, Store};
use serde_json::{Value, json};

const GROUPS: usize = 40;
const ROUTES_PER_SIDE: usize = 50; // two sides (x and y) per group: 4,000 routes
const MODULES: usize = 300;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn generate(root: &Path) {
    write(
        root,
        "composer.json",
        r#"{"require":{"php":"^8.2","laravel/framework":"^11.0"}}"#,
    );
    write(root, "artisan", "#!/usr/bin/env php\n<?php\n");
    write(
        root,
        "bootstrap/app.php",
        "<?php\nuse Illuminate\\Foundation\\Application;\nreturn Application::configure(basePath: dirname(__DIR__))\n    ->withRouting(api: __DIR__.'/../routes/api.php')\n    ->create();\n",
    );
    let mut routes = String::from("<?php\nuse Illuminate\\Support\\Facades\\Route;\n");
    for g in 0..GROUPS {
        for side in ["x", "y"] {
            let _ = writeln!(
                routes,
                "Route::prefix('area{g}/{side}')->group(function () {{"
            );
            for r in 0..ROUTES_PER_SIDE {
                let _ = writeln!(routes, "    Route::get('item{r}/calculate', fn () => []);");
            }
            routes.push_str("});\n");
        }
    }
    write(root, "routes/api.php", &routes);
    write(
        root,
        "package.json",
        r#"{"dependencies":{"vue":"^3.4.0","axios":"^1.6.0"},"devDependencies":{"vite":"^5.0.0"}}"#,
    );
    write(
        root,
        "resources/js/api.js",
        "import axios from 'axios'\nexport const api = axios.create({ baseURL: '/api' })\n",
    );
    for m in 0..MODULES {
        let g = m % GROUPS;
        let mut src = String::from("import { api } from '../api'\n");
        let _ = writeln!(
            src,
            "const prefix = (kind) => (kind === 'x' ? 'area{g}/x' : 'area{g}/y')"
        );
        for r in [m % ROUTES_PER_SIDE, (m + 7) % ROUTES_PER_SIDE] {
            let _ = writeln!(
                src,
                "export function calc{r}(kind) {{\n  return api.get(`${{prefix(kind)}}/item{r}/calculate`)\n}}"
            );
        }
        // A value codemap does not follow: unresolved, saying why.
        if m == 0 {
            src.push_str("const contract = { base: 'area0/x' }\nexport function viaObject() {\n  return api.get(`${contract.base}/item1/calculate`)\n}\n");
        }
        write(root, &format!("resources/js/modules/m{m}.js"), &src);
    }
}

#[test]
fn prefixes_resolve_at_scale() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("p");
    generate(&root);
    let project = codemap_mcp::Project {
        app: None,
        config: Config::load(&root).unwrap(),
        root: root.clone(),
        id: "stress".into(),
        index_path: dir.path().join("idx/index.db"),
    };
    let started = Instant::now();
    let mut store = Store::open(&project.index_path).unwrap();
    codemap_mcp::refresh(&mut store, &root, &project.config, ScanOptions::default()).unwrap();
    let indexed = started.elapsed();

    let started = Instant::now();
    let v: Value = codemap_mcp::call(
        &store,
        &project,
        "api_usage",
        json!({ "status": "unresolved", "detail": "full", "limit": 50 }),
        codemap_mcp::Freshness::default(),
    )
    .unwrap()
    .data;
    let checked = started.elapsed();

    let routes: i64 = store
        .conn()
        .query_row("SELECT COUNT(*) FROM routes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(routes as usize, GROUPS * 2 * ROUTES_PER_SIDE);
    // Every prefix-helper call resolves; only the object-property one stays
    // unresolved, and it says why.
    assert_eq!(
        v["summary"]["unresolved"], 1,
        "{}\n{}",
        v["summary"], v["calls"][0]
    );
    assert_eq!(
        v["summary"]["matched"].as_u64(),
        Some((MODULES * 2) as u64),
        "{}",
        v["summary"]
    );
    let why = v["calls"][0]["why"].as_str().unwrap_or_default();
    assert!(
        why.contains("prefix not expanded: it comes from an object property"),
        "{why}"
    );
    // Generous bounds for debug builds on shared CI machines.
    assert!(indexed.as_secs() < 120, "indexing took {indexed:?}");
    assert!(checked.as_secs() < 60, "api_usage took {checked:?}");
    eprintln!(
        "stress: {routes} routes, {MODULES} modules, indexed in {indexed:?}, api_usage in {checked:?}"
    );
}
