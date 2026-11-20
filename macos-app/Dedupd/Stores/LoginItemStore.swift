import Foundation
import Observation

/// Whether macOS opens the app at login. The app's own matter, not the
/// daemon's: the daemon has its launchd agent and does not need the app.
@MainActor
@Observable
final class LoginItemStore {
    private(set) var isEnabled = false
    /// Why the last change did not take; shown where the switch is.
    private(set) var failure: String?

    @ObservationIgnored private let item: any LoginItemControl

    init(item: any LoginItemControl) {
        self.item = item
        isEnabled = item.isEnabled
    }

    /// Read what macOS says now: the owner can also change it in System
    /// Settings.
    func refresh() {
        isEnabled = item.isEnabled
    }

    func set(_ enabled: Bool) {
        do {
            try item.set(enabled)
            failure = nil
        } catch {
            failure = "macOS did not change the login item: \(error.localizedDescription)"
        }
        isEnabled = item.isEnabled
    }
}
