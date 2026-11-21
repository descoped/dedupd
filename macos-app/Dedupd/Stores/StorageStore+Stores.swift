import DedupdClient
import Foundation

/// The stores: warm locations on the disks that hold app folders. The one
/// in dedupd's home is always there; the owner adds, names and removes the
/// others. Each change sends the whole list to the daemon, which checks the
/// disk (APFS, no store on it yet), writes its config file and uses it at
/// once. Removing a store deletes nothing on its disk.
extension StorageStore {
    /// The stores as the daemon has them, the one in dedupd's home first.
    var stores: [StoreSetting] { daemonStatus?.stores ?? [] }

    /// Add the folder the owner picked as a store. An empty name gives the
    /// disk's name. Returns why it was refused, or nil.
    func addStore(folder: String, name: String) async -> String? {
        var list = others.map(Self.entry)
        list.append(ArchiveEntry(name: name, path: folder))
        return await sendStores(list)
    }

    /// Give a store another name. Returns why it was refused, or nil.
    func rename(_ store: StoreSetting, to name: String) async -> String? {
        let list = others.map { $0.path == store.path ? ArchiveEntry(name: name, path: $0.path) : Self.entry($0) }
        return await sendStores(list)
    }

    /// Stop using a store. Its files stay on its disk.
    func remove(_ store: StoreSetting) async -> String? {
        await sendStores(others.filter { $0.path != store.path }.map(Self.entry))
    }

    /// The stores the owner can change: all but the one in dedupd's home.
    private var others: [StoreSetting] { stores.filter { !$0.primary } }

    private static func entry(_ store: StoreSetting) -> ArchiveEntry {
        ArchiveEntry(name: store.name, path: store.path)
    }

    private func sendStores(_ list: [ArchiveEntry]) async -> String? {
        do {
            _ = try await daemonClient.setStores(list, at: daemonAddress)
        } catch {
            return describe(error)
        }
        await refreshAfterArchives()
        return nil
    }
}
