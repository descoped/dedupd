import Foundation

/// An app the daemon can place a model into. The raw value is the name the
/// daemon's control API uses.
enum ModelApp: String, CaseIterable, Identifiable, Sendable {
    case bionic
    case ollama
    case huggingFace = "hf"

    var id: String { rawValue }

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

    /// Which tools read from this place.
    var detail: String {
        switch self {
        case .bionic: "Bionic and LM Studio"
        case .ollama: "One GGUF file per model"
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

    /// The title for a name the daemon reports, also one this app does not know.
    static func title(for name: String) -> String {
        ModelApp(rawValue: name)?.title ?? name
    }

    static func shortTitle(for name: String) -> String {
        ModelApp(rawValue: name)?.shortTitle ?? name
    }

    static func badge(for name: String) -> String {
        ModelApp(rawValue: name)?.badge ?? name
    }

    static func symbol(for name: String) -> String {
        ModelApp(rawValue: name)?.symbol ?? "app"
    }
}

/// The kinds of model the owner chooses apps for. The choice is remembered
/// for each kind, because each app takes different kinds.
enum ModelKind: String, CaseIterable, Identifiable, Sendable {
    /// One GGUF file: every app can use it.
    case gguf
    /// A GGUF in several parts, which Ollama cannot use.
    case splitGGUF = "gguf-split"
    /// An MLX repository, for Bionic and the MLX tools.
    case mlx
    /// Anything else: only tools that read the Hugging Face cache.
    case other

    var id: String { rawValue }

    var title: String {
        switch self {
        case .gguf: "GGUF"
        case .splitGGUF: "GGUF in several parts"
        case .mlx: "MLX"
        case .other: "Other formats"
        }
    }

    /// Where a model of this kind goes unless the owner chose otherwise.
    var defaultApps: Set<ModelApp> {
        switch self {
        case .gguf: [.bionic, .ollama]
        case .splitGGUF: [.bionic]
        case .mlx: [.bionic]
        case .other: [.huggingFace]
        }
    }
}
