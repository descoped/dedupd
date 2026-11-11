import Foundation

/// What the daemon is asked to download and where to place it.
struct PullRequest: Equatable, Sendable {
    /// `org/repo`, `org/repo:Q4_K_M` or `org/repo@revision`.
    var model: String
    /// The apps to place it into, by the names the daemon uses.
    var apps: [String]
    var include: [String] = []
    /// Use only what is archived; the provider is not asked.
    var offline = false
}

/// What can be done to a download in the queue. The raw value is the word
/// the daemon's control API uses.
enum JobAction: String, Sendable {
    case pause
    case resume
    case retry
    /// Start now, ahead of the queue.
    case start
    case cancel
    /// Take a finished download off the list.
    case remove
}

/// Whether a piece of text names a repository the way the daemon reads it:
/// `org/repo`, `org/repo:Q4_K_M` or `org/repo@revision`.
enum ModelSpec {
    static func isValid(_ text: String) -> Bool {
        let pattern = /[A-Za-z0-9][A-Za-z0-9._-]*\/[A-Za-z0-9][A-Za-z0-9._-]*(:[A-Za-z0-9._-]+)?(@[A-Za-z0-9._\/-]+)?/
        return text.wholeMatch(of: pattern) != nil
    }

    /// The repository a search text names, when it is a repository's name or
    /// its address on huggingface.co rather than words to search for.
    static func repository(in text: String) -> String? {
        var text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        for prefix in ["https://huggingface.co/", "http://huggingface.co/", "huggingface.co/", "hf.co/"]
        where text.hasPrefix(prefix) {
            text = String(text.dropFirst(prefix.count))
        }
        let parts = text.split(separator: "/", omittingEmptySubsequences: false).prefix(2)
        let repo = parts.joined(separator: "/")
        guard parts.count == 2, isValid(repo), !repo.contains(":"), !repo.contains("@") else {
            return nil
        }
        return repo
    }
}
