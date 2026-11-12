import DedupdClient
import Foundation

/// The sentences about archiving a model and bringing it back. Kept out of
/// the views so they can be tested.
enum ArchiveWords {
    /// Why Archive can or cannot be used now.
    static func archiveHelp(_ status: DaemonStatus?) -> String {
        guard let folder = status?.archivePath else {
            return "Set an archive folder on the Daemon screen first"
        }
        guard status?.archiveAttached == true else {
            return "The archive disk is not attached (\(folder))"
        }
        return "Take the model out of every app and off the internal disk. The copy on \(folder) stays."
    }

    /// Why Restore can or cannot be used now.
    static func restoreHelp(_ status: DaemonStatus?) -> String {
        guard let folder = status?.archivePath, status?.archiveAttached == true else {
            return "The archive disk is not attached"
        }
        return "Copy the model back from \(folder). Activate it in an app afterwards."
    }
}

extension LibraryModel {
    /// "13 files, 16.08 GB", and where they are when that is not this Mac.
    var storedText: String {
        let size = "\(ByteFormat.files(storedFiles)), \(ByteFormat.string(storedBytes))"
        return isArchived ? "\(size), on the archive disk only" : size
    }
}

extension ArchivePlan {
    /// The apps the model is taken out of, as "Bionic and Ollama".
    var appList: String {
        ListFormatter.localizedString(byJoining: deactivate.map { ModelApp.shortTitle(for: $0.app) })
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
                "\(ByteFormat.files(copyFiles)), \(ByteFormat.string(copyBytes)), are copied to the archive disk first, "
                    + "because it does not have them yet."
            )
        }
        steps.append("Every copy on the archive disk is read in full and checked. Nothing is removed unless all are good.")
        if !deactivate.isEmpty {
            steps.append("The model is taken out of \(appList).")
        }
        steps.append(
            "\(ByteFormat.files(files)), \(ByteFormat.string(bytes)), are removed from the internal disk. "
                + "The copy in \(archive) stays, and Restore brings the model back."
        )
        return steps
    }
}

extension ModelMove {
    var isArchive: Bool { kind == "archive" }

    /// How far the move is, from 0 to 1; nil when there is nothing to count.
    var fraction: Double? {
        guard running, bytesTotal > 0 else {
            return nil
        }
        return min(1, Double(bytesDone) / Double(bytesTotal))
    }

    /// One line on the move: how far it is, or why it failed.
    var statusText: String {
        if let error {
            return "\(isArchive ? "Archiving" : "Restoring") failed: \(error)"
        }
        let progress = "\(ByteFormat.string(bytesDone)) of \(ByteFormat.string(bytesTotal))"
        if running {
            return isArchive
                ? "Archiving: \(progress) checked on the archive disk"
                : "Restoring: \(progress) copied back"
        }
        return isArchive
            ? "Archived: \(ByteFormat.files(files)), \(ByteFormat.string(bytesDone)), left the internal disk"
            : "Restored: \(ByteFormat.files(files)), \(ByteFormat.string(bytesDone))"
    }
}
