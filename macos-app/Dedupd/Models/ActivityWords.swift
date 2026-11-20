import DedupdClient
import Foundation

extension ActivityEntry {
    /// The symbol for what was done.
    var symbol: String {
        switch action {
        case .activate: "plus.circle"
        case .deactivate: "minus.circle"
        case .archive: "archivebox"
        case .restore: "arrow.uturn.backward.circle"
        }
    }

    /// What did not happen, in front of the reason: "Not activated in Ollama".
    var failureTitle: String {
        let app = app.map { " in \($0.title)" } ?? ""
        return switch action {
        case .activate: "Not activated\(app)"
        case .deactivate: "Not deactivated\(app)"
        case .archive: "Not archived"
        case .restore: "Not restored"
        }
    }
}
