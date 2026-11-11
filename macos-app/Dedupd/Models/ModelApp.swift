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

    /// Which tools read from this place.
    var detail: String {
        switch self {
        case .bionic: "Bionic and LM Studio"
        case .ollama: "GGUF models only"
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

    static func symbol(for name: String) -> String {
        ModelApp(rawValue: name)?.symbol ?? "app"
    }
}
