use demo_tool::{Item, Store};

#[test]
fn adds_items() {
    let mut s = Store::open();
    assert!(s.add(Item { id: 1, name: "a".into() }));
}
