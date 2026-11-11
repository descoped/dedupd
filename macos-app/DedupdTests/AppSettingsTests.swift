import DedupdClient
import Foundation
import Testing

@testable import Dedupd

@Suite("Settings and launch options")
@MainActor
struct AppSettingsTests {
    @Test("Without a choice the daemon's default address is used")
    func defaultAddress() {
        let settings = makeSettings(address: "127.0.0.1:8642")
        #expect(settings.address == "127.0.0.1:8642")
        #expect(settings.customAddress.isEmpty)
    }

    @Test("The default comes from the Rust client, which shares it with the daemon")
    func rustDefault() {
        #expect(DedupdClient.defaultAddress() == "127.0.0.1:8642")
    }

    @Test("A chosen address is used, trimmed, and survives a restart")
    func custom() {
        let suite = "io.descoped.dedupd.tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite) ?? .standard
        defer { defaults.removePersistentDomain(forName: suite) }

        let settings = AppSettings(defaults: defaults, defaultAddress: "127.0.0.1:8642")
        settings.customAddress = "  127.0.0.1:9000 "
        #expect(settings.address == "127.0.0.1:9000")

        let again = AppSettings(defaults: defaults, defaultAddress: "127.0.0.1:8642")
        #expect(again.address == "127.0.0.1:9000")

        again.customAddress = "   "
        #expect(again.address == "127.0.0.1:8642")
        #expect(AppSettings(defaults: defaults, defaultAddress: "127.0.0.1:8642").customAddress.isEmpty)
    }

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
            "Dedupd", "-mainWindow", "YES", "-pane", "downloads", "-daemonHome", "/tmp/home", "-agentLabel", "com.dedupd.trial",
        ])
        #expect(options.showsMainWindow)
        #expect(options.pane == .downloads)
        #expect(options.agentHome == "/tmp/home")
        #expect(options.agentLabel == "com.dedupd.trial")
        #expect(LaunchOptions(arguments: ["Dedupd", "-pane", "nonsense"]).pane == nil)
        #expect(!LaunchOptions(arguments: ["Dedupd"]).showsMainWindow)
    }

    @Test("The row size and the notification choice are remembered")
    func preferences() {
        let suite = "io.descoped.dedupd.tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite) ?? .standard
        defer { defaults.removePersistentDomain(forName: suite) }

        let settings = AppSettings(defaults: defaults, defaultAddress: "127.0.0.1:8642")
        #expect(settings.isFirstLaunch)
        #expect(settings.density == .compact)
        #expect(settings.notifications)
        settings.density = .comfortable
        settings.notifications = false

        let again = AppSettings(defaults: defaults, defaultAddress: "127.0.0.1:8642")
        #expect(!again.isFirstLaunch)
        #expect(again.density == .comfortable)
        #expect(!again.notifications)
    }

    @Test("Where a model goes is remembered for each kind of model")
    func targets() {
        let suite = "io.descoped.dedupd.tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite) ?? .standard
        defer { defaults.removePersistentDomain(forName: suite) }

        let settings = AppSettings(defaults: defaults, defaultAddress: "127.0.0.1:8642")
        #expect(settings.apps(for: .gguf) == [.bionic, .ollama], "the usual apps until the owner chooses")
        #expect(settings.apps(for: .mlx) == [.bionic])

        settings.setApp(.bionic, for: .gguf, to: false)
        settings.setApp(.huggingFace, for: .mlx, to: true)

        let again = AppSettings(defaults: defaults, defaultAddress: "127.0.0.1:8642")
        #expect(again.apps(for: .gguf) == [.ollama])
        #expect(again.apps(for: .mlx) == [.bionic, .huggingFace])
        #expect(again.apps(for: .splitGGUF) == [.bionic], "a kind the owner never touched keeps its usual apps")
        // An app that cannot use a variant is left out, in the daemon's order.
        #expect(again.apps(for: .mlx, usable: [.bionic, .huggingFace]) == [.bionic, .huggingFace])
        #expect(again.apps(for: .gguf, usable: [.bionic, .huggingFace]).isEmpty)

        again.setApp(.ollama, for: .gguf, to: false)
        #expect(AppSettings(defaults: defaults, defaultAddress: "127.0.0.1:8642").apps(for: .gguf).isEmpty,
                "choosing no app is a choice too: the store only")
    }

    @Test("An address given on the command line is used for that run")
    func addressArgument() {
        // `-daemonAddress host:port` reaches the app through the argument
        // domain of UserDefaults; the suite here stands in for it.
        let suite = "io.descoped.dedupd.tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite) ?? .standard
        defer { defaults.removePersistentDomain(forName: suite) }
        defaults.set("127.0.0.1:8643", forKey: "daemonAddress")
        #expect(AppSettings(defaults: defaults, defaultAddress: "127.0.0.1:8642").address == "127.0.0.1:8643")
    }

    @Test("The app knows when it is hosting unit tests")
    func hostingTests() {
        #expect(LaunchOptions(environment: ["XCTestConfigurationFilePath": "/tmp/x"]).isHostingUnitTests)
        #expect(!LaunchOptions(environment: [:]).isHostingUnitTests)
        // This very process hosts the tests, so the app must have noticed.
        #expect(AppEnvironment.shared.options.isHostingUnitTests)
    }

    @Test("The canned daemons answer as their names say")
    func fixtures() async throws {
        let running = try await FixtureDaemonClient(scenario: .running).status(at: "a")
        #expect(running.downloadingTotal == 0)
        #expect(try await FixtureDaemonClient(scenario: .running).models(at: "a").count == 5)
        let downloading = try await FixtureDaemonClient(scenario: .downloading).status(at: "a")
        #expect(downloading.downloadingTotal > downloading.downloadingDone)
        await #expect(throws: ClientError.NotRunning(address: "a")) {
            try await FixtureDaemonClient(scenario: .notRunning).status(at: "a")
        }
    }
}
