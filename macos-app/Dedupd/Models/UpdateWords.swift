import Foundation

/// What the app says about its updates and about the daemon following it.
enum UpdateWords {
    /// After an update of the app, the older installed daemon was replaced.
    static func daemonFollowed(from old: String, to new: String) -> String {
        "The daemon was updated from \(old) to \(new) together with this app."
    }

    /// Under the update switches in Settings.
    static let howItWorks = "A new version is offered with what changed, and installed only when you say so. "
        + "The daemon is brought to the same version with it."

    /// In Settings, for a copy that does not check: a build of its own, or the
    /// canned daemon of the tests. Every released copy checks: the release
    /// script refuses to make one without the update key.
    static let notChecking = "This copy does not check for updates: it is a build of its own, not a release."

    /// When the last check was made.
    static func lastChecked(_ date: Date, now: Date = .now) -> String {
        "Last checked \(date.formatted(.relative(presentation: .named, unitsStyle: .wide)))"
    }
}
