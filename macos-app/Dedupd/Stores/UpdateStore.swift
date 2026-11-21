import Foundation
import Observation
import Sparkle

/// New releases of the app: checks the release feed and installs a new
/// version on the owner's yes, with Sparkle.
///
/// Only a release build that carries the update key checks. A debug build
/// (as `just app-install` makes), the canned daemon of the UI tests and the
/// unit tests' host do not; a debug build checks only when a trial names a
/// feed on this Mac with `-updateFeed`.
@MainActor
@Observable
final class UpdateStore {
    /// This copy of the app checks for updates.
    let isAvailable: Bool
    /// A check can be started now (none is running).
    private(set) var canCheck = false
    /// Look for a new release by itself, once a day.
    var checksAutomatically: Bool {
        didSet {
            controller?.updater.automaticallyChecksForUpdates = checksAutomatically
        }
    }
    /// When the last check was made, by the owner or by itself.
    private(set) var lastChecked: Date?

    @ObservationIgnored private let controller: SPUStandardUpdaterController?
    @ObservationIgnored private let feed: FeedChoice
    @ObservationIgnored private var observations: [NSKeyValueObservation] = []

    init(isAllowed: Bool, publicKey: String?, feedOverride: String?) {
        feed = FeedChoice(feed: feedOverride)
        let keyed = publicKey.map { !$0.isEmpty && !$0.hasPrefix("$(") } ?? false
        isAvailable = isAllowed && keyed
        guard isAvailable else {
            controller = nil
            checksAutomatically = false
            return
        }
        let controller = SPUStandardUpdaterController(startingUpdater: true, updaterDelegate: feed, userDriverDelegate: nil)
        self.controller = controller
        checksAutomatically = controller.updater.automaticallyChecksForUpdates
        lastChecked = controller.updater.lastUpdateCheckDate
        let updater = controller.updater
        // Sparkle says what changed on the main thread; the values are read
        // again here rather than carried across.
        observations = [
            updater.observe(\.canCheckForUpdates, options: [.initial, .new]) { [weak self] _, _ in
                Task { @MainActor in self?.refresh() }
            },
            updater.observe(\.lastUpdateCheckDate, options: [.new]) { [weak self] _, _ in
                Task { @MainActor in self?.refresh() }
            },
        ]
    }

    /// The owner asked: check now and show what was found.
    func check() {
        controller?.checkForUpdates(nil)
    }

    private func refresh() {
        guard let updater = controller?.updater else {
            return
        }
        canCheck = updater.canCheckForUpdates
        lastChecked = updater.lastUpdateCheckDate
    }

    /// The feed a trial names, when it is on this Mac; otherwise the one in
    /// Info.plist (the latest GitHub release's `appcast.xml`).
    nonisolated static func trialFeed(_ text: String?) -> String? {
        guard let text, let url = URL(string: text), url.scheme == "http" || url.scheme == "https",
              let host = url.host(percentEncoded: false),
              ["127.0.0.1", "localhost", "::1"].contains(host) else {
            return nil
        }
        return text
    }
}

/// Tells Sparkle which feed to read: the trial's, or the one in Info.plist.
private final class FeedChoice: NSObject, SPUUpdaterDelegate, Sendable {
    let feed: String?

    init(feed: String?) {
        self.feed = feed
    }

    func feedURLString(for updater: SPUUpdater) -> String? {
        feed
    }
}
