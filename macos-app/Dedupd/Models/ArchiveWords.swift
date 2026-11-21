import DedupdClient
import Foundation

/// The sentences about archiving a model and bringing it back. Kept out of
/// the views so they can be tested.
enum ArchiveWords {
    /// Whether an archive is attached, so Archive and Restore can be used.
    static func canMove(_ status: DaemonStatus?) -> Bool {
        status?.archives.contains(where: \.attached) == true
    }

    /// Why Archive can or cannot be used now.
    static func archiveHelp(_ status: DaemonStatus?) -> String {
        let archives = status?.archives ?? []
        if archives.isEmpty {
            return "Add an archive on the Storage screen first"
        }
        if !canMove(status) {
            return "No archive's disk is attached (\(names(archives)))"
        }
        return "Take the model out of every app and off this Mac. You choose the archive; its copy stays."
    }

    /// Why Restore can or cannot be used now.
    static func restoreHelp(_ status: DaemonStatus?) -> String {
        let archives = status?.archives ?? []
        guard canMove(status) else {
            return archives.isEmpty
                ? "No archive is set"
                : "No archive's disk is attached (\(names(archives)))"
        }
        return "Copy the model back from the archive that has it. Activate it in an app afterwards."
    }

    /// The menu bar panel's tile: how many archives are attached.
    static func tileValue(_ archives: [ArchiveSetting]) -> String {
        let attached = archives.filter(\.attached).count
        switch archives.count {
        case 0: return "None"
        case 1: return attached == 1 ? "Attached" : "Detached"
        default: return "\(attached) of \(archives.count) attached"
        }
    }

    /// Under it: the backup archive, or that there is none.
    static func tileDetail(_ archives: [ArchiveSetting]) -> String {
        if archives.isEmpty {
            return "no second copy"
        }
        return archives.first(where: \.backup).map { "backup: \($0.name)" } ?? "no backup of every download"
    }

    private static func names(_ archives: [ArchiveSetting]) -> String {
        ListFormatter.localizedString(byJoining: archives.map(\.name))
    }
}

extension LibraryModel {
    /// "13 files, 16.08 GB", and where they are when that is not this Mac.
    var storedText: String {
        let size = "\(ByteFormat.files(storedFiles)), \(ByteFormat.string(storedBytes))"
        return isArchived ? "\(size), in an archive only" : size
    }
}

extension ArchivePlan {
    /// The apps the model is taken out of, as "Bionic and Ollama".
    var appList: String {
        ListFormatter.localizedString(byJoining: deactivate.map(\.app.shortTitle))
    }

    /// Nothing would change: the model is in no app and has no file on the
    /// internal disk that could go.
    var isEmpty: Bool {
        files == 0 && deactivate.allSatisfy { $0.files == 0 }
    }

    /// What archiving does, in the order it happens.
    var steps: [String] {
        var steps: [String] = []
        if copyFiles > 0 {
            steps.append(
                "\(ByteFormat.files(copyFiles)), \(ByteFormat.string(copyBytes)), are copied to \(archiveName) first, "
                    + "because it does not have them yet."
            )
        }
        steps.append("Every copy in \(archiveName) is read in full and checked. Nothing is removed unless all are good.")
        if !deactivate.isEmpty {
            steps.append("The model is taken out of \(appList).")
        }
        steps.append(
            "\(ByteFormat.files(files)), \(ByteFormat.string(bytes)), are removed from this Mac. "
                + "The copy in \(archiveName) (\(archive)) stays, and Restore brings the model back."
        )
        return steps
    }
}

extension ModelMove {
    var isArchive: Bool { kind == .archive }

    /// How far the move is, from 0 to 1; nil when there is nothing to count.
    var fraction: Double? {
        guard running, bytesTotal > 0 else {
            return nil
        }
        return min(1, Double(bytesDone) / Double(bytesTotal))
    }

    /// How fast it goes and how long it has left, once there is enough to
    /// tell: "120 MB/s · about 3 min left". The rate is the average since
    /// it started; one large file is counted only when it is done.
    func pace(at now: Date = Date()) -> String? {
        let elapsed = now.timeIntervalSince1970 - Double(startedAt)
        guard running, bytesDone > 0, bytesTotal > bytesDone, elapsed >= 2 else {
            return nil
        }
        let rate = Double(bytesDone) / elapsed
        let left = Double(bytesTotal - bytesDone) / rate
        return "\(ByteFormat.speed(UInt64(rate))) · about \(ByteFormat.duration(seconds: UInt64(left))) left"
    }

    /// One line on the move: how far it is, or why it failed.
    var statusText: String {
        if let error {
            return "\(isArchive ? "Archiving" : "Restoring") failed: \(error)"
        }
        let progress = "\(ByteFormat.string(bytesDone)) of \(ByteFormat.string(bytesTotal))"
        if running {
            return isArchive
                ? "Archiving to \(archive ?? "the archive"): \(progress) checked there"
                : "Restoring: \(progress) copied back"
        }
        return isArchive
            ? "Archived to \(archive ?? "the archive"): \(ByteFormat.files(files)), \(ByteFormat.string(bytesDone)), "
                + "left this Mac"
            : "Restored: \(ByteFormat.files(files)), \(ByteFormat.string(bytesDone))"
    }
}

extension ArchiveSetting: @retroactive Identifiable {
    public var id: String { path }

    /// One line on what the archive is, or what is wrong with it.
    var note: String {
        if !attached {
            return "The disk is not attached. Models kept only here cannot be restored until it is."
        }
        if sameDiskAsStore {
            return "On the same disk as the store: no protection if that disk fails."
        }
        return backup
            ? "Gets a copy of every download, and of the models you archive here."
            : "Holds the models you archive here."
    }

    /// What it holds, from the places the daemon counted: "3 models, 16.08 GB".
    func contents(in locations: Locations?) -> String? {
        guard attached, let place = locations?.archives.first(where: { $0.path == path }) else {
            return nil
        }
        let models = place.models == 1 ? "1 model" : "\(place.models) models"
        return "\(models), \(ByteFormat.string(place.bytes))"
    }
}

extension ArchiveWords {
    /// What removing an archive does, before the owner agrees.
    static func removeMessage(_ archive: ArchiveSetting) -> String {
        "dedupd stops using “\(archive.name)”. Nothing on its disk is deleted: add the folder again to use "
            + "what it holds. A model whose only copy is there cannot be restored meanwhile."
    }

    /// The name a picked folder gets unless the owner types another: the
    /// disk's name for a folder on an external disk, otherwise the folder's.
    static func suggestedName(for folder: URL) -> String {
        let parts = folder.pathComponents
        if parts.count > 2, parts[1] == "Volumes" {
            return parts[2]
        }
        return folder.lastPathComponent
    }
}
