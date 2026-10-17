//! The keys a call sends, checked against what the backend action accepts:
//! its Form Request rules, an inline `validate()`, or the fields it reads.

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

#[test]
fn sent_keys_against_rules_and_reads() {
    let data = tempfile::tempdir().unwrap();
    let root = fixture("api-payload/frontend");
    let project = codemap_mcp::Project {
        app: None,
        config: Config::load(&root).unwrap(),
        root,
        id: "front".into(),
        index_path: data.path().join("front").join("index.db"),
    };
    let mut store = Store::open(&project.index_path).unwrap();
    codemap_mcp::refresh(
        &mut store,
        &project.root,
        &project.config,
        ScanOptions::default(),
    )
    .unwrap();
    let out = codemap_mcp::call(
        &store,
        &project,
        "api_usage",
        json!({ "file": "composables/offers.ts", "detail": "full" }),
        codemap_mcp::Freshness::default(),
    )
    .unwrap()
    .data;
    let calls = out["calls"].as_array().unwrap();
    let at = |line: u32| {
        calls
            .iter()
            .find(|c| c["at"] == format!("composables/offers.ts:{line}"))
            .cloned()
            .unwrap_or(Value::Null)
    };
    for c in calls {
        assert_eq!(c["status"], "matched", "{c}");
    }

    // Form Request: `description` is required and never sent; `desc` has no rule.
    let create = at(3);
    assert_eq!(
        create["missing_required"],
        json!(["description"]),
        "{create}"
    );
    assert_eq!(create["unknown_field"], json!(["desc"]), "{create}");
    assert_eq!(
        create["possible_rename"],
        json!(["desc → description"]),
        "{create}"
    );
    let ev = &create["payload_evidence"];
    assert!(
        ev["accepts_from"]
            .as_str()
            .unwrap()
            .contains("StoreOfferRequest"),
        "{ev}"
    );
    assert!(
        ev["accepts_at"]
            .as_str()
            .unwrap()
            .contains("StoreOfferRequest.php:"),
        "{ev}"
    );

    // Inline validate(): everything sent is accepted.
    let rename = at(6);
    assert!(rename.get("missing_required").is_none(), "{rename}");
    assert!(rename.get("unknown_field").is_none(), "{rename}");

    // A spread payload is not guessed: it is reported as unchecked.
    let edit = at(9);
    assert!(edit.get("missing_required").is_none(), "{edit}");
    assert!(
        edit["payload_unchecked"].as_str().unwrap().contains("form"),
        "{edit}"
    );

    // Reads only: `tags` is never read.
    let note = at(12);
    assert_eq!(note["unknown_field"], json!(["tags"]), "{note}");
    // Rules from a helper: their own line, and where the helper is called.
    assert_eq!(
        note["payload_evidence"]["accepts_at"],
        "app/Http/Controllers/NoteController.php:28 (in project(), called at line 14)",
        "{note}"
    );

    // The request goes to other code: anything may be read there.
    let import = at(18);
    assert!(import.get("unknown_field").is_none(), "{import}");

    assert_eq!(out["summary"]["missing_required"], 1);
    assert_eq!(out["summary"]["unknown_field"], 2);
    assert_eq!(out["summary"]["possible_rename"], 1);

    // A GET's unread query parameter is only a note, and `page` is accepted
    // because the action paginates (a POST that does not, does not take it).
    let list = at(21);
    assert!(list.get("unknown_field").is_none(), "{list}");
    assert_eq!(out["summary"]["unknown_query_note"], 1);
    let all = codemap_mcp::call(
        &store,
        &project,
        "api_usage",
        json!({ "file": "composables/offers.ts", "status": "payload_all", "detail": "full" }),
        codemap_mcp::Freshness::default(),
    )
    .unwrap()
    .data;
    let list = all["calls"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["at"] == "composables/offers.ts:21")
        .cloned()
        .unwrap();
    assert_eq!(list["note_unknown_query"], json!(["filter"]), "{list}");
    let accepts = list["payload_evidence"]["accepts"].as_array().unwrap();
    assert!(accepts.iter().any(|a| a == "page"), "{list}");
    assert!(
        !note["payload_evidence"]["accepts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a == "page"),
        "{note}"
    );
}

#[test]
fn spoofed_methods_across_files() {
    let data = tempfile::tempdir().unwrap();
    let root = fixture("api-payload/frontend");
    let project = codemap_mcp::Project {
        app: None,
        config: Config::load(&root).unwrap(),
        root,
        id: "front".into(),
        index_path: data.path().join("front").join("index.db"),
    };
    let mut store = Store::open(&project.index_path).unwrap();
    codemap_mcp::refresh(
        &mut store,
        &project.root,
        &project.config,
        ScanOptions::default(),
    )
    .unwrap();
    let get = |file: &str| {
        codemap_mcp::call(
            &store,
            &project,
            "api_usage",
            json!({ "file": file, "detail": "full" }),
            codemap_mcp::Freshness::default(),
        )
        .unwrap()
        .data["calls"][0]
            .clone()
    };
    // The body is a parameter; a caller in another file passes `_method: 'PUT'`.
    let act = get("composables/invites.ts");
    assert_eq!(act["status"], "matched", "{act}");
    assert_eq!(act["call"], "POST|PUT /invites/{}/{}", "{act}");
    // FormData filled from an imported helper that returns `_method: 'PATCH'`.
    let quote = get("composables/quotes.ts");
    assert_eq!(quote["status"], "matched", "{quote}");
    assert_eq!(quote["route"], "PATCH api/v1/quotes/{quote}", "{quote}");
    // Callers build the body with an imported helper (inline or in a
    // variable), or the function spreads the caller's body into a new one.
    let all = codemap_mcp::call(
        &store,
        &project,
        "api_usage",
        json!({ "file": "composables/widgets.ts", "detail": "full" }),
        codemap_mcp::Freshness::default(),
    )
    .unwrap()
    .data;
    for c in all["calls"].as_array().unwrap() {
        assert_eq!(c["status"], "matched", "{c}");
        assert_eq!(
            c["call"].as_str().unwrap().split(' ').next(),
            Some("POST|PUT"),
            "{c}"
        );
    }
    assert_eq!(all["calls"].as_array().unwrap().len(), 2);
}
