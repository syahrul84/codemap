import Foundation

/// One thing in the list.
struct Item: Identifiable, Codable {
    let id: UUID
    var name: String
}

protocol ItemStore {
    func load() throws -> [Item]
    func save(_ items: [Item]) throws
}

final class FileItemStore: ItemStore {
    static let shared = FileItemStore()
    private let url = URL(fileURLWithPath: "items.json")
    func load() throws -> [Item] { try JSONDecoder().decode([Item].self, from: Data(contentsOf: url)) }
    func save(_ items: [Item]) throws { try JSONEncoder().encode(items).write(to: url) }
}

enum Filter: String, CaseIterable {
    case all, done
}
