import AppKit
import Observation
import SwiftUI

/// Which screen the main window shows. Shared, so the menu bar panel and a
/// notification can send the owner to a screen.
@MainActor
@Observable
final class Navigation {
    var pane: Pane

    init(pane: Pane = .library) {
        self.pane = pane
    }
}

/// Opens the main window and brings the app forward.
///
/// Only a view can reach SwiftUI's window actions, so every scene's root
/// view hands its action to this object (`MainWindowOpener`), which the
/// views get from their environment like the stores.
@MainActor
@Observable
final class WindowControl {
    @ObservationIgnored private let navigation: Navigation
    /// Brings the app forward as an ordinary app with a Dock icon.
    @ObservationIgnored private let activate: @MainActor () -> Void
    /// Opens the main window; set by the first view that appears.
    @ObservationIgnored var open: (() -> Void)?

    init(navigation: Navigation, activate: @escaping @MainActor () -> Void = { WindowControl.bringForward() }) {
        self.navigation = navigation
        self.activate = activate
    }

    /// Bring the main window to the front, opening it when it is closed,
    /// on the given screen when one is named.
    func show(_ pane: Pane? = nil) {
        if let pane {
            navigation.pane = pane
        }
        activate()
        open?()
    }

    static func bringForward() {
        NSApp.setActivationPolicy(.regular)
        NSApp.activate()
    }
}

/// The objects that live as long as the app does. Built once, and shared by
/// the scenes and the app delegate.
@MainActor
final class AppEnvironment {
    static let shared = AppEnvironment(options: LaunchOptions(process: .processInfo))

    let options: LaunchOptions
    let settings: AppSettings
    let navigation: Navigation
    let daemon: DaemonStore
    let library: LibraryStore
    let activity: ActivityStore
    let discover: DiscoverStore
    let storage: StorageStore
    let favorites: FavoritesStore
    let service: ServiceStore
    let loginItem: LoginItemStore
    let notifier: Notifier
    let windows: WindowControl
    let updates: UpdateStore
    /// This copy may replace the installed daemon by itself when it carries
    /// a newer one: the app as installed in /Applications, not a trial, a
    /// canned daemon or a build run from Xcode.
    let followsDaemon: Bool

    init(options: LaunchOptions) {
        self.options = options
        let client: any DaemonClient
        let control: any AgentControl
        if let scenario = options.fixture {
            // A UI test shows fixed data and must not read or change what the
            // user has set, so it gets preferences of its own.
            let suite = "io.descoped.dedupd.app.uitests"
            let defaults = UserDefaults(suiteName: suite) ?? .standard
            defaults.removePersistentDomain(forName: suite)
            settings = AppSettings(defaults: defaults, defaultAddress: FixtureData.address)
            client = FixtureDaemonClient(scenario: scenario)
            control = FixtureAgent(installed: scenario != .notRunning)
        } else {
            // The app that hosts the unit tests must not use up the owner's
            // first launch, so it keeps preferences of its own too.
            let hosted = options.isHostingUnitTests ? UserDefaults(suiteName: "io.descoped.dedupd.app.unittests") : nil
            // An address option without its value follows nothing either.
            let launchAddress = options.malformed.contains("-daemonAddress")
                ? "-daemonAddress without a value" : options.daemonAddress
            settings = AppSettings(defaults: hosted ?? .standard, launchAddress: launchAddress)
            client = LiveDaemonClient()
            // A trial names its home and its label. With either name there
            // and its value missing, the commands would run on the real
            // home, so none is run.
            let unfinished = options.malformed.filter { $0 == "-daemonHome" || $0 == "-agentLabel" }
            control = unfinished.isEmpty
                ? BundledAgent(home: options.agentHome, label: options.agentLabel)
                : RefusingAgent(why: "The app was started with \(unfinished.joined(separator: " and ")) and no value "
                    + "after it. Nothing is installed or changed; start it again with a value for each.")
        }
        navigation = Navigation(pane: options.pane ?? .library)
        windows = WindowControl(navigation: navigation)
        daemon = DaemonStore(client: client, settings: settings)
        library = LibraryStore(daemon: daemon)
        activity = ActivityStore(daemon: daemon)
        discover = DiscoverStore(daemon: daemon)
        storage = StorageStore(daemon: daemon)
        favorites = FavoritesStore(daemon: daemon)
        service = ServiceStore(daemon: daemon, control: control)
        // A canned daemon and the tests' host must not register anything
        // with macOS.
        let isReal = options.fixture == nil && !options.isHostingUnitTests
        loginItem = LoginItemStore(item: isReal ? SystemLoginItem() : FixtureLoginItem())
        notifier = Notifier(isLive: options.fixture == nil && !options.isHostingUnitTests)
        updates = UpdateStore(
            isAllowed: isReal,
            publicKey: Bundle.main.infoDictionary?["SUPublicEDKey"] as? String,
            feedOverride: options.updateFeed
        )
        followsDaemon = isReal && control is BundledAgent && options.agentHome == nil && options.agentLabel == nil
            && Bundle.main.bundleURL.deletingLastPathComponent().path == "/Applications"
        daemon.onJobsChanged = { [notifier, settings] before, after in
            if settings.notifications {
                notifier.jobsChanged(from: before, to: after)
            }
        }
    }

    /// Whether the main window opens by itself at launch: under a fixture,
    /// when asked for, and the first time the app runs. Otherwise the app
    /// starts as a menu bar icon only.
    var opensMainWindowAtLaunch: Bool {
        if options.isHostingUnitTests || options.showsPanelWindow {
            return false
        }
        return options.fixture != nil || options.showsMainWindow || settings.isFirstLaunch
    }

    /// Bring the main window to the front, opening it when it is closed.
    /// With a window open the app is an ordinary app with a Dock icon.
    func showMainWindow(_ pane: Pane? = nil) {
        windows.show(pane)
    }
}

extension View {
    /// Give a scene's root view the app-wide objects.
    @MainActor
    func appEnvironment(_ environment: AppEnvironment) -> some View {
        self
            .environment(environment.settings)
            .environment(environment.navigation)
            .environment(environment.daemon)
            .environment(environment.library)
            .environment(environment.activity)
            .environment(environment.discover)
            .environment(environment.storage)
            .environment(environment.favorites)
            .environment(environment.service)
            .environment(environment.loginItem)
            .environment(environment.windows)
            .environment(environment.updates)
    }
}
