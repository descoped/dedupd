import DedupdClient
import Foundation
import Observation

/// What the user has chosen, kept in the app's preferences.
@MainActor
@Observable
final class AppSettings {
    private static let addressKey = "daemonAddress"

    /// Where the daemon listens unless the user chose another address.
    let defaultAddress: String

    @ObservationIgnored private let defaults: UserDefaults

    /// What the user typed in Settings; empty means the default address.
    var customAddress: String {
        didSet {
            let trimmed = customAddress.trimmingCharacters(in: .whitespacesAndNewlines)
            if trimmed.isEmpty {
                defaults.removeObject(forKey: Self.addressKey)
            } else {
                defaults.set(trimmed, forKey: Self.addressKey)
            }
        }
    }

    /// The address every call to the daemon uses: `host:port`.
    var address: String {
        let trimmed = customAddress.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? defaultAddress : trimmed
    }

    init(defaults: UserDefaults = .standard, defaultAddress: String = DedupdClient.defaultAddress()) {
        self.defaults = defaults
        self.defaultAddress = defaultAddress
        self.customAddress = defaults.string(forKey: Self.addressKey) ?? ""
    }
}
