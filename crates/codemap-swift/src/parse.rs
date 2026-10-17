//! A tree-sitter parser per thread, so files can be parsed in parallel.

use std::cell::RefCell;

use tree_sitter::{Parser, Tree};

thread_local! {
    static PARSER: RefCell<Parser> = RefCell::new({
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_swift::LANGUAGE.into())
            .expect("tree-sitter-swift grammar matches the tree-sitter version");
        parser
    });
}

/// Parses Swift source. `None` only if parsing was cancelled.
pub fn parse(src: &[u8]) -> Option<Tree> {
    PARSER.with(|p| p.borrow_mut().parse(src, None))
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore]
    fn dump() {
        let src = r#"import SwiftUI

@main
struct DemoApp: App {
    var body: some Scene {
        WindowGroup { ContentView() }
    }
}

protocol Store { func load() async throws -> [Item] }

struct ContentView: View {
    @State private var items: [Item] = []
    @Binding var selected: Item?
    let title: String
    var body: some View {
        NavigationStack {
            List(items) { item in
                NavigationLink(destination: DetailView(item: item)) { Text(item.name) }
            }
            .sheet(isPresented: $show) { SettingsView() }
            Text("welcome.title")
            Text(NSLocalizedString("greeting", comment: ""))
        }
        .task { items = try await api.load() }
    }
}

final class ItemStore: ObservableObject, Store {
    static let shared = ItemStore()
    func load() async throws -> [Item] { Formatter.format(1); return [] }
    init(x: Int = 1) {}
}

extension ItemStore { func reset() {} }

enum Kind: String { case a, b(Int) }
typealias ID = String
func helper(_ a: Int, label b: String) -> Bool { true }
"#;
        let tree = super::parse(src.as_bytes()).unwrap();
        println!("{}", tree.root_node().to_sexp());
    }
}
