//! Runs the real `codemap` binary. Guards the rule that stdout carries only
//! command output (MCP protocol in serve mode) and logs go to stderr.

use std::process::{Command, Output};

fn codemap(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_codemap"))
        .args(args)
        .env_remove("CODEMAP_LOG")
        .env_remove("CODEMAP_PROJECT_ID")
        .env_remove("CODEMAP_DATA_DIR")
        .output()
        .expect("failed to run codemap")
}

#[test]
fn help_prints_usage_on_stdout_only() {
    let out = codemap(&["--help"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Usage: codemap"), "stdout: {stdout}");
    for cmd in ["scan", "serve", "query", "stats", "doctor"] {
        assert!(stdout.contains(cmd), "help is missing `{cmd}`");
    }
    assert!(out.stderr.is_empty());
}

#[test]
fn errors_log_to_stderr_and_keep_stdout_empty() {
    let out = codemap(&[
        "serve",
        "--project",
        "/no/such/project",
        "--log-level",
        "debug",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        out.stdout.is_empty(),
        "stdout must stay empty, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not found"), "stderr: {stderr}");
    assert!(
        stderr.contains("starting codemap"),
        "debug log missing: {stderr}"
    );
}

#[test]
fn invalid_log_level_fails_cleanly() {
    let out = codemap(&["stats", "--project", "/w", "--log-level", "bad [filter"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid log level"));
}

fn listing(root: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            out.push(path.strip_prefix(root).unwrap().display().to_string());
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
    out.sort();
    out
}

fn json(out: &Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "exit {:?}, stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("stdout is JSON")
}

#[test]
fn scan_then_stats_end_to_end() {
    let project = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let root = project.path();
    std::fs::create_dir_all(root.join("app/Models")).unwrap();
    std::fs::write(root.join("app/Models/Order.php"), "<?php\nclass Order {}\n").unwrap();
    std::fs::write(root.join("composer.json"), "{}").unwrap();
    std::fs::write(root.join(".env"), "SECRET=1\n").unwrap();
    let before = listing(root);

    let p = root.to_str().unwrap();
    let d = data.path().to_str().unwrap();
    let common = ["--project", p, "--data-dir", d, "--project-id", "t1"];
    let scan_args: Vec<&str> = ["scan"]
        .iter()
        .chain(&common)
        .chain(&["--json"])
        .copied()
        .collect();

    let first = json(&codemap(&scan_args));
    assert_eq!(first["scan"]["walked"], 2);
    assert_eq!(first["scan"]["reparsed"], 2);
    assert_eq!(first["scan"]["changed_paths"][0], "app/Models/Order.php");
    assert_eq!(first["php"]["parsed"], 1);
    assert_eq!(first["php"]["symbols"], 1);

    let second = json(&codemap(&scan_args));
    assert_eq!(second["scan"]["reparsed"], 0);
    assert_eq!(second["scan"]["unchanged"], 2);
    assert_eq!(second["php"]["parsed"], 0);

    let stats_args: Vec<&str> = ["stats"]
        .iter()
        .chain(&common)
        .chain(&["--json"])
        .copied()
        .collect();
    let stats = json(&codemap(&stats_args));
    assert_eq!(stats["project_id"], "t1");
    assert_eq!(stats["files_total"], 2);
    assert_eq!(stats["files_by_lang"]["php"], 1);
    assert_eq!(stats["symbols_by_kind"]["class"], 1);
    assert_eq!(stats["last_scan"]["reparsed"], 0);
    assert!(stats["index_bytes"].as_u64().unwrap() > 0);
    assert!(data.path().join("t1/index.db").exists());

    // The scanned project is never written to.
    assert_eq!(listing(root), before);

    // Plain-text summary.
    let text = codemap(&[
        "scan",
        "--project",
        p,
        "--data-dir",
        d,
        "--project-id",
        "t1",
    ]);
    assert!(String::from_utf8_lossy(&text.stdout).contains("(0 re-parsed)"));
}

#[test]
fn stats_without_index_fails_with_hint() {
    let project = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let out = codemap(&[
        "stats",
        "--project",
        project.path().to_str().unwrap(),
        "--data-dir",
        data.path().to_str().unwrap(),
        "--project-id",
        "none",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("run `codemap scan` first"));
}

#[test]
fn bad_project_id_is_rejected() {
    let project = tempfile::tempdir().unwrap();
    let out = codemap(&[
        "scan",
        "--project",
        project.path().to_str().unwrap(),
        "--project-id",
        "../escape",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid project id"));
}

fn query_fixture(args: &[&str]) -> Output {
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/laravel11-slim");
    let data = std::env::temp_dir().join(format!("codemap-cli-test-{}", std::process::id()));
    let data = data.to_str().unwrap().to_owned();
    let mut all = vec![
        "query",
        "--project",
        fixture,
        "--data-dir",
        &data,
        "--project-id",
        "q",
    ];
    all.extend_from_slice(args);
    codemap(&all)
}

#[test]
fn query_search_symbol_returns_envelope() {
    let out = query_fixture(&["search_symbol", r#"{"query":"OrderStatus"}"#]);
    let v = json(&out);
    assert_eq!(v["data"]["found"], true);
    assert_eq!(v["data"]["matches"][0]["fqn"], "App\\Enums\\OrderStatus");
    let meta = v["meta"].as_object().unwrap();
    for key in ["project", "indexed_at", "git_commit"] {
        assert!(meta.contains_key(key), "meta.{key} missing: {meta:?}");
    }
    // Default values are left out to save tokens.
    for key in ["truncated", "next_cursor", "warnings"] {
        assert!(
            !meta.contains_key(key),
            "meta.{key} should be omitted: {meta:?}"
        );
    }
    assert_eq!(v["meta"]["project"], "acme/slim-shop");
    assert!(
        v["meta"].get("stale").is_none(),
        "stale is omitted when false"
    );

    let typo = json(&query_fixture(&[
        "search_symbol",
        r#"{"query":"OrderStatsu"}"#,
    ]));
    assert_eq!(typo["data"]["found"], false);
    assert_eq!(typo["data"]["suggestions"][0]["name"], "OrderStatus");
}

#[test]
fn query_rejects_unknown_tool_and_bad_args() {
    let out = query_fixture(&["find_everything"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("available: project_overview"));

    let out = query_fixture(&["search_symbol", "{not json"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());

    let out = query_fixture(&["search_symbol", r#"{"querry":"x"}"#]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid arguments"));
}

#[test]
fn doctor_explains_detection() {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/laravel11-upgraded"
    );
    let data = tempfile::tempdir().unwrap();
    let args = [
        "doctor",
        "--project",
        fixture,
        "--data-dir",
        data.path().to_str().unwrap(),
        "--project-id",
        "d",
    ];
    let out = codemap(&args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("project      acme/upgraded-billing"),
        "{text}"
    );
    assert!(
        text.contains("laravel      11.9.2 (from composer.lock)"),
        "{text}"
    );
    assert!(text.contains("layout       legacy"), "{text}");
    assert!(text.contains("upgraded app"), "{text}");
    assert!(text.contains("routes       3 (3 static)"), "{text}");
    assert!(text.contains("missing code 0"), "{text}");

    let mut json_args = args.to_vec();
    json_args.push("--json");
    let v = json(&codemap(&json_args));
    assert_eq!(v["detection"]["layout"], "legacy");
    assert_eq!(v["routes"]["total"], 3);
}

#[test]
fn query_list_routes_filters_and_not_found() {
    let v = json(&query_fixture(&[
        "list_routes",
        r#"{"uri_contains":"customers","method":"PUT"}"#,
    ]));
    assert_eq!(v["data"]["found"], true);
    assert_eq!(v["data"]["total"], 1);
    assert_eq!(v["data"]["routes"][0]["name"], "customers.update");
    assert_eq!(v["data"]["routes"][0]["file"], "routes/api.php:9");
    assert_eq!(v["meta"]["project"], "acme/slim-shop");

    let v = json(&query_fixture(&[
        "list_routes",
        r#"{"name_prefix":"order.","limit":2}"#,
    ]));
    assert_eq!(v["data"]["found"], false);
    assert!(v["data"]["hint"].as_str().unwrap().contains("closest"));

    let v = json(&query_fixture(&[
        "list_routes",
        r#"{"controller":"OrderController","limit":2}"#,
    ]));
    assert_eq!(v["data"]["routes"].as_array().unwrap().len(), 2);
    assert_eq!(v["meta"]["truncated"], true);
    assert!(v["meta"]["next_cursor"].is_string());
}

#[test]
fn migrations_not_yet_applied_are_listed_per_database() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/laravel11-slim")
        .canonicalize()
        .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("local.sqlite");
    {
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch(
            "CREATE TABLE migrations (id INTEGER PRIMARY KEY, migration TEXT, batch INTEGER);
             INSERT INTO migrations (migration, batch) VALUES
               ('0001_01_01_000000_create_users_table', 1),
               ('2024_05_01_000001_create_customers_table', 1),
               ('2024_05_01_000002_create_orders_table', 2);",
        )
        .unwrap();
    }
    let out = Command::new(env!("CARGO_BIN_EXE_codemap"))
        .args([
            "query",
            "--project",
            fixture.to_str().unwrap(),
            "get_schema",
            r#"{"migrations": true}"#,
        ])
        .env_remove("CODEMAP_LOG")
        .env_remove("CODEMAP_PROJECT_ID")
        .env("CODEMAP_DATA_DIR", tmp.path().join("data"))
        .env("CODEMAP_DB_LOCAL", format!("sqlite://{}", db.display()))
        .env(
            "CODEMAP_DB_STAGING",
            "postgres://app:s3cret@127.0.0.1:1/none",
        )
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("s3cret"), "credentials leaked: {stdout}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let dbs = v["data"]["databases"].as_array().unwrap();
    let local = dbs.iter().find(|d| d["name"] == "local").unwrap();
    assert_eq!(local["status"], "pending");
    assert_eq!(local["last_batch"], 2);
    assert_eq!(
        local["pending"][0],
        "2024_05_01_000003_create_order_items_table"
    );
    let staging = dbs.iter().find(|d| d["name"] == "staging").unwrap();
    assert_eq!(staging["status"], "unreachable");
    assert_eq!(staging["database"], "postgres 127.0.0.1:1/none");
}
