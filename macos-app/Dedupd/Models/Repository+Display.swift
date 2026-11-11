import DedupdClient
import Foundation

/// How a repository's files compare with what Hugging Face lists.
enum RepositoryHealth: Equatable {
    /// Every app file was checked and matches.
    case verified
    /// At least one file differs from what upstream lists.
    case different
    /// Some files are not listed upstream or were not checked.
    case unconfirmed
    /// No app holds the model; it is in the store only.
    case storedOnly

    var symbol: String {
        switch self {
        case .verified: "checkmark.seal.fill"
        case .different: "exclamationmark.triangle.fill"
        case .unconfirmed: "questionmark.circle"
        case .storedOnly: "archivebox"
        }
    }

    var label: String {
        switch self {
        case .verified: "Verified against Hugging Face"
        case .different: "Differs from Hugging Face"
        case .unconfirmed: "Not fully checked"
        case .storedOnly: "In the store only"
        }
    }
}

extension Repository {
    /// The first twelve characters of the commit `main` points at.
    var shortCommit: String? {
        commit.map { String($0.prefix(12)) }
    }

    var health: RepositoryHealth {
        if mismatch > 0 {
            return .different
        }
        if files == 0 {
            return .storedOnly
        }
        if notUpstream > 0 || unchecked > 0 {
            return .unconfirmed
        }
        return .verified
    }

    /// "13 stored · 13 in apps · main@6067b15cf581", as the command line
    /// writes a repository, with the parts that are zero left out.
    var detail: String {
        var parts = ["\(storedFiles) stored", "\(files) in apps"]
        if let shortCommit {
            parts.append("main@\(shortCommit)")
        }
        let notes: [(UInt64, String)] = [
            (mismatch, "different from upstream"),
            (notUpstream, "not listed upstream"),
            (unchecked, "not checked"),
        ]
        for (count, label) in notes where count > 0 {
            parts.append("\(count) \(label)")
        }
        return parts.joined(separator: " · ")
    }
}

extension PullResult {
    /// "4 files, 106.07 MB. Downloaded 1; 3 were already in the store."
    var summary: String {
        let already = files >= downloaded ? files - downloaded : 0
        let source = fromArchive ? " From the archive: Hugging Face could not be asked." : ""
        return "\(ByteFormat.files(files)), \(ByteFormat.string(bytes)). "
            + "Downloaded \(downloaded); \(already) already in the store.\(source)"
    }

    /// One line per app the model was placed into.
    var placements: [String] {
        placed.map { app in
            "\(ModelApp.title(for: app.app)): \(app.cloned) placed as clones, \(app.present) already there"
        }
    }

    /// Notes from the daemon and files it left alone because the app already
    /// had a different file at that place.
    var warnings: [String] {
        notes + placed.flatMap { app in
            app.conflicts.map { "\(ModelApp.title(for: app.app)): left alone \($0)" }
        }
    }
}

extension ScanResult {
    /// What a scan changed, or that it changed nothing.
    var summary: String {
        var parts: [String] = []
        if adopted > 0 {
            parts.append("adopted \(ByteFormat.files(adopted)) (\(ByteFormat.string(adoptedBytes)))")
        }
        if reclaimed > 0 {
            parts.append("freed \(ByteFormat.string(reclaimedBytes)) by sharing \(ByteFormat.files(reclaimed))")
        }
        if archived > 0 {
            parts.append("copied \(ByteFormat.files(archived)) to the archive")
        }
        if forgotten > 0 {
            parts.append("forgot \(ByteFormat.files(forgotten)) no longer there")
        }
        if parts.isEmpty {
            return "Nothing new in the app folders."
        }
        let sentence = parts.joined(separator: ", ")
        return sentence.prefix(1).uppercased() + sentence.dropFirst() + "."
    }
}
