import AppKit
import SwiftUI

/// The objects that live as long as the app does. Built once, and shared by
/// the scenes and the app delegate.
@MainActor
final class AppEnvironment {
    static let shared = AppEnvironment(options: LaunchOptions(process: .processInfo))

    let options: LaunchOptions
    let settings: AppSettings
    let store: DaemonStore

    init(options: LaunchOptions) {
        self.options = options
        if let scenario = options.fixture {
            // A UI test shows fixed data and must not read or change what the
            // user has set, so it gets preferences of its own.
            let suite = "io.descoped.dedupd.app.uitests"
            let defaults = UserDefaults(suiteName: suite) ?? .standard
            defaults.removePersistentDomain(forName: suite)
            settings = AppSettings(defaults: defaults)
            store = DaemonStore(client: FixtureDaemonClient(scenario: scenario), settings: settings)
        } else {
            settings = AppSettings()
            store = DaemonStore(client: LiveDaemonClient(), settings: settings)
        }
    }
}

/// How the app was started.
struct LaunchOptions: Equatable {
    /// The canned daemon a UI test asked for, with `-uiTestFixture <name>`.
    var fixture: FixtureScenario?
    /// Also show the panel in an ordinary window: asked for with
    /// `-panelWindow`, and always under a fixture. A menu bar panel closes
    /// when it loses focus, so it can be neither driven by a UI test nor
    /// kept open beside the code while working on it.
    var showsPanelWindow = false
    /// The app is hosting its unit tests and must leave the real daemon alone.
    var isHostingUnitTests = false

    init(arguments: [String] = [], environment: [String: String] = [:]) {
        if let flag = arguments.firstIndex(of: "-uiTestFixture"), arguments.indices.contains(flag + 1) {
            fixture = FixtureScenario(rawValue: arguments[flag + 1])
        }
        showsPanelWindow = fixture != nil || arguments.contains("-panelWindow")
        isHostingUnitTests = environment["XCTestConfigurationFilePath"] != nil
    }

    init(process: ProcessInfo) {
        self.init(arguments: process.arguments, environment: process.environment)
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private var fixtureWindow: NSWindow?

    func applicationDidFinishLaunching(_ notification: Notification) {
        let environment = AppEnvironment.shared
        if environment.options.isHostingUnitTests {
            return
        }
        environment.store.startPolling()
        if environment.options.showsPanelWindow {
            showPanelInWindow(environment)
        }
    }

    private func showPanelInWindow(_ environment: AppEnvironment) {
        let panel = StatusPanel()
            .environment(environment.store)
            .environment(environment.settings)
        let window = NSWindow(contentViewController: NSHostingController(rootView: panel))
        window.title = "dedupd"
        window.styleMask = [.titled, .closable]
        window.identifier = NSUserInterfaceItemIdentifier("panel-window")
        window.center()
        NSApp.setActivationPolicy(.regular)
        window.makeKeyAndOrderFront(nil)
        NSApp.activate()
        fixtureWindow = window
    }
}
