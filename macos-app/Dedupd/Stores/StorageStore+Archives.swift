import DedupdClient
import Foundation

/// The archives: cold locations the owner adds, names and removes. Each
/// change sends the whole list to the daemon, which writes it to its config
/// file and uses it at once. Removing an archive deletes nothing on its disk.
extension StorageStore {
    /// The archives as the daemon has them, the backup archive first.
    var archives: [ArchiveSetting] { daemonStatus?.archives ?? [] }

    /// The archive that gets a copy of every download, if any.
    var backupArchive: ArchiveSetting? { archives.first(where: \.backup) }

    /// Add the folder the owner picked as an archive. An empty name gives
    /// the disk's name. Returns why it was refused, or nil.
    func addArchive(folder: String, name: String, backup: Bool) async -> String? {
        var list = archives.map(Self.entry)
        list.append(ArchiveEntry(name: name, path: folder))
        return await send(list, backup: backup ? folder : backupArchive?.path)
    }

    /// Give an archive another name. Returns why it was refused, or nil.
    func rename(_ archive: ArchiveSetting, to name: String) async -> String? {
        let list = archives.map { $0.path == archive.path ? ArchiveEntry(name: name, path: $0.path) : Self.entry($0) }
        return await send(list, backup: backupArchive?.path)
    }

    /// Make this archive the backup, or with nil have no backup.
    func useAsBackup(_ archive: ArchiveSetting?) async -> String? {
        await send(archives.map(Self.entry), backup: archive?.path)
    }

    /// Stop using an archive. Its files stay on its disk.
    func remove(_ archive: ArchiveSetting) async -> String? {
        let list = archives.filter { $0.path != archive.path }.map(Self.entry)
        let backup = backupArchive.flatMap { $0.path == archive.path ? nil : $0.path }
        return await send(list, backup: backup)
    }

    private static func entry(_ archive: ArchiveSetting) -> ArchiveEntry {
        ArchiveEntry(name: archive.name, path: archive.path)
    }

    private func send(_ list: [ArchiveEntry], backup: String?) async -> String? {
        do {
            _ = try await daemonClient.setArchives(list, backup: backup, at: daemonAddress)
        } catch {
            return describe(error)
        }
        await refreshAfterArchives()
        return nil
    }
}
