import DedupdClient
import Foundation
import Observation

/// What the user has chosen, kept in the app's preferences.
@MainActor
@Observable
final class AppSettings {
    private enum Key {
        static let address = "daemonAddress"
        static let density = "rowDensity"
        static let targets = "targets"
        static let notifications = "notifications"
        static let launched = "hasLaunchedBefore"
    }

    /// Where the daemon listens unless the user chose another address.
    let defaultAddress: String

    @ObservationIgnored private let defaults: UserDefaults

    /// What the user typed in Settings; empty means the default address.
    var customAddress: String {
        didSet {
            let trimmed = customAddress.trimmingCharacters(in: .whitespacesAndNewlines)
            if trimmed.isEmpty {
                defaults.removeObject(forKey: Key.address)
            } else {
                defaults.set(trimmed, forKey: Key.address)
            }
        }
    }

    /// How much room a row of the tables takes.
    var density: RowDensity {
        didSet { defaults.set(density.rawValue, forKey: Key.density) }
    }

    /// Tell the owner when a download is done or needs them.
    var notifications: Bool {
        didSet { defaults.set(notifications, forKey: Key.notifications) }
    }

    /// The apps a model goes into, remembered for each kind of model:
    /// a GGUF file suits other apps than an MLX repository does.
    private(set) var targets: [ModelKind: Set<ModelApp>] {
        didSet {
            let stored = Dictionary(uniqueKeysWithValues: targets.map { kind, apps in
                (kind.rawValue, apps.map(\.rawValue).sorted())
            })
            defaults.set(stored, forKey: Key.targets)
        }
    }

    /// The address every call to the daemon uses: `host:port`.
    var address: String {
        let trimmed = customAddress.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? defaultAddress : trimmed
    }

    /// True the first time the app runs with these preferences.
    let isFirstLaunch: Bool

    init(defaults: UserDefaults = .standard, defaultAddress: String = DedupdClient.defaultAddress()) {
        self.defaults = defaults
        self.defaultAddress = defaultAddress
        customAddress = defaults.string(forKey: Key.address) ?? ""
        density = RowDensity(rawValue: defaults.string(forKey: Key.density) ?? "") ?? .compact
        notifications = defaults.object(forKey: Key.notifications) as? Bool ?? true
        let stored = defaults.dictionary(forKey: Key.targets) as? [String: [String]] ?? [:]
        targets = Dictionary(uniqueKeysWithValues: stored.compactMap { kind, apps in
            ModelKind(rawValue: kind).map { ($0, Set(apps.compactMap(ModelApp.init(rawValue:)))) }
        })
        isFirstLaunch = !defaults.bool(forKey: Key.launched)
        defaults.set(true, forKey: Key.launched)
    }

    /// Where a model of this kind goes: what the owner chose last, or the
    /// usual apps for the kind.
    func apps(for kind: ModelKind) -> Set<ModelApp> {
        targets[kind] ?? kind.defaultApps
    }

    /// The remembered apps a variant can actually be used by, in the order
    /// the daemon lists them.
    func apps(for kind: ModelKind, usable: [ModelApp]) -> [ModelApp] {
        let chosen = apps(for: kind)
        return usable.filter(chosen.contains)
    }

    func setApp(_ app: ModelApp, for kind: ModelKind, to enabled: Bool) {
        var apps = apps(for: kind)
        if enabled {
            apps.insert(app)
        } else {
            apps.remove(app)
        }
        targets[kind] = apps
    }
}
