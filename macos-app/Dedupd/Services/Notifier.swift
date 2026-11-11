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
    /// go on without them. A download that was already in that state when
    /// the app first saw it is old news.
    static func notices(from before: [Job], to after: [Job]) -> [Notice] {
        let known = Dictionary(before.map { ($0.id, $0.phase) }, uniquingKeysWith: { first, _ in first })
        return after.compactMap { job in
            guard let earlier = known[job.id], earlier != job.phase else {
                return nil
            }
            switch job.phase {
            case .done:
                return Notice(
                    id: "download-\(job.id)",
                    title: "Downloaded \(job.model)",
                    body: "\(ByteFormat.string(job.bytesTotal)), placed into \(job.appList)."
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

/// Posts notices through macOS. Permission is asked for the first time
/// there is something to say, not at launch.
@MainActor
final class Notifier {
    /// False under tests and canned daemons, which must not post anything.
    private let isLive: Bool

    init(isLive: Bool) {
        self.isLive = isLive
    }

    func jobsChanged(from before: [Job], to after: [Job]) {
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
