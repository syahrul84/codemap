//! A workspace folder with a Laravel backend and a Nuxt frontend: one server
//! answers for both, and the frontend is linked to the backend automatically.

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

struct Ws {
    projects: Vec<codemap_mcp::Project>,
    stores: Vec<Store>,
}

impl Ws {
    fn open(data: &Path) -> Self {
        let root = fixture("api-link");
        let projects =
            codemap_mcp::workspace::projects(&root, Config::load(&root).unwrap(), data, "ws")
                .unwrap();
        let mut stores = Vec::new();
        for p in &projects {
            let mut s = Store::open(&p.index_path).unwrap();
            codemap_mcp::refresh(&mut s, &p.root, &p.config, ScanOptions::default()).unwrap();
            stores.push(s);
        }
        Self { projects, stores }
    }

    fn call(&self, tool: &str, args: Value) -> Value {
        let apps: Vec<(&Store, &codemap_mcp::Project)> =
            self.stores.iter().zip(&self.projects).collect();
        codemap_mcp::workspace::call(&apps, tool, args, codemap_mcp::Freshness::default())
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
fn change_impact_follows_callers_to_routes_and_their_tests() {
    let data = tempfile::tempdir().unwrap();
    let ws = Ws::open(data.path());
    let r = ws.call(
        "find_usages",
        json!({ "changed_files": ["backend/app/Offers/PriceRules.php"] }),
    );
    let e = &r["changed"][0];
    assert_eq!(e["tests_direct"], 0);
    assert!(
        e["routes_via_callers"]
            .as_array()
            .unwrap()
            .contains(&json!("GET|HEAD api/v1/malls/{mall:slug}/offers (via PriceRules → OfferRanking → MallController)")),
        "{e}"
    );
    assert_eq!(
        e["tests_via_route_files"],
        json!(["tests/Feature/MallTest.php"])
    );
    assert_eq!(
        r["run"],
        json!([
            "cd backend && docker compose exec -T app php artisan test tests/Feature/MallTest.php"
        ])
    );
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
        json!([
            "cd backend && docker compose exec -T app php artisan test tests/Feature/MallTest.php"
        ])
    );
    let ctrl = &back["changed"][0];
    assert_eq!(ctrl["is"], "class App\\Http\\Controllers\\MallController");
    assert_eq!(ctrl["routes"].as_array().unwrap().len(), 3);
    let offer = &back["changed"][1];
    assert_eq!(
        offer["used_in"],
        json!([
            "app/Http/Controllers/MallController.php",
            "app/Http/Resources/MallResource.php"
        ])
    );
    let front = &r["apps"]["frontend"];
    assert_eq!(front["changed"][0]["file"], "pages/malls/index.vue");
    assert_eq!(front["tests"], json!([]));
    // The backend change reaches the frontend: calls to the changed routes.
    assert_eq!(
        r["frontend_calls_to_changed_routes"]["frontend"],
        json!([
            "composables/api/malls.ts:10  ? /malls → GET|HEAD api/v1/malls",
            "pages/malls/[slug].vue:3  ? /malls/{} → GET|HEAD api/v1/malls/{mall:slug}",
            "pages/malls/index.vue:3  GET /malls → GET|HEAD api/v1/malls",
            "pages/malls/offers.vue:6  GET /malls/{}/offers → GET|HEAD api/v1/malls/{mall:slug}/offers"
        ])
    );
}

#[test]
fn release_check_lists_only_problems_per_app() {
    let data = tempfile::tempdir().unwrap();
    let ws = Ws::open(data.path());
    let r = ws.call("verify", json!({ "release_check": true }));
    assert_eq!(r["clean"], false);
    let problems = |app: &str| -> Vec<String> {
        r["apps"][app]["problems"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_str().unwrap().to_owned())
            .collect()
    };
    let back = problems("backend");
    assert!(
        back.iter().any(|p| p
            == "translation: resources/views/receipt.blade.php:3 orders.shipped used but defined in no locale (laravel)"),
        "{back:?}"
    );
    assert!(
        back.iter()
            .any(|p| p.starts_with("copy: lang/ms/orders.php:4 \"dibuat\"")),
        "{back:?}"
    );
    let front = problems("frontend");
    assert!(
        front
            .iter()
            .any(|p| p.starts_with("api: ? /orders/history  pages/malls/[slug].vue:4")),
        "{front:?}"
    );
    assert!(
        r["apps"]["backend"]["checked"]
            .as_array()
            .unwrap()
            .contains(&json!("copy rules"))
    );
    // A nested field whose last word has a Malay name, but the full key does not.
    assert!(
        back.iter().any(|p| p.starts_with(
            "translation: field `translations.*.description` has no name in ms validation.attributes (only `description` has)"
        )),
        "{back:?}"
    );
    // `codemap-ignore` marks the env() read as intended; the guarded translation is fine.
    assert!(
        back.iter().any(|p| p.contains(
            "field `translations.*.description` has no name in en validation.attributes (ms names `description`)"
        )),
        "{back:?}"
    );
    assert!(
        !back.iter().any(|p| p.contains("Closed for the day")),
        "{back:?}"
    );
    assert!(
        !back.iter().any(|p| p.contains("ALLOW_DB_WIPE")),
        "{back:?}"
    );
    assert!(
        r["apps"]["backend"]["ignored"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p.as_str().unwrap().contains("ALLOW_DB_WIPE"))
    );
    assert!(
        !back.iter().any(|p| p.contains("orders.archived")),
        "{back:?}"
    );
}

#[test]
fn code_map_links_screens_to_tables() {
    let data = tempfile::tempdir().unwrap();
    let ws = Ws::open(data.path());
    let apps: Vec<(&Store, &codemap_mcp::Project)> = ws.stores.iter().zip(&ws.projects).collect();
    let mut map = codemap_mcp::map::data(&apps, "api-link").unwrap();
    codemap_mcp::map::prune(&mut map);
    let nodes = map["nodes"].as_array().unwrap();
    let edges = map["edges"].as_array().unwrap();
    let label = |id: &str| {
        nodes
            .iter()
            .find(|n| n["id"] == id)
            .map(|n| {
                format!(
                    "{}:{}",
                    n["kind"].as_str().unwrap(),
                    n["label"].as_str().unwrap()
                )
            })
            .unwrap_or_default()
    };
    let links: Vec<String> = edges
        .iter()
        .map(|e| {
            format!(
                "{} -{}-> {}",
                label(e["from"].as_str().unwrap()),
                e["kind"].as_str().unwrap(),
                label(e["to"].as_str().unwrap())
            )
        })
        .collect();
    // Screen → endpoint → controller → model, across the two apps.
    let has = |s: &str| links.iter().any(|l| l == s);
    assert!(
        has("screen:/malls -screen_calls-> endpoint:GET /api/v1/malls"),
        "{links:#?}"
    );
    assert!(
        links
            .iter()
            .any(|l| l.starts_with("endpoint:GET /api/v1/malls -handled_by-> controller:"))
    );
    assert!(
        has("controller:MallController -uses-> model:Mall"),
        "{links:#?}"
    );
    // A screen reaches the shared composable's functions it uses only.
    assert!(has(
        "frontend:useCartApi.ts -calls-> endpoint:GET /api/v1/carts"
    ));
    // The page carries the data, and text in it cannot close the script.
    let page = codemap_mcp::map::html(&map);
    assert!(page.contains("\"title\":\"api-link\""));
    assert!(!page.contains("/*__CODEMAP_DATA__*/"));
    assert_eq!(page.matches("</script>").count(), 1);
}

#[test]
fn ai_mistakes_are_logged_and_rechecked() {
    let data = tempfile::tempdir().unwrap();
    let ws = Ws::open(data.path());
    let projects: Vec<&codemap_mcp::Project> = ws.projects.iter().collect();
    let asked = [
        (
            "verify",
            json!({ "claims": { "classes": ["App\\Models\\Mal"] } }),
        ),
        (
            "verify",
            json!({ "claims": { "classes": ["App\\Models\\Mal"] } }),
        ),
        ("search_symbol", json!({ "query": "MallControler" })),
        (
            "verify",
            json!({ "claims": { "classes": ["App\\Models\\Mall"] } }),
        ),
    ];
    for (tool, args) in &asked {
        let answer = ws.call(tool, args.clone());
        codemap_mcp::misses::record(&projects, tool, args, &answer);
    }
    let apps: Vec<(&Store, &codemap_mcp::Project)> = ws.stores.iter().zip(&ws.projects).collect();
    let r = codemap_mcp::misses::report(&apps, 10);
    let lines: Vec<String> = r
        .iter()
        .map(|m| {
            format!(
                "{} {} {} {}",
                m["times"], m["kind"], m["item"], m["closest"]
            )
        })
        .collect();
    assert_eq!(
        lines,
        [
            r#"2 "class" "App\\Models\\Mal" "App\\Models\\Mall""#,
            r#"1 "symbol" "MallControler" "App\\Http\\Controllers\\MallController""#,
        ]
    );
    // The map shows them, and marks the real class the AI meant.
    let map = codemap_mcp::map::data(&apps, "api-link").unwrap();
    assert_eq!(map["mistakes"].as_array().unwrap().len(), 2);
    let mall = map["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["sub"] == "App\\Models\\Mall")
        .unwrap();
    assert_eq!(mall["mistaken_for"][0], "2× asked for `App\\Models\\Mal`");
}
