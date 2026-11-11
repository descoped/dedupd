import Foundation

/// What the daemon is asked to fetch and where to place it.
struct PullRequest: Equatable, Sendable {
    var model: String
    var apps: [String]
    var include: [String] = []
    var offline = false
}

/// What the user has typed and ticked in the pull form.
struct PullForm: Equatable {
    /// `org/repo`, optionally with `:quantization` and `@revision`.
    var model = ""
    var apps: Set<ModelApp> = [.bionic]
    var offline = false

    /// The model with surrounding white space removed.
    var trimmedModel: String {
        model.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// Whether the text names a repository the way the daemon reads it:
    /// `org/repo`, `org/repo:Q4_K_M` or `org/repo@revision`.
    var isValid: Bool {
        let pattern = /[A-Za-z0-9][A-Za-z0-9._-]*\/[A-Za-z0-9][A-Za-z0-9._-]*(:[A-Za-z0-9._-]+)?(@[A-Za-z0-9._\/-]+)?/
        return trimmedModel.wholeMatch(of: pattern) != nil
    }

    /// Why the form cannot be sent, or nil when it can.
    var problem: String? {
        if trimmedModel.isEmpty {
            return nil
        }
        return isValid ? nil : "Write the model as org/repo, optionally with :quantization"
    }

    /// The request to send, or nil while the form is not valid. The apps are
    /// listed in a fixed order so the same form always gives the same request.
    var request: PullRequest? {
        guard isValid else {
            return nil
        }
        let names = ModelApp.allCases.filter(apps.contains).map(\.rawValue)
        return PullRequest(model: trimmedModel, apps: names, offline: offline)
    }
}
