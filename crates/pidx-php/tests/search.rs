//! search_symbol behaviour on the Laravel 10 fixture.

mod common;

use pidx_core::Store;
use pidx_core::envelope::Detail;
use pidx_core::symbols::{SearchInput, SymbolKind, search};
use serde_json::Value;

fn store() -> Store {
    common::indexed(&common::fixture("laravel10-legacy")).0
}

fn run(store: &Store, input: SearchInput) -> (Value, Option<String>) {
    let r = search(store.conn(), &input).unwrap();
    (r.data, r.next_cursor)
}

fn q(query: &str) -> SearchInput {
    SearchInput {
        query: query.into(),
        ..SearchInput::default()
    }
}

fn first_fqn(data: &Value) -> &str {
    assert_eq!(data["found"], true, "{data:#}");
    data["matches"][0]["fqn"].as_str().unwrap()
}

#[test]
fn by_short_name_fqn_and_member() {
    let s = store();
    assert_eq!(
        first_fqn(&run(&s, q("OrderService")).0),
        "App\\Services\\OrderService"
    );
    assert_eq!(
        first_fqn(&run(&s, q("orderservice")).0),
        "App\\Services\\OrderService"
    );
    assert_eq!(
        first_fqn(&run(&s, q("\\App\\Services\\OrderService")).0),
        "App\\Services\\OrderService"
    );
    assert_eq!(
        first_fqn(&run(&s, q("Services\\OrderService")).0),
        "App\\Services\\OrderService"
    );
    assert_eq!(
        first_fqn(&run(&s, q("OrderController::markPaid")).0),
        "App\\Http\\Controllers\\OrderController::markPaid"
    );
    assert_eq!(
        first_fqn(&run(&s, q("OrderService::markPaid")).0),
        "App\\Services\\OrderService::markPaid"
    );
    assert_eq!(first_fqn(&run(&s, q("money")).0), "money");
}

#[test]
fn partial_and_short_queries() {
    let s = store();
    let (data, _) = run(&s, q("Service"));
    let fqns: Vec<&str> = data["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["fqn"].as_str().unwrap())
        .collect();
    assert_eq!(fqns[0], "App\\Services\\OrderService", "{fqns:?}");
    assert!(fqns.contains(&"App\\Providers\\RouteServiceProvider"));

    // Under 3 characters falls back to LIKE.
    let (data, _) = run(&s, q("Or"));
    assert_eq!(data["found"], true);
}

#[test]
fn doc_summary_matches_rank_last() {
    let s = store();
    let (data, _) = run(&s, q("transaction"));
    assert_eq!(first_fqn(&data), "App\\Services\\OrderService::place");
}

#[test]
fn kind_and_namespace_filters() {
    let s = store();
    let (data, _) = run(
        &s,
        SearchInput {
            kind: Some(SymbolKind::Class),
            ..q("Order")
        },
    );
    assert!(
        data["matches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["kind"] == "class")
    );
    assert_eq!(first_fqn(&data), "App\\Models\\Order");

    let (data, _) = run(
        &s,
        SearchInput {
            namespace_prefix: Some("\\App\\Http\\".into()),
            ..q("Order")
        },
    );
    assert!(
        data["matches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["fqn"].as_str().unwrap().starts_with("App\\Http\\"))
    );
    assert_eq!(first_fqn(&data), "App\\Http\\Controllers\\OrderController");
}

#[test]
fn typo_gives_suggestions() {
    let s = store();
    let (data, cursor) = run(&s, q("OrderServcie"));
    assert_eq!(data["found"], false);
    assert_eq!(data["query"], "OrderServcie");
    assert_eq!(data["suggestions"][0]["fqn"], "App\\Services\\OrderService");
    assert_eq!(
        data["suggestions"][0]["file"],
        "app/Services/OrderService.php"
    );
    assert_eq!(data["hint"], "Did you mean OrderService?");
    assert!(cursor.is_none());

    let (data, _) = run(&s, q("Order::markPayd"));
    assert_eq!(data["found"], false);
    assert_eq!(data["suggestions"][0]["name"], "markPaid");

    let (data, _) = run(&s, q("Zzyzx"));
    assert_eq!(data["found"], false);
    assert_eq!(data["suggestions"].as_array().unwrap().len(), 0);
}

#[test]
fn pagination_has_no_duplicates() {
    let s = store();
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let (data, next) = run(
            &s,
            SearchInput {
                limit: Some(2),
                cursor: cursor.clone(),
                ..q("order")
            },
        );
        for m in data["matches"].as_array().unwrap() {
            seen.push(m["fqn"].as_str().unwrap().to_owned());
        }
        if next.is_none() {
            assert_eq!(seen.len(), data["total"].as_u64().unwrap() as usize);
            break;
        }
        cursor = next;
    }
    let mut dedup = seen.clone();
    dedup.sort();
    dedup.dedup();
    assert_eq!(dedup.len(), seen.len());
    assert!(seen.len() > 10);
}

#[test]
fn detail_levels() {
    let s = store();
    let (brief, _) = run(&s, q("StoreOrderRequest"));
    let m = &brief["matches"][0];
    assert!(m.get("meta").is_none() && m.get("score").is_none());
    assert_eq!(
        m.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["name", "fqn", "kind", "signature", "file", "line"]
    );

    let (full, _) = run(
        &s,
        SearchInput {
            detail: Detail::Full,
            ..q("StoreOrderRequest")
        },
    );
    let m = &full["matches"][0];
    assert_eq!(
        m["meta"]["extends"][0],
        "Illuminate\\Foundation\\Http\\FormRequest"
    );
    assert_eq!(m["score"], 1.0);
    assert_eq!(m["end_line"], 25);
}
