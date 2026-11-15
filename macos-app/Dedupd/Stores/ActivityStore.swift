import DedupdClient
import Foundation
import Observation

/// A line of the Activity list: an activation under way, or an entry of
/// the history.
enum ActivityLine: Hashable, Sendable {
    case job(Int64)
    case entry(Int64)
}

/// What happened to the models in the apps: the Activity screen's list, and
/// the failures the owner has not seen yet, for the sidebar's badge.
@MainActor
@Observable
final class ActivityStore {
    private(set) var entries: [ActivityEntry] = []

    @ObservationIgnored private let daemon: DaemonStore

    init(daemon: DaemonStore) {
        self.daemon = daemon
    }

    /// Failures that came after the owner last looked.
    var unseenFailures: Int {
        let seen = daemon.settings.activitySeen
        return entries.filter { !$0.ok && $0.id > seen }.count
    }

    /// Activations the daemon is working on, or that wait for the owner.
    var running: [Job] {
        daemon.activationJobs.filter { !$0.phase.isFinished }
    }

    /// The model a line is about.
    func repo(of line: ActivityLine) -> String? {
        switch line {
        case let .job(id): running.first { $0.id == id }?.repo
        case let .entry(id): entries.first { $0.id == id }?.repo
        }
    }

    /// Read the list again. Quiet on failure: the list is history, and the
    /// daemon being away is shown elsewhere.
    func reload() async {
        let address = daemon.address
        guard daemon.isRunning, let answer = try? await daemon.client.activity(at: address),
              address == daemon.address
        else {
            return
        }
        entries = answer
    }

    /// The owner is looking at the list: what is in it now is seen.
    func markSeen() {
        guard let newest = entries.first?.id, newest > daemon.settings.activitySeen else {
            return
        }
        daemon.settings.activitySeen = newest
    }
}
