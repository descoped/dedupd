import DedupdClient
import Foundation

/// An app the daemon can place a model into. The daemon defines the apps;
/// this is how the app shows them.
extension ModelApp: @retroactive Identifiable {
    public var id: String { name }

    /// The name the daemon's control API uses, also for remembering the
    /// owner's choices and naming controls.
    var name: String {
        switch self {
        case .bionic: "bionic"
        case .ollama: "ollama"
        case .huggingFace: "hf"
        }
    }

    init?(name: String) {
        guard let app = Self.allCases.first(where: { $0.name == name }) else {
            return nil
        }
        self = app
    }

    /// The name shown in the app.
    var title: String {
        switch self {
        case .bionic: "Bionic"
        case .ollama: "Ollama"
        case .huggingFace: "Hugging Face cache"
        }
    }

    /// A name short enough for a table cell.
    var shortTitle: String {
        switch self {
        case .bionic: "Bionic"
        case .ollama: "Ollama"
        case .huggingFace: "HF cache"
        }
    }

    /// A name short enough for a tag in a table row.
    var badge: String {
        switch self {
        case .bionic: "Bionic"
        case .ollama: "Ollama"
        case .huggingFace: "HF"
        }
    }

    /// Which tools read from this place. What each takes is the daemon's
    /// to say (the app profiles), not this text's.
    var detail: String {
        switch self {
        case .bionic: "Bionic and LM Studio"
        case .ollama: "Ollama"
        case .huggingFace: "MLX tools, llama.cpp, hf"
        }
    }

    var symbol: String {
        switch self {
        case .bionic: "cpu"
        case .ollama: "shippingbox"
        case .huggingFace: "tray.full"
        }
    }
}

/// The formats the owner chooses apps for. The choice is remembered for
/// each format, because each app takes different formats; which ones is
/// the daemon's answer (`AppInfo`), from the app profiles.
extension ModelFormat: @retroactive Identifiable {
    public var id: String { name }

    /// The name the choice is remembered by.
    var name: String {
        switch self {
        case .gguf: "gguf"
        case .mlx: "mlx"
        case .other: "other"
        }
    }

    init?(name: String) {
        guard let format = Self.allCases.first(where: { $0.name == name }) else {
            return nil
        }
        self = format
    }

    var title: String {
        switch self {
        case .gguf: "GGUF"
        case .mlx: "MLX"
        case .other: "Other formats"
        }
    }
}

extension [AppInfo] {
    /// The apps dedupd can give a model of this format, in the daemon's
    /// order.
    func apps(taking format: ModelFormat) -> [ModelApp] {
        filter { $0.takes.contains { $0.format == format } }.map(\.app)
    }

    /// Where a new model of this format goes until the owner chooses: the
    /// apps that share the stored files at no cost. The Hugging Face cache,
    /// which serves developer tools, only when no other app takes the format.
    func defaultApps(for format: ModelFormat) -> Set<ModelApp> {
        let free = filter { $0.takes.contains(Taken(format: format, cost: .nothing)) }.map(\.app)
        let apps = free.filter { $0 != .huggingFace }
        return Set(apps.isEmpty ? free : apps)
    }
}
