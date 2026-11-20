import Foundation

/// Addresses on huggingface.co.
enum HuggingFace {
    /// A model's page; nil for anything that is not a model's name, so a
    /// name the daemon passed on cannot lead to another page of the site.
    static func page(_ repo: String) -> URL? {
        guard ModelSpec.isRepository(repo) else {
            return nil
        }
        return URL(string: "https://huggingface.co/\(repo)")
    }
}
