import DedupdClient
import Foundation
import UserNotifications

/// Something worth telling the owner while the window is closed.
struct Notice: Equatable, Sendable {
    var id: String
    var title: String
    var body: String

    /// What changed between two states of the download queue that the owner
    /// wants to hear about: a download that finished, and one that cannot
    /// go on without them. `before` is nil when the app had no list (it
    /// has just started, or the daemon was away): what is finished then is
    /// old news. A job that is not in a list the app did have is new, and
    /// is told about also when it was finished the first time it was seen
    /// (the daemon reports a change once it has settled, so a quick job is
    /// never seen running).
    static func notices(from before: [Job]?, to after: [Job]) -> [Notice] {
        guard let before else {
            return []
        }
        let known = Dictionary(before.map { ($0.id, $0.state) }, uniquingKeysWith: { first, _ in first })
        return after.compactMap { job in
            guard known[job.id] != job.state else {
                return nil
            }
            switch job.state {
            case .done where job.isActivation:
                // An activation is seen where it was asked for; the owner
                // hears only when an app did not take the model.
                guard !job.missed.isEmpty else {
                    return nil
                }
                return Notice(id: "download-\(job.id)", title: job.repo, body: Self.sentence(job.outcomeText))
            case .done:
                return Notice(
                    id: "download-\(job.id)",
                    title: "Downloaded \(job.model)",
                    body: "\(ByteFormat.string(job.bytesTotal)). \(Self.sentence(job.outcomeText))"
                )
            case .needsAttention:
                return Notice(
                    id: "download-\(job.id)",
                    title: "\(job.model) needs attention",
                    body: job.detail ?? "The download stopped and will not continue by itself."
                )
            default:
                return nil
            }
        }
    }
}

extension Notice {
    /// The text with one full stop at its end.
    static func sentence(_ text: String) -> String {
        text.hasSuffix(".") ? text : text + "."
    }
}

/// Posts notices through macOS. Permission is asked for the first time
/// there is something to say, not at launch.
@MainActor
final class Notifier {
    /// False under tests and canned daemons, which must not post anything.
    private let isLive: Bool

    init(isLive: Bool) {
        self.isLive = isLive
    }

    func jobsChanged(from before: [Job]?, to after: [Job]) {
        let notices = Notice.notices(from: before, to: after)
        guard isLive, !notices.isEmpty else {
            return
        }
        Task {
            let center = UNUserNotificationCenter.current()
            guard (try? await center.requestAuthorization(options: [.alert, .sound])) == true else {
                return
            }
            for notice in notices {
                let content = UNMutableNotificationContent()
                content.title = notice.title
                content.body = notice.body
                content.sound = .default
                try? await center.add(UNNotificationRequest(identifier: notice.id, content: content, trigger: nil))
            }
        }
    }
}
