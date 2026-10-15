//! A workspace folder with a Laravel backend and a Nuxt frontend: one server
//! answers for both, and the frontend is linked to the backend automatically.

use std::path::{Path, PathBuf};

use pidx_core::{Config, ScanOptions, Store};
use serde_json::{Value, json};

fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(rel)
        .canonicalize()
        .unwrap()
}

struct Ws {
    projects: Vec<pidx_mcp::Project>,
    stores: Vec<Store>,
}

impl Ws {
    fn open(data: &Path) -> Self {
        let root = fixture("api-link");
        let projects =
            pidx_mcp::workspace::projects(&root, Config::load(&root).unwrap(), data, "ws").unwrap();
        let mut stores = Vec::new();
        for p in &projects {
            let mut s = Store::open(&p.index_path).unwrap();
            pidx_mcp::refresh(&mut s, &p.root, &p.config, ScanOptions::default()).unwrap();
            stores.push(s);
        }
        Self { projects, stores }
    }

    fn call(&self, tool: &str, args: Value) -> Value {
        let apps: Vec<(&Store, &pidx_mcp::Project)> =
            self.stores.iter().zip(&self.projects).collect();
        pidx_mcp::workspace::call(&apps, tool, args, pidx_mcp::Freshness::default())
            .unwrap()
            .data
    }
}

#[test]
fn workspace_apps_are_found_and_linked() {
    let data = tempfile::tempdir().unwrap();
    let ws = Ws::open(data.path());
    let apps: Vec<&str> = ws
        .projects
        .iter()
        .map(|p| p.app.as_deref().unwrap())
        .collect();
    assert_eq!(apps, ["backend", "frontend"]);
    let front = &ws.projects[1];
    assert_eq!(
        front.config.links[0].index_path.as_ref(),
        Some(&ws.projects[0].index_path)
    );

    let overview = ws.call("project_overview", json!({}));
    assert!(
        overview["apps"]["backend"]["framework"]["laravel"].is_string(),
        "{overview}"
    );
    assert!(
        overview["apps"]["frontend"]["frontend"]["framework"].is_string(),
        "{overview}"
    );

    // Only the frontend makes HTTP calls: its answer comes back, tagged.
    let api = ws.call("api_usage", json!({ "status": "missing" }));
    assert_eq!(api["app"], "frontend");
    assert_eq!(api["backends"][0]["api_base"], "/api/v1");

    // Only the backend has this class.
    let sym = ws.call("search_symbol", json!({ "query": "MallResource" }));
    assert_eq!(sym["app"], "backend");

    // Explicit app.
    let none = ws.call(
        "get_component",
        json!({ "name": "MallController", "app": "backend" }),
    );
    assert_eq!(none["found"], false);

    // verify merges items across apps.
    let v = ws.call(
        "verify",
        json!({ "claims": { "classes": ["App\\Http\\Resources\\MallResource"], "endpoints": ["GET /malls"], "i18n_keys": ["orders.created"] } }),
    );
    let st: Vec<(&str, &str)> = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["status"].as_str().unwrap(),
                r["app"].as_str().unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(
        st,
        [
            ("exists", "backend"),
            ("exists", "backend"),
            ("exists", "frontend")
        ]
    );
    assert_eq!(v["all_exist"], true);
}

#[test]
fn change_impact_routes_files_to_apps_and_lists_tests() {
    let data = tempfile::tempdir().unwrap();
    let ws = Ws::open(data.path());
    let r = ws.call(
        "find_usages",
        json!({ "changed_files": [
            "backend/app/Http/Controllers/MallController.php",
            "backend/app/Http/Resources/OfferResource.php",
            "frontend/pages/malls/index.vue"
        ] }),
    );
    let back = &r["apps"]["backend"];
    assert_eq!(back["tests"], json!(["tests/Feature/MallTest.php"]));
    assert_eq!(
        back["run"],
        json!(["cd backend && php artisan test tests/Feature/MallTest.php"])
    );
    let ctrl = &back["changed"][0];
    assert_eq!(ctrl["is"], "class App\\Http\\Controllers\\MallController");
    assert_eq!(ctrl["routes"].as_array().unwrap().len(), 2);
    let offer = &back["changed"][1];
    assert_eq!(
        offer["used_in"],
        json!(["app/Http/Resources/MallResource.php"])
    );
    let front = &r["apps"]["frontend"];
    assert_eq!(front["changed"][0]["file"], "pages/malls/index.vue");
    assert_eq!(front["tests"], json!([]));
}
