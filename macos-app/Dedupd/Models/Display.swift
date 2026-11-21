import DedupdClient
import SwiftUI

/// How a model's files compare with what Hugging Face lists.
enum ModelHealth: Equatable, Sendable {
    /// Every app file was checked and matches.
    case verified
    /// At least one file differs from what upstream lists.
    case different
    /// Some files are not listed upstream or were not checked.
    case unconfirmed
    /// No app holds the model; it is in the store only.
    case storedOnly
    /// The owner archived the model: it is on the archive disk only.
    case archived
    /// The provider no longer has the model; this copy is what is left.
    case goneUpstream

    var symbol: String {
        switch self {
        case .verified: "checkmark.seal.fill"
        case .different: "exclamationmark.triangle.fill"
        case .unconfirmed: "questionmark.circle"
        case .storedOnly: "internaldrive"
        case .archived: "archivebox"
        case .goneUpstream: "icloud.slash"
        }
    }

    var label: String {
        switch self {
        case .verified: "Verified"
        case .different: "Differs from Hugging Face"
        case .unconfirmed: "Not fully checked"
        case .storedOnly: "In the store only"
        case .archived: "Archived"
        case .goneUpstream: "Gone from Hugging Face"
        }
    }

    var tint: Color {
        switch self {
        case .verified: .green
        case .different: .red
        case .unconfirmed, .storedOnly, .archived: .secondary
        case .goneUpstream: .orange
        }
    }
}

extension LibraryModel: @retroactive Identifiable {
    public var id: String { "\(provider)/\(repo)" }

    /// The part after the organisation: what people call the model.
    var name: String {
        RepoName.name(repo)
    }

    var organisation: String {
        meta.author ?? RepoName.organisation(repo)
    }

    var shortCommit: String? {
        commit.map { String($0.prefix(12)) }
    }

    var health: ModelHealth {
        if upstream == .gone {
            return .goneUpstream
        }
        if mismatch > 0 {
            return .different
        }
        if isArchived {
            return .archived
        }
        if apps.isEmpty {
            return .storedOnly
        }
        if notUpstream > 0 || unchecked > 0 {
            return .unconfirmed
        }
        return .verified
    }

    /// The bytes the model takes on disk: what the store holds, or what the
    /// apps hold when the store has no revision of it.
    var bytes: UInt64 {
        max(storedBytes, apps.map(\.bytes).max() ?? 0)
    }

    var appNames: [ModelApp] { apps.map(\.app) }

    /// The owner moved the model to the archive disk; it is in no app.
    var isArchived: Bool { archivedAt != nil }

    /// When the model's newest revision was added, for sorting; the oldest date when
    /// it never was.
    var fetched: Date {
        fetchedAt.map(TimeFormat.date(epoch:)) ?? .distantPast
    }
}

extension SearchHit: @retroactive Identifiable {
    var name: String {
        RepoName.name(id)
    }

    var organisation: String {
        meta.author ?? RepoName.organisation(id)
    }

    var modified: Date {
        meta.lastModified.flatMap(TimeFormat.date(iso:)) ?? .distantPast
    }

    /// What is already here: "In Bionic and Ollama", "In the store",
    /// "Downloading", or nothing.
    var localState: String? {
        if downloading {
            return "Downloading"
        }
        if !apps.isEmpty {
            return "In " + ListFormatter.localizedString(byJoining: apps.map(\.shortTitle))
        }
        return archived ? "In the store" : nil
    }
}

extension Variant: @retroactive Identifiable {
    public var id: String { spec }

    /// Every file of it is on the internal disk.
    var isStored: Bool { bytes > 0 && storedBytes >= bytes }

    /// What of it is not on the internal disk.
    var missingBytes: UInt64 { bytes - min(bytes, storedBytes) }

    /// It can be activated without a download of its weights: what it lacks
    /// is small files (a README, a settings file), fetched on the way.
    var canActivate: Bool { isStored || weightsStored }

    var usableApps: [ModelApp] {
        ModelApp.allCases.filter(usableBy.contains)
    }

    func isPlaced(in app: ModelApp) -> Bool {
        placedIn.contains(app)
    }

    /// What the daemon decided for an app, from the app's profile.
    func decision(for app: ModelApp) -> Decision? {
        decisions.first { $0.app == app }
    }

    /// What activating this in an app copies first: the store on the disk
    /// of the app's folder, and what of this it lacks that another store has.
    func storeCopy(for app: ModelApp) -> StoreCopy? {
        copies.first { $0.app == app }
    }

    /// The disk of the app's folder has a store, so the app can be given
    /// this. A daemon that does not say counts as yes.
    func hasStore(for app: ModelApp) -> Bool {
        copies.isEmpty || storeCopy(for: app)?.store != nil
    }

    /// The bytes Activate copies into the store on the app's disk first.
    func copyBytes(for app: ModelApp) -> UInt64 {
        storeCopy(for: app)?.bytes ?? 0
    }

    /// The sentence that asks before such a copy.
    func copyQuestion(for app: ModelApp) -> String {
        let store = storeCopy(for: app)?.store ?? "the store"
        return "Activating \(name) in \(app.title) first copies \(ByteFormat.string(copyBytes(for: app))) "
            + "into the store \(store), on the disk of \(app.title)'s folder. The other copy stays."
    }

    /// What placing this into an app does and what it costs, as the daemon
    /// decided it.
    func placeHelp(for app: ModelApp) -> String {
        let how = switch decision(for: app)?.cost {
        case .megabytes:
            "Give \(app.title) the model as clones of the stored files with their index rewritten. "
                + "It takes a few megabytes."
        case .full:
            "Give \(app.title) a copy of the model. It takes the model's size again."
        case .nothing, nil:
            "Give \(app.title) the model as a clone of the stored copy. It takes no extra space."
        }
        let fetched = isStored ? how : how + " First the \(ByteFormat.string(missingBytes)) of small files it lacks are fetched."
        let copy = copyBytes(for: app)
        guard copy > 0, let store = storeCopy(for: app)?.store else {
            return fetched
        }
        return fetched + " Before that, \(ByteFormat.string(copy)) is copied into the store \(store), on the disk of its folder."
    }
}

extension [Variant] {
    /// What to say about an app that does not have the model. When the app
    /// cannot be given it, the reason is the daemon's own; when the daemon was
    /// not tested with the installed version of the app, that is said too.
    func note(for app: ModelApp) -> String {
        let decisions = compactMap { $0.decision(for: app) }
        guard isEmpty || contains(where: { $0.usableApps.contains(app) }) else {
            return decisions.first?.reason ?? "Cannot use this model"
        }
        if !isEmpty, !contains(where: { $0.hasStore(for: app) }) {
            return "No store on the disk of its folder · add one under Storage"
        }
        let untried = decisions.first { $0.state == .ready && $0.versionState == .untested }
        if let version = untried?.version {
            return "Does not have it · dedupd was not tested with version \(version)"
        }
        return "Does not have it"
    }
}

extension Job: @retroactive Identifiable {}

extension Location: @retroactive Identifiable {
    public var id: String {
        switch kind {
        case .store: name.map { "store-\($0)" } ?? "store"
        case .archive: "archive-\(name ?? path)"
        case .app: app?.name ?? path
        }
    }

    var title: String {
        switch kind {
        case .store: name ?? "Store"
        case .archive: name ?? "Archive"
        case .app: app?.title ?? "App"
        }
    }

    var symbol: String {
        switch kind {
        case .store: "square.stack.3d.up.fill"
        case .archive: "externaldrive"
        case .app: app?.symbol ?? "app"
        }
    }
}

extension ModelMeta {
    /// When the provider last changed the model.
    var modified: Date? {
        lastModified.flatMap(TimeFormat.date(iso:))
    }

    /// The format that says how the model is used: GGUF or MLX when the
    /// repository has one of them, otherwise the first one listed.
    var primaryFormat: String? {
        ["gguf", "mlx"].first(where: formats.contains) ?? formats.first
    }

    /// The tags that say something the other fields do not.
    var plainTags: [String] {
        tags.filter { tag in
            !tag.contains(":") && !formats.contains(tag) && tag != pipelineTag && tag != libraryName
        }
    }
}

/// The badge for a model format.
enum FormatBadge {
    static func title(for format: String) -> String {
        switch format {
        case "gguf": "GGUF"
        case "mlx": "MLX"
        case "safetensors": "Safetensors"
        case "onnx": "ONNX"
        default: format
        }
    }

    static func tint(for format: String) -> Color {
        switch format {
        case "gguf": .blue
        case "mlx": .purple
        case "safetensors": .teal
        case "onnx": .orange
        default: .secondary
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
            parts.append("backed up \(ByteFormat.files(archived)) to the backup archive")
        }
        if forgotten > 0 {
            parts.append("forgot \(ByteFormat.files(forgotten)) no longer there")
        }
        if recognised > 0 {
            parts.append("recorded \(ByteFormat.files(recognised)) that were already in the store")
        }
        if parts.isEmpty {
            return "Nothing new in the app folders."
        }
        let sentence = parts.joined(separator: ", ")
        return sentence.prefix(1).uppercased() + sentence.dropFirst() + "."
    }
}

extension Governor {
    /// "3 of 8 connections", and why when the server asked for a pause.
    var summary: String {
        if holdSeconds > 0 {
            return "The server asked for a pause of \(ByteFormat.duration(seconds: holdSeconds))"
        }
        return "\(connections) of \(window) connections in use (at most \(maxConnections))"
    }
}

extension Budget {
    /// What the provider calls its limits, in plain words.
    var title: String {
        switch name {
        case "api": "Questions to Hugging Face"
        case "resolvers": "File requests"
        default: name
        }
    }

    var fraction: Double {
        quota == 0 ? 0 : min(1, Double(remaining) / Double(quota))
    }
}
