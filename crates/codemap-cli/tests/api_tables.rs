//! HTTP calls whose URL is not written in the call: kept in a constant of
//! the function, read out of a table of definitions
//! (`contracts[kind].endpoint`), built by a helper of the file, or read from
//! an object the callers hand in (`contract.endpoint`). A route's callers
//! include them, so an endpoint a page reaches this way is not shown as
//! unused.

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

fn open(data: &Path) -> (Store, codemap_mcp::Project) {
    let root = fixture("api-tables/frontend");
    let project = codemap_mcp::Project {
        app: None,
        config: Config::load(&root).unwrap(),
        root,
        id: "front".into(),
        index_path: data.join("front").join("index.db"),
    };
    let mut store = Store::open(&project.index_path).unwrap();
    codemap_mcp::refresh(
        &mut store,
        &project.root,
        &project.config,
        ScanOptions::default(),
    )
    .unwrap();
    (store, project)
}

fn call(store: &Store, project: &codemap_mcp::Project, tool: &str, args: Value) -> Value {
    codemap_mcp::call(
        store,
        project,
        tool,
        args,
        codemap_mcp::Freshness::default(),
    )
    .unwrap()
    .data
}

/// The row of the call at `line` of the file an answer lists.
fn row(calls: &Value, line: u32) -> &Value {
    calls
        .as_array()
        .unwrap()
        .iter()
        .find(|c| {
            c["at"]
                .as_str()
                .is_some_and(|at| at.ends_with(&format!(":{line}")))
        })
        .unwrap_or_else(|| panic!("no call at line {line}: {calls}"))
}

fn strings(v: &Value) -> Vec<&str> {
    v.as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

#[test]
fn a_url_read_from_a_table_reaches_each_route_it_names() {
    let data = tempfile::tempdir().unwrap();
    let (store, project) = open(data.path());
    let out = call(
        &store,
        &project,
        "api_usage",
        json!({ "file": "src/modules/permits.ts" }),
    );
    let calls = &out["calls"];
    // `permitContracts[kind].endpoint`: every definition's value.
    let list = row(calls, 29);
    assert_eq!(list["status"], "matched", "{list}");
    assert_eq!(list["call"], "GET `permitContracts[kind].endpoint`");
    assert_eq!(
        strings(&list["routes"]),
        [
            "GET|HEAD api/permits/moving",
            "GET|HEAD api/permits/delivery",
            "GET|HEAD api/permits/renovation"
        ],
        "{list}"
    );
    // With an id behind it (the call was left out altogether: `id` is a
    // parameter, so the URL was left to callers that cannot fill it in).
    let get = row(calls, 32);
    assert_eq!(
        strings(&get["routes"]),
        [
            "GET|HEAD api/permits/moving/{permit}",
            "GET|HEAD api/permits/delivery/{permit}",
            "GET|HEAD api/permits/renovation/{permit}"
        ],
        "{get}"
    );
    // Through a constant of the function holding one of the definitions.
    // One of the three has no DELETE route: the call may never use it, so
    // that is not reported as a missing route.
    let remove = row(calls, 36);
    assert_eq!(remove["status"], "matched", "{remove}");
    assert_eq!(
        strings(&remove["routes"]),
        [
            "DELETE api/permits/moving/{permit}",
            "DELETE api/permits/renovation/{permit}"
        ],
        "{remove}"
    );
    assert!(remove.get("variants_without_route").is_none(), "{remove}");
    // A default: `endpoint || permitContracts[kind].endpoint`.
    let save = row(calls, 39);
    assert_eq!(save["status"], "matched", "{save}");
    assert_eq!(strings(&save["routes"]).len(), 3, "{save}");
    // One value: an ordinary call.
    let approvers = row(calls, 42);
    assert_eq!(approvers["call"], "GET /setup/approvers");
    assert_eq!(approvers["route"], "GET|HEAD api/setup/approvers");
    // What a helper of the file returns, its own constant included.
    let extension = row(calls, 45);
    assert_eq!(extension["status"], "matched", "{extension}");
    assert_eq!(
        strings(&extension["routes"]),
        [
            "GET|HEAD api/permits/renovation/{permit}/extension/{extension}",
            "GET|HEAD api/permits/renovation/{permit}/extension"
        ],
        "{extension}"
    );
    // A URL builder kept in an object.
    let visitor = row(calls, 48);
    assert_eq!(visitor["status"], "matched", "{visitor}");
    assert_eq!(
        visitor["route"], "GET|HEAD api/visitors/{visitor}",
        "{visitor}"
    );
    // A constant of the function: both of its values are URLs of the call.
    let save_visitor = row(calls, 52);
    assert_eq!(save_visitor["status"], "matched", "{save_visitor}");
    assert_eq!(
        save_visitor["call"], "POST|PATCH /visitors",
        "{save_visitor}"
    );
    assert!(
        save_visitor.get("variants_without_route").is_none(),
        "{save_visitor}"
    );
    // Nothing here is a problem.
    let summary = out["summary"].as_object().unwrap();
    assert_eq!(summary["matched"], 8, "{summary:?}");
    assert_eq!(summary.len(), 1, "{summary:?}");
}

#[test]
fn a_route_lists_the_calls_that_reach_it_through_a_table() {
    let data = tempfile::tempdir().unwrap();
    let (store, project) = open(data.path());
    let out = call(
        &store,
        &project,
        "api_usage",
        json!({ "endpoint": "GET api/permits/delivery/{permit}" }),
    );
    assert_eq!(out["found"], true, "{out}");
    let callers = out["endpoints"][0]["callers"].as_array().unwrap();
    assert_eq!(callers.len(), 1, "{out}");
    assert_eq!(callers[0]["at"], "src/modules/permits.ts:32");
    // The delivery permit cannot be deleted, and nothing says it can.
    let out = call(
        &store,
        &project,
        "api_usage",
        json!({ "endpoint": "DELETE api/permits/moving/{permit}" }),
    );
    let callers = out["endpoints"][0]["callers"].as_array().unwrap();
    assert_eq!(callers[0]["at"], "src/modules/permits.ts:36", "{out}");
}

#[test]
fn a_url_handed_in_is_a_possible_caller_of_what_the_file_defines() {
    let data = tempfile::tempdir().unwrap();
    let (store, project) = open(data.path());
    let out = call(
        &store,
        &project,
        "api_usage",
        json!({ "file": "src/modules/reports.ts" }),
    );
    let calls = &out["calls"];
    // `contract.endpoint`, `contract` a parameter: one of the `endpoint`
    // values of the file, which one is not known here.
    let run = row(calls, 13);
    assert_eq!(run["status"], "unresolved", "{run}");
    assert_eq!(run["call"], "GET `contract.endpoint`");
    assert_eq!(
        strings(&run["could_be"]),
        ["GET|HEAD api/reports/sales", "GET|HEAD api/reports/stock"],
        "{run}"
    );
    assert!(
        run["why"]
            .as_str()
            .unwrap()
            .contains("any of the 2 URL(s) this file keeps under that property"),
        "{run}"
    );
    // With an id behind it: only the definition that has such a route.
    let detail = row(calls, 16);
    assert_eq!(detail["status"], "unresolved", "{detail}");
    assert_eq!(
        strings(&detail["could_be"]),
        ["GET|HEAD api/reports/sales/{report}"],
        "{detail}"
    );
    // No object of the file has a `path`: still a call, endpoint unknown.
    let open_call = row(calls, 19);
    assert_eq!(open_call["status"], "unresolved", "{open_call}");
    assert!(open_call.get("could_be").is_none(), "{open_call}");
    assert_eq!(out["summary"]["unresolved"], 3, "{out}");

    // The route's side: not a caller for sure, but named.
    let out = call(
        &store,
        &project,
        "api_usage",
        json!({ "endpoint": "GET api/reports/stock" }),
    );
    let endpoint = &out["endpoints"][0];
    assert_eq!(endpoint["callers"].as_array().unwrap().len(), 0, "{out}");
    let possible = endpoint["possible_callers"].as_array().unwrap();
    assert_eq!(possible.len(), 1, "{out}");
    assert_eq!(possible[0]["at"], "src/modules/reports.ts:13");
    assert_eq!(possible[0]["call"], "api.get contract.endpoint");
}
