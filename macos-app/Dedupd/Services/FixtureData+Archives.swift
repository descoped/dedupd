import DedupdClient
import Foundation

/// The canned daemon's archives: the backup archive, which holds a copy of
/// every stored file, and a second archive that holds only what the owner
/// archives to it. The rules for a list the owner sends are the daemon's
/// (`Config::set_archives` in the core), so the screens meet the same
/// answers and refusals.
extension FixtureData {
    static let archives: [ArchiveSetting] = [
        ArchiveSetting(
            name: "Archive", path: archiveFolder, backup: true, attached: true, sameDiskAsStore: false
        ),
        ArchiveSetting(
            name: "Drawer", path: "/Volumes/Drawer/dedupd", backup: false, attached: true, sameDiskAsStore: false
        ),
    ]

    /// The attached archives that hold every file of a model: the backup
    /// holds every model, another archive those archived to it.
    static func holders(of repo: String, archives: [ArchiveSetting], archivedTo: [String: String]) -> [String] {
        archives.filter { $0.attached && ($0.backup || archivedTo[repo] == $0.name) }.map(\.name)
    }

    /// The archive a model goes to: the one named, or the backup, or the
    /// only one; refused as the daemon refuses it.
    static func archiveTo(_ name: String?, in archives: [ArchiveSetting]) throws -> ArchiveSetting {
        let names = archives.map(\.name).joined(separator: ", ")
        let chosen: ArchiveSetting? = if let name {
            archives.first { $0.name.caseInsensitiveCompare(name) == .orderedSame }
        } else {
            archives.first(where: \.backup) ?? (archives.count == 1 ? archives.first : nil)
        }
        guard let chosen else {
            if archives.isEmpty {
                throw ClientError.Failed(message: "no archive is set; add one under Settings, Storage")
            }
            if let name {
                throw ClientError.Failed(message: "there is no archive named \(name); the archives are \(names)")
            }
            throw ClientError.Failed(message: "say which archive to use: \(names)")
        }
        guard chosen.attached else {
            throw ClientError.Failed(
                message: "the disk of the archive \(chosen.name) is not attached (\(chosen.path) not found)"
            )
        }
        return chosen
    }

    /// The list the daemon keeps from what the owner sent: a disk's top
    /// folder gets a `dedupd` folder, an empty name the disk's name, and
    /// two of one name or one folder are refused.
    static func archives(
        from entries: [ArchiveEntry], backup: String?, known: [ArchiveSetting]
    ) throws -> [ArchiveSetting] {
        var list: [ArchiveSetting] = []
        for entry in entries {
            guard entry.path.hasPrefix("/") else {
                throw ClientError.Failed(message: "an archive's folder must be a full path, not \(entry.path)")
            }
            let path = archiveFolder(for: entry.path)
            let trimmed = entry.name.trimmingCharacters(in: .whitespaces)
            let name = trimmed.isEmpty ? defaultArchiveName(path) : trimmed
            if list.contains(where: { $0.name.caseInsensitiveCompare(name) == .orderedSame }) {
                throw ClientError.Failed(message: "two archives are named \(name); give each its own name")
            }
            if list.contains(where: { $0.path == path }) {
                throw ClientError.Failed(message: "\(path) is listed twice")
            }
            let attached = known.first { $0.path == path }?.attached ?? true
            list.append(ArchiveSetting(name: name, path: path, backup: false, attached: attached, sameDiskAsStore: false))
        }
        if let backup {
            let folder = archiveFolder(for: backup)
            guard let index = list.firstIndex(where: { $0.path == folder }) else {
                throw ClientError.Failed(message: "the backup archive \(folder) is not one of the archives")
            }
            list[index].backup = true
            // The daemon lists the backup archive first.
            list.insert(list.remove(at: index), at: 0)
        }
        return list
    }

    /// `archive_folder_for` in the core.
    static func archiveFolder(for picked: String) -> String {
        let url = URL(fileURLWithPath: picked)
        return url.lastPathComponent == "dedupd" ? url.path : url.appendingPathComponent("dedupd").path
    }

    /// `default_archive_name` in the core.
    static func defaultArchiveName(_ path: String) -> String {
        let parts = URL(fileURLWithPath: path).pathComponents
        if parts.count > 2, parts[1] == "Volumes" {
            return parts[2]
        }
        let url = URL(fileURLWithPath: path)
        let named = url.lastPathComponent == "dedupd" ? url.deletingLastPathComponent() : url
        return named.lastPathComponent.isEmpty ? "Archive" : named.lastPathComponent
    }
}
