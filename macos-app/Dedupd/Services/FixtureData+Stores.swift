import DedupdClient
import Foundation

/// The canned daemon's stores: the one in dedupd's home, which serves every
/// app folder, and a second on another disk that serves none yet. The rules
/// for a list the owner sends are the daemon's (`Config::set_stores` in the
/// core), so the screens meet the same answers and refusals.
extension FixtureData {
    static let primaryStore = "Internal"

    static let stores: [StoreSetting] = [
        StoreSetting(
            name: primaryStore, path: "/Users/example/.dedupd", primary: true, attached: true,
            apps: [.bionic, .ollama, .huggingFace], total: 994_660_000_000, free: 412_300_000_000
        ),
        StoreSetting(
            name: "Studio", path: "/Volumes/Studio/dedupd", primary: false, attached: true,
            apps: [], total: 1_999_000_000_000, free: 1_841_000_000_000
        ),
    ]

    /// What activating a variant copies between stores: nothing, because
    /// every canned app folder is on the disk of the store that holds the
    /// library.
    static let copies: [StoreCopy] = ModelApp.allCases.map {
        StoreCopy(app: $0, store: primaryStore, files: 0, bytes: 0)
    }

    /// The list the daemon keeps from what the owner sent: a picked folder
    /// gets a `dedupd` folder, an empty name the disk's name; a folder that
    /// is not a full path, inside dedupd's home, or listed twice, and a
    /// name used twice or the primary's, are refused. The disk checks
    /// (APFS, one store per disk) are the daemon's and are not imitated:
    /// every canned disk passes them.
    static func stores(from entries: [ArchiveEntry], known: [StoreSetting]) throws -> [StoreSetting] {
        guard let primary = known.first(where: \.primary) else {
            return known
        }
        var list = [primary]
        for entry in entries {
            guard entry.path.hasPrefix("/") else {
                throw ClientError.Failed(message: "a store's folder must be a full path, not \(entry.path)")
            }
            let isKnown = known.contains { $0.path == entry.path }
            let path = isKnown ? entry.path : archiveFolder(for: entry.path)
            let trimmed = entry.name.trimmingCharacters(in: .whitespaces)
            let name = trimmed.isEmpty ? defaultArchiveName(path) : trimmed
            if list.contains(where: { $0.name.caseInsensitiveCompare(name) == .orderedSame }) {
                throw ClientError.Failed(message: "two stores are named \(name); give each its own name")
            }
            if list.contains(where: { $0.path == path }) {
                throw ClientError.Failed(message: "\(path) is listed twice")
            }
            if path.hasPrefix(primary.path) {
                throw ClientError.Failed(message: "\(path) is dedupd's own folder, which has its store already")
            }
            let old = known.first { $0.path == path }
            list.append(StoreSetting(
                name: name, path: path, primary: false, attached: old?.attached ?? true, apps: old?.apps ?? [],
                total: old?.total ?? 1_999_000_000_000, free: old?.free ?? 1_841_000_000_000
            ))
        }
        return list
    }

    /// Each store but the primary as a location: the canned library is in
    /// the primary store, so the others hold nothing.
    static func storeLocations(_ stores: [StoreSetting]) -> [Location] {
        stores.filter { !$0.primary }.map { store in
            Location(
                kind: .store, app: nil, name: store.name, backup: false, path: store.path + "/store",
                present: store.attached, files: 0, bytes: 0, sharedBytes: 0, models: 0
            )
        }
    }
}
