import DedupdClient
import Foundation

/// What the Storage screen and the Library say about the stores: the warm
/// locations, one on each disk that holds app folders.
enum StoreWords {
    /// A store as a choice for Restore: "Envoy (1.84 TB free)".
    static func restoreChoice(_ store: StoreSetting) -> String {
        "\(store.name) (\(ByteFormat.string(store.free)) free)"
    }

    /// What removing a store does, before the owner agrees.
    static func removeMessage(_ store: StoreSetting) -> String {
        "dedupd stops using “\(store.name)”. Nothing on its disk is deleted: add the folder again to use "
            + "what it holds. Meanwhile the apps on that disk cannot be given models, and a model whose "
            + "only copy is there counts as not stored."
    }

    /// The name a picked folder gets when the owner leaves it empty: the
    /// disk's name, as the daemon gives it.
    static func suggestedName(for folder: URL) -> String {
        ArchiveWords.suggestedName(for: folder)
    }
}

extension StoreSetting: @retroactive Identifiable {
    public var id: String { path }

    /// One line under its name: what it serves, or why it does not.
    var note: String {
        if !attached {
            return "The disk is not attached. The apps on it cannot be given models until it is."
        }
        if primary {
            return apps.isEmpty
                ? "In dedupd's home. Downloads go here when no other store serves the apps."
                : "In dedupd's home. Serves \(Self.names(apps)); downloads go here unless another store serves the apps."
        }
        return apps.isEmpty
            ? "Serves no app yet: no app keeps its models on this disk."
            : "Serves \(Self.names(apps)), whose folders are on this disk; their downloads go here."
    }

    /// What it holds, from the places the daemon counted: "3 models, 16.08 GB".
    func contents(in locations: Locations?) -> String? {
        guard attached, let locations else {
            return nil
        }
        let place = primary ? locations.store : locations.stores.first { $0.name == name }
        guard let place else {
            return nil
        }
        let models = place.models == 1 ? "1 model" : "\(place.models) models"
        return "\(models), \(ByteFormat.string(place.bytes))"
    }

    private static func names(_ apps: [ModelApp]) -> String {
        ListFormatter.localizedString(byJoining: apps.map(\.title))
    }
}
