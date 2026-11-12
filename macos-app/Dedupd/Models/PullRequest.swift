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

/// What a piece of typed or pasted text says a model is.
enum ModelAddress: Equatable, Sendable {
    /// A repository on Hugging Face, as `org/name`.
    case repository(String)
    /// An address at a source dedupd cannot download from yet; the text is
    /// the source's name.
    case unsupported(String)
}

/// What the text of a search field asks for.
enum TypedText: Equatable, Sendable {
    /// Show this repository, `org/name`.
    case model(String)
    /// Search for these words; none lists what the source offers.
    case words(String)
    /// Nothing is done with the text; the sentence says why.
    case refused(String)
}

/// Reading a model's name or address out of text a person typed or pasted.
///
/// The text is not trusted. The only thing that ever leaves this type is an
/// `org/name` made of letters, digits, `-`, `_` and `.`, checked the way
/// Hugging Face checks a repository's name. Nothing else of the text is
/// used: not its host (it is compared with a fixed list and then dropped),
/// not the rest of its path, not its query. A text that is anything but a
/// plain name or a plain address on a known host is not an address at all.
enum ModelSpec {
    /// `org/repo`, `org/repo:Q4_K_M` or `org/repo@revision`: the way the
    /// daemon reads what it is asked to download.
    static func isValid(_ text: String) -> Bool {
        let pattern = /[A-Za-z0-9][A-Za-z0-9._-]*\/[A-Za-z0-9][A-Za-z0-9._-]*(:[A-Za-z0-9._-]+)?(@[A-Za-z0-9._\/-]+)?/
        return text.wholeMatch(of: pattern) != nil
    }

    /// The hosts whose addresses name a repository on Hugging Face.
    private static let huggingFaceHosts: Set<String> = ["huggingface.co", "www.huggingface.co", "hf.co"]
    /// Hosts of sources that are known and not built yet, with their names.
    private static let laterHosts: [String: String] = [
        "modelscope.cn": "ModelScope", "www.modelscope.cn": "ModelScope",
        "modelscope.ai": "ModelScope", "www.modelscope.ai": "ModelScope",
    ]
    /// What an address on huggingface.co can start with that is not an
    /// organisation: its other kinds of repository and its own pages.
    private static let notOrganisations: Set<String> = [
        "datasets", "spaces", "models", "docs", "api", "blog", "collections", "organizations", "settings",
        "join", "login", "pricing", "tasks", "papers", "posts", "chat", "learn", "enterprise", "new",
    ]
    /// The longest text that is read at all.
    private static let longest = 512

    /// One half of `org/name`, by Hugging Face's own rule: up to 96
    /// letters, digits, `-`, `_` and `.`; it starts and ends with a letter,
    /// a digit or `_`; no `--`, no `..`, and it does not end in `.git`.
    static func isNamePart(_ part: some StringProtocol) -> Bool {
        let bytes = Array(part.utf8)
        func isWord(_ byte: UInt8) -> Bool {
            (byte >= 0x30 && byte <= 0x39) || (byte >= 0x41 && byte <= 0x5A) || (byte >= 0x61 && byte <= 0x7A) || byte == 0x5F
        }
        guard let first = bytes.first, let last = bytes.last, bytes.count <= 96, isWord(first), isWord(last) else {
            return false
        }
        guard bytes.allSatisfy({ isWord($0) || $0 == 0x2D || $0 == 0x2E }) else {
            return false
        }
        return !part.contains("--") && !part.contains("..") && !part.hasSuffix(".git")
    }

    /// Whether the text is `org/name` and nothing else. A host's name is
    /// not an organisation: `hf.co/org` is half an address, not a model.
    static func isRepository(_ text: String) -> Bool {
        let parts = text.split(separator: "/", omittingEmptySubsequences: false)
        guard parts.count == 2, parts.allSatisfy({ isNamePart($0) }) else {
            return false
        }
        let first = parts[0].lowercased()
        return !notOrganisations.contains(first) && !huggingFaceHosts.contains(first) && laterHosts[first] == nil
    }

    /// What the text names: a repository, by `org/name` or by its address on
    /// huggingface.co; an address at a source that is not built yet; or
    /// nothing (words to search for, or something that is not to be used).
    static func address(in text: String) -> ModelAddress? {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        // Only what a name or a plain address is written with: no spaces,
        // no control characters, no backslash or quotes, nothing outside
        // ASCII (so no letter that only looks like another).
        let allowed = Set("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~:/?#[]@!$&'()*+,;=%".utf8)
        guard !text.isEmpty, text.utf8.count <= longest, text.utf8.allSatisfy(allowed.contains) else {
            return nil
        }
        if isRepository(text) {
            return .repository(text)
        }
        let lower = text.lowercased()
        let spelled: String
        if lower.hasPrefix("https://") || lower.hasPrefix("http://") {
            spelled = text
        } else if text.contains("://") {
            // Any other scheme: a file, a script, something else.
            return nil
        } else {
            spelled = "https://" + text
        }
        // The host must be one of the known ones exactly as written, with
        // no name or password before it and no port after it.
        guard let parts = URLComponents(string: spelled), parts.user == nil, parts.password == nil, parts.port == nil,
              let host = parts.encodedHost?.lowercased()
        else {
            return nil
        }
        if let source = laterHosts[host] {
            return .unsupported(source)
        }
        guard huggingFaceHosts.contains(host) else {
            return nil
        }
        // The first two parts of the path, as they are written: nothing
        // escaped, nothing empty, and each a name Hugging Face would allow.
        let path = parts.percentEncodedPath
        let names = path.split(separator: "/", omittingEmptySubsequences: false).dropFirst().prefix(2)
        guard path.hasPrefix("/"), !path.contains("%"), names.count == 2 else {
            return nil
        }
        let repo = names.joined(separator: "/")
        return isRepository(repo) ? .repository(repo) : nil
    }

    /// The longest text that is searched for.
    private static let longestWords = 200

    /// What a search field's text asks for. A model's name, the name as
    /// the command line writes it (`org/name:Q4_K_M`) or its address shows
    /// that model. Any other text with a `/` in it is taken for an address
    /// that is not one, and is not searched for. The rest is words.
    static func typed(_ text: String) -> TypedText {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        switch address(in: text) {
        case let .repository(repo):
            return .model(repo)
        case let .unsupported(source):
            return .refused(
                "\(source) is not supported yet. dedupd downloads from Hugging Face: "
                    + "paste a model's address on huggingface.co, or its name as org/name."
            )
        case nil:
            break
        }
        if isValid(text), let repo = text.split(whereSeparator: { $0 == ":" || $0 == "@" }).first.map(String.init),
           isRepository(repo) {
            return .model(repo)
        }
        if text.contains("/") || text.contains("\\") {
            return .refused(
                "That is not a model's name or an address on huggingface.co. "
                    + "Paste an address like https://huggingface.co/org/name, or the name as org/name."
            )
        }
        let isPlain = text.unicodeScalars.allSatisfy { scalar in
            switch scalar.properties.generalCategory {
            case .control, .format, .surrogate, .privateUse, .unassigned, .lineSeparator, .paragraphSeparator:
                false
            default:
                true
            }
        }
        guard text.count <= longestWords, isPlain else {
            return .refused("That text cannot be searched for. Type a few words, or a model's name as org/name.")
        }
        return .words(text)
    }

    /// The repository a text names, when it names one.
    static func repository(in text: String) -> String? {
        if case let .repository(repo) = address(in: text) {
            return repo
        }
        return nil
    }
}
