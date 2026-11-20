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
        static let activitySeen = "activitySeen"
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

    /// The newest entry of the Activity list the owner has seen; a failure
    /// after it counts on the sidebar's badge.
    var activitySeen: Int64 {
        didSet { defaults.set(Int(activitySeen), forKey: Key.activitySeen) }
    }

    /// Tell the owner when a download is done or needs them.
    var notifications: Bool {
        didSet { defaults.set(notifications, forKey: Key.notifications) }
    }

    /// The apps a model goes into, as the owner chose them for each
    /// format: a GGUF file suits other apps than an MLX repository does.
    private(set) var targets: [ModelFormat: Set<ModelApp>] {
        didSet {
            let stored = Dictionary(uniqueKeysWithValues: targets.map { format, apps in
                (format.name, apps.map(\.name).sorted())
            })
            defaults.set(stored, forKey: Key.targets)
        }
    }

    /// The address every call to the daemon uses: `host:port`. A saved
    /// address that is not one (kept by a build that did not check) is not
    /// used; Settings says so.
    var address: String {
        let trimmed = customAddress.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty || !addressIsValid(address: trimmed) ? defaultAddress : trimmed
    }

    /// True the first time the app runs with these preferences.
    let isFirstLaunch: Bool

    init(defaults: UserDefaults = .standard, defaultAddress: String = DedupdClient.defaultAddress()) {
        self.defaults = defaults
        self.defaultAddress = defaultAddress
        customAddress = defaults.string(forKey: Key.address) ?? ""
        density = RowDensity(rawValue: defaults.string(forKey: Key.density) ?? "") ?? .compact
        notifications = defaults.object(forKey: Key.notifications) as? Bool ?? true
        activitySeen = Int64(defaults.integer(forKey: Key.activitySeen))
        let stored = defaults.dictionary(forKey: Key.targets) as? [String: [String]] ?? [:]
        targets = Dictionary(uniqueKeysWithValues: stored.compactMap { kind, apps in
            ModelFormat(name: kind).map { ($0, Set(apps.compactMap(ModelApp.init(name:)))) }
        })
        isFirstLaunch = !defaults.bool(forKey: Key.launched)
        defaults.set(true, forKey: Key.launched)
    }

    /// Where a model of this format goes: what the owner chose last, or
    /// the daemon's apps that share the stored files at no cost.
    func apps(for format: ModelFormat, known: [AppInfo]) -> Set<ModelApp> {
        targets[format] ?? known.defaultApps(for: format)
    }

    /// The remembered apps a variant can actually be used by, in the order
    /// the daemon lists them.
    func apps(for format: ModelFormat, usable: [ModelApp], known: [AppInfo]) -> [ModelApp] {
        let chosen = apps(for: format, known: known)
        return usable.filter(chosen.contains)
    }

    func setApp(_ app: ModelApp, for format: ModelFormat, to enabled: Bool, known: [AppInfo]) {
        var apps = apps(for: format, known: known)
        if enabled {
            apps.insert(app)
        } else {
            apps.remove(app)
        }
        targets[format] = apps
    }
}
