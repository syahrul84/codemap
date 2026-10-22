import SwiftUI

/// The tabs.
struct RootView: View {
    @State private var tab = 0
    var body: some View {
        TabView(selection: $tab) {
            ItemListView().tabItem { Label("Items", systemImage: "list.bullet") }
            SettingsView().tabItem { Label("Settings", systemImage: "gear") }
        }
    }
}

struct ItemListView: View {
    @State private var items: [Item] = []
    @State private var showAdd = false
    var body: some View {
        List(items) { item in
            NavigationLink(destination: ItemDetailView(item: item)) { ItemRow(item: item) }
        }
        .navigationTitle("Items")
        .sheet(isPresented: $showAdd) { AddItemView() }
        .toolbar { Button("Add") { showAdd = true } }
    }
}

struct ItemRow: View {
    let item: Item
    var body: some View { Text(item.name) }
}

struct ItemDetailView: View {
    let item: Item
    var body: some View { Text(NSLocalizedString("item.detail", comment: "")) }
}

struct AddItemView: View {
    @Environment(\.dismiss) private var dismiss
    var body: some View { Button("Save") { dismiss() } }
}

struct SettingsView: View {
    @AppStorage("useMetric") private var useMetric = true
    var body: some View { Toggle("Metric", isOn: $useMetric) }
}

struct UnusedView: View {
    var body: some View { Text("nobody shows me") }
}
