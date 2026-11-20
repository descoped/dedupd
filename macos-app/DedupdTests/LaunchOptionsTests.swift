import DedupdClient
import Foundation
import Testing

@testable import Dedupd

@Suite("Launch options and the app-wide objects")
@MainActor
struct LaunchOptionsTests {
    @Test("A UI test names its canned daemon on the command line")
    func fixtureArgument() {
        #expect(LaunchOptions(arguments: ["Dedupd", "-uiTestFixture", "downloading"]).fixture == .downloading)
        #expect(LaunchOptions(arguments: ["Dedupd", "-uiTestFixture", "nonsense"]).fixture == nil)
        #expect(LaunchOptions(arguments: ["Dedupd", "-uiTestFixture"]).fixture == nil)
        #expect(LaunchOptions(arguments: ["Dedupd"]).fixture == nil)
    }

    @Test("The panel is shown in a window only when asked for")
    func panelWindow() {
        #expect(!LaunchOptions(arguments: ["Dedupd", "-uiTestFixture", "running"]).showsPanelWindow)
        #expect(LaunchOptions(arguments: ["Dedupd", "-panelWindow", "YES"]).showsPanelWindow)
        #expect(!LaunchOptions(arguments: ["Dedupd", "-panelWindow"]).showsPanelWindow, "an option is a name and a value")
        #expect(!LaunchOptions(arguments: ["Dedupd"]).showsPanelWindow)
    }

    @Test("A screen, a main window and a trial daemon can be asked for on the command line")
    func otherArguments() {
        let options = LaunchOptions(arguments: [
            "Dedupd", "-mainWindow", "YES", "-pane", "downloads", "-daemonHome", "/tmp/home", "-agentLabel", "io.descoped.dedupd.trial",
        ])
        #expect(options.showsMainWindow)
        #expect(options.pane == .downloads)
        #expect(options.agentHome == "/tmp/home")
        #expect(options.agentLabel == "io.descoped.dedupd.trial")
        #expect(LaunchOptions(arguments: ["Dedupd", "-pane", "nonsense"]).pane == nil)
        #expect(!LaunchOptions(arguments: ["Dedupd"]).showsMainWindow)
    }

    @Test("The app knows when it is hosting unit tests")
    func hostingTests() {
        #expect(LaunchOptions(environment: ["XCTestConfigurationFilePath": "/tmp/x"]).isHostingUnitTests)
        #expect(!LaunchOptions(environment: [:]).isHostingUnitTests)
        // This very process hosts the tests, so the app must have noticed.
        #expect(AppEnvironment.shared.options.isHostingUnitTests)
    }

    @Test("An option with its value missing is noticed, and never takes the next option's name")
    func missingValues() {
        let options = LaunchOptions(arguments: ["Dedupd", "-daemonHome", "-agentLabel", "x"])
        #expect(options.agentHome == nil)
        #expect(options.agentLabel == "x")
        #expect(options.malformed == ["-daemonHome"])
        #expect(LaunchOptions(arguments: ["Dedupd", "-agentLabel"]).malformed == ["-agentLabel"])
        #expect(LaunchOptions(arguments: ["Dedupd", "-daemonHome", "/tmp/h"]).malformed.isEmpty)
        #expect(LaunchOptions(arguments: ["Dedupd", "-daemonAddress", "127.0.0.1:8643"]).daemonAddress == "127.0.0.1:8643")
    }

    @Test("A trial with its home missing installs nothing")
    func unfinishedTrial() async {
        // As when hosting the tests, so the owner's preferences are left
        // alone; the agent control is built from the options either way.
        let environment = AppEnvironment(options: LaunchOptions(
            arguments: ["Dedupd", "-daemonHome"], environment: ["XCTestConfigurationFilePath": "/tmp/x"]
        ))
        await environment.service.loadAgent()
        #expect(environment.service.agent.failure?.contains("-daemonHome") == true)
        await environment.service.run(.install(setEnvironment: false))
        #expect(environment.daemon.problem?.contains("Nothing is installed") == true)
    }

    @Test("A canned daemon comes with preferences of its own, a canned client and a login item that is only remembered")
    func fixtureEnvironment() async {
        let environment = AppEnvironment(options: LaunchOptions(arguments: ["Dedupd", "-uiTestFixture", "running", "-pane", "storage"]))
        #expect(environment.settings.address == FixtureData.address)
        #expect(environment.navigation.pane == .storage)
        #expect(environment.opensMainWindowAtLaunch)
        #expect(environment.daemon.client is FixtureDaemonClient)
        environment.loginItem.set(true)
        #expect(environment.loginItem.isEnabled, "nothing was registered with macOS: the canned one remembers")
    }

    @Test("The main window opens by itself only when asked for, under a fixture or at the first launch")
    func opensAtLaunch() {
        func opens(_ arguments: [String], _ environment: [String: String] = [:]) -> Bool {
            AppEnvironment(options: LaunchOptions(arguments: ["Dedupd"] + arguments, environment: environment))
                .opensMainWindowAtLaunch
        }
        #expect(opens(["-uiTestFixture", "running"]))
        #expect(!opens(["-uiTestFixture", "running", "-panelWindow", "YES"]), "the panel in a window stands alone")
        #expect(!opens(["-mainWindow", "YES"], ["XCTestConfigurationFilePath": "/tmp/x"]), "never while hosting tests")
    }

    @Test("Showing the main window goes to the screen asked for and calls the opener")
    func windowControl() {
        let navigation = Navigation(pane: .library)
        var activated = 0
        var opened = 0
        let windows = WindowControl(navigation: navigation) { activated += 1 }
        // No view has appeared yet: the app comes forward and nothing traps.
        windows.show(.downloads)
        #expect(navigation.pane == .downloads)
        #expect((activated, opened) == (1, 0))
        windows.open = { opened += 1 }
        windows.show()
        #expect(navigation.pane == .downloads, "no screen named: it stays")
        #expect((activated, opened) == (2, 1))
    }

    @Test("The app has a Dock icon exactly while an ordinary window is open")
    func dockIcon() {
        #expect(DockIcon.isOrdinary(visible: true, titled: true, normalLevel: true, panel: false))
        #expect(!DockIcon.isOrdinary(visible: false, titled: true, normalLevel: true, panel: false))
        #expect(!DockIcon.isOrdinary(visible: true, titled: false, normalLevel: true, panel: false), "the menu bar panel")
        #expect(!DockIcon.isOrdinary(visible: true, titled: true, normalLevel: false, panel: false), "a status bar window")
        #expect(!DockIcon.isOrdinary(visible: true, titled: true, normalLevel: true, panel: true))
    }

    @Test("The login item's switch follows macOS, and a refusal is said where the switch is")
    func loginItem() {
        struct Refused: LocalizedError {
            var errorDescription: String? { "not allowed" }
        }
        let item = FixtureLoginItem()
        let store = LoginItemStore(item: item)
        #expect(!store.isEnabled)
        store.set(true)
        #expect(store.isEnabled && store.failure == nil)
        item.refusal = Refused()
        store.set(false)
        #expect(store.isEnabled, "macOS did not change it")
        #expect(store.failure == "macOS did not change the login item: not allowed")
        item.refusal = nil
        store.set(false)
        #expect(!store.isEnabled && store.failure == nil)
    }
}
