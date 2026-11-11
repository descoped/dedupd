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
    let discover: DiscoverStore
    let storage: StorageStore
    let service: ServiceStore
    let notifier: Notifier

    /// Opens the main window. Set by the menu bar label, which is the one
    /// view that exists from launch and can reach SwiftUI's window actions.
    var openMainWindow: (() -> Void)?

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
            settings = AppSettings()
            client = LiveDaemonClient()
            control = BundledAgent(home: options.agentHome, label: options.agentLabel)
        }
        navigation = Navigation(pane: options.pane ?? .library)
        daemon = DaemonStore(client: client, settings: settings)
        library = LibraryStore(daemon: daemon)
        discover = DiscoverStore(daemon: daemon)
        storage = StorageStore(daemon: daemon)
        service = ServiceStore(daemon: daemon, control: control)
        notifier = Notifier(isLive: options.fixture == nil && !options.isHostingUnitTests)
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
        if let pane {
            navigation.pane = pane
        }
        NSApp.setActivationPolicy(.regular)
        openMainWindow?()
        NSApp.activate()
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
            .environment(environment.discover)
            .environment(environment.storage)
            .environment(environment.service)
    }
}

/// How the app was started.
///
/// Every option is written as a pair, `-name value`, the way AppKit reads
/// its own arguments. A name without a value in front of another option
/// shifts the pairs, the last word is then taken for a file to open, and an
/// app started to open a file shows no window of its own.
struct LaunchOptions: Equatable {
    /// The canned daemon a UI test asked for, with `-uiTestFixture <name>`.
    var fixture: FixtureScenario?
    /// Show the menu bar panel in an ordinary window, asked for with
    /// `-panelWindow YES`. A menu bar panel closes when it loses focus, so it
    /// can be neither driven by a UI test nor photographed.
    var showsPanelWindow = false
    /// Open the main window at launch, asked for with `-mainWindow YES`.
    var showsMainWindow = false
    /// The screen to start on, asked for with `-pane <name>`.
    var pane: Pane?
    /// Another home folder for the daemon the app installs: `-daemonHome <dir>`.
    var agentHome: String?
    /// Another launchd label for the daemon the app installs: `-agentLabel <label>`.
    var agentLabel: String?
    /// The app is hosting its unit tests and must leave the real daemon alone.
    var isHostingUnitTests = false

    init(arguments: [String] = [], environment: [String: String] = [:]) {
        func value(of flag: String) -> String? {
            guard let index = arguments.firstIndex(of: flag), arguments.indices.contains(index + 1) else {
                return nil
            }
            return arguments[index + 1]
        }
        fixture = value(of: "-uiTestFixture").flatMap(FixtureScenario.init(rawValue:))
        showsPanelWindow = value(of: "-panelWindow") == "YES"
        showsMainWindow = value(of: "-mainWindow") == "YES"
        pane = value(of: "-pane").flatMap(Pane.init(rawValue:))
        agentHome = value(of: "-daemonHome")
        agentLabel = value(of: "-agentLabel")
        isHostingUnitTests = environment["XCTestConfigurationFilePath"] != nil
    }

    init(process: ProcessInfo) {
        self.init(arguments: process.arguments, environment: process.environment)
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private var panelWindow: NSWindow?

    func applicationDidFinishLaunching(_ notification: Notification) {
        let environment = AppEnvironment.shared
        if environment.options.isHostingUnitTests {
            return
        }
        let center = NotificationCenter.default
        center.addObserver(self, selector: #selector(windowAppeared), name: NSWindow.didBecomeKeyNotification, object: nil)
        center.addObserver(self, selector: #selector(windowClosing), name: NSWindow.willCloseNotification, object: nil)
        environment.daemon.start()
        if environment.options.showsPanelWindow {
            showPanelInWindow(environment)
        }
    }

    /// Closing the last window leaves the app in the menu bar.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }

    /// A click on the Dock icon with no window open opens the main window.
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows: Bool) -> Bool {
        if !hasVisibleWindows {
            AppEnvironment.shared.showMainWindow()
        }
        return true
    }

    // MARK: - Dock icon

    /// The app has a Dock icon exactly while it has an ordinary window open.
    @objc
    private func windowAppeared(_ notification: Notification) {
        updateDockIcon(closing: nil)
    }

    @objc
    private func windowClosing(_ notification: Notification) {
        updateDockIcon(closing: notification.object as? NSWindow)
    }

    private func updateDockIcon(closing: NSWindow?) {
        let hasWindow = NSApp.windows.contains { window in
            window !== closing && Self.isOrdinary(window)
        }
        let wanted: NSApplication.ActivationPolicy = hasWindow ? .regular : .accessory
        if NSApp.activationPolicy() != wanted {
            NSApp.setActivationPolicy(wanted)
        }
    }

    /// A window the owner opened, as opposed to the menu bar panel and the
    /// windows the system keeps for the menu bar.
    private static func isOrdinary(_ window: NSWindow) -> Bool {
        window.isVisible && window.styleMask.contains(.titled) && window.level == .normal && !(window is NSPanel)
    }

    // MARK: - The panel in a window

    private func showPanelInWindow(_ environment: AppEnvironment) {
        let panel = MenuPanel().appEnvironment(environment)
        let window = NSWindow(contentViewController: NSHostingController(rootView: panel))
        window.title = "dedupd menu"
        window.styleMask = [.titled, .closable]
        window.identifier = NSUserInterfaceItemIdentifier("panel-window")
        window.center()
        NSApp.setActivationPolicy(.regular)
        window.makeKeyAndOrderFront(nil)
        NSApp.activate()
        panelWindow = window
    }
}
