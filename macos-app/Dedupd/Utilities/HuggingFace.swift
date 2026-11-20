import Foundation

/// Addresses on huggingface.co.
enum HuggingFace {
    /// A model's page; nil for a name that makes no address.
    static func page(_ repo: String) -> URL? {
        URL(string: "https://huggingface.co/\(repo)")
    }
}
