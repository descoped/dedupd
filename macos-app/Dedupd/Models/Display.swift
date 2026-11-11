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
    /// The provider no longer has the model; this copy is what is left.
    case goneUpstream

    var symbol: String {
        switch self {
        case .verified: "checkmark.seal.fill"
        case .different: "exclamationmark.triangle.fill"
        case .unconfirmed: "questionmark.circle"
        case .storedOnly: "archivebox"
        case .goneUpstream: "icloud.slash"
        }
    }

    var label: String {
        switch self {
        case .verified: "Verified"
        case .different: "Differs from Hugging Face"
        case .unconfirmed: "Not fully checked"
        case .storedOnly: "In the store only"
        case .goneUpstream: "Gone from Hugging Face"
        }
    }

    var tint: Color {
        switch self {
        case .verified: .green
        case .different: .red
        case .unconfirmed, .storedOnly: .secondary
        case .goneUpstream: .orange
        }
    }
}

extension LibraryModel: @retroactive Identifiable {
    public var id: String { "\(provider)/\(repo)" }

    /// The part after the organisation: what people call the model.
    var name: String {
        repo.split(separator: "/").last.map(String.init) ?? repo
    }

    var organisation: String {
        meta.author ?? repo.split(separator: "/").first.map(String.init) ?? ""
    }

    var shortCommit: String? {
        commit.map { String($0.prefix(12)) }
    }

    var health: ModelHealth {
        if upstream == "gone" {
            return .goneUpstream
        }
        if mismatch > 0 {
            return .different
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

    var appNames: [String] { apps.map(\.app) }

    /// When the model was last archived, for sorting; the oldest date when
    /// it never was.
    var fetched: Date {
        fetchedAt.map(TimeFormat.date(epoch:)) ?? .distantPast
    }

    /// "GGUF · 135M · apache-2.0", with the parts that are known.
    var summary: String {
        var parts = meta.formats.map(FormatBadge.title(for:))
        if let parameters = meta.parameters {
            parts.append(ByteFormat.parameters(parameters))
        }
        if let license = meta.license {
            parts.append(license)
        }
        return parts.joined(separator: " · ")
    }
}

extension SearchHit: @retroactive Identifiable {
    var name: String {
        id.split(separator: "/").last.map(String.init) ?? id
    }

    var organisation: String {
        meta.author ?? id.split(separator: "/").first.map(String.init) ?? ""
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
            return "In " + ListFormatter.localizedString(byJoining: apps.map(ModelApp.shortTitle(for:)))
        }
        return archived ? "In the store" : nil
    }
}

extension Variant: @retroactive Identifiable {
    public var id: String { spec }

    /// Which kind of model this is, for remembering where such models go.
    func kind(formats: [String]) -> ModelKind {
        if kind == "gguf" {
            return usableBy.contains(ModelApp.ollama.rawValue) ? .gguf : .splitGGUF
        }
        return formats.contains("mlx") ? .mlx : .other
    }

    /// Every file of it is on the internal disk.
    var isStored: Bool { bytes > 0 && storedBytes >= bytes }

    var usableApps: [ModelApp] {
        ModelApp.allCases.filter { usableBy.contains($0.rawValue) }
    }

    func isPlaced(in app: ModelApp) -> Bool {
        placedIn.contains(app.rawValue)
    }
}

extension Job: @retroactive Identifiable {}

extension Location: @retroactive Identifiable {
    var title: String {
        switch id {
        case "store": "Store"
        case "archive": "Archive"
        default: ModelApp.title(for: id)
        }
    }

    var symbol: String {
        switch id {
        case "store": "square.stack.3d.up.fill"
        case "archive": "externaldrive"
        default: ModelApp.symbol(for: id)
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
