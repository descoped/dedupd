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

    @Test("The panel is shown in a window under a fixture and when asked for")
    func panelWindow() {
        #expect(LaunchOptions(arguments: ["Dedupd", "-uiTestFixture", "running"]).showsPanelWindow)
        #expect(LaunchOptions(arguments: ["Dedupd", "-panelWindow"]).showsPanelWindow)
        #expect(!LaunchOptions(arguments: ["Dedupd"]).showsPanelWindow)
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
        #expect(running.repositories.count == 4)
        let downloading = try await FixtureDaemonClient(scenario: .downloading).status(at: "a")
        #expect(downloading.downloadingTotal > downloading.downloadingDone)
        await #expect(throws: ClientError.NotRunning(address: "a")) {
            try await FixtureDaemonClient(scenario: .notRunning).status(at: "a")
        }
    }
}
