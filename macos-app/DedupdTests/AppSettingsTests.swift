import DedupdClient
import Foundation
import Testing

@testable import Dedupd

@Suite("Settings")
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

        // A saved address that is not one is not used.
        again.customAddress = "http://127.0.0.1:9000"
        #expect(again.address == "127.0.0.1:8642")

        again.customAddress = "   "
        #expect(again.address == "127.0.0.1:8642")
        #expect(AppSettings(defaults: defaults, defaultAddress: "127.0.0.1:8642").customAddress.isEmpty)
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

    @Test("Where a model goes is remembered for each format")
    func targets() {
        let suite = "io.descoped.dedupd.tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite) ?? .standard
        defer { defaults.removePersistentDomain(forName: suite) }
        let known = RecordedProfiles.apps

        let settings = AppSettings(defaults: defaults, defaultAddress: "127.0.0.1:8642")
        #expect(settings.apps(for: .gguf, known: known) == [.bionic, .ollama], "the daemon's defaults until the owner chooses")
        #expect(settings.apps(for: .mlx, known: known) == [.bionic])

        settings.setApp(.bionic, for: .gguf, to: false, known: known)
        settings.setApp(.huggingFace, for: .mlx, to: true, known: known)

        let again = AppSettings(defaults: defaults, defaultAddress: "127.0.0.1:8642")
        #expect(again.apps(for: .gguf, known: known) == [.ollama])
        #expect(again.apps(for: .mlx, known: known) == [.bionic, .huggingFace])
        #expect(again.apps(for: .other, known: known) == [.huggingFace], "a format the owner never touched keeps its defaults")
        // An app that cannot use a variant is left out, in the daemon's order.
        #expect(again.apps(for: .mlx, usable: [.bionic, .huggingFace], known: known) == [.bionic, .huggingFace])
        #expect(again.apps(for: .gguf, usable: [.bionic, .huggingFace], known: known).isEmpty)

        again.setApp(.ollama, for: .gguf, to: false, known: known)
        #expect(AppSettings(defaults: defaults, defaultAddress: "127.0.0.1:8642").apps(for: .gguf, known: known).isEmpty,
                "choosing no app is a choice too: the store only")
    }

    @Test("An address given on the command line is used for that run and never saved")
    func addressArgument() {
        let suite = "io.descoped.dedupd.tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite) ?? .standard
        defer { defaults.removePersistentDomain(forName: suite) }
        // The owner chose an address earlier; the run is given another.
        AppSettings(defaults: defaults, defaultAddress: "127.0.0.1:8642").customAddress = "127.0.0.1:9000"

        let run = AppSettings(defaults: defaults, defaultAddress: "127.0.0.1:8642", launchAddress: "127.0.0.1:8643")
        #expect(run.address == "127.0.0.1:8643")
        #expect(run.customAddress.isEmpty, "it is not shown as if it had been typed")
        #expect(AppSettings(defaults: defaults, defaultAddress: "127.0.0.1:8642").address == "127.0.0.1:9000",
                "the saved choice is there again in the next run")

        // One that is not an address of this Mac follows no daemon: the
        // run never falls back to the saved one, which may be the real
        // daemon's.
        let other = AppSettings(defaults: defaults, defaultAddress: "127.0.0.1:8642", launchAddress: "example.com:80")
        #expect(other.address == "example.com:80")
        #expect(!addressIsValid(address: other.address))
    }

    @Test("Only an address of this Mac itself is used")
    func loopbackOnly() {
        let settings = makeSettings(address: "127.0.0.1:8642")
        for other in ["192.168.1.10:8642", "mac-studio.local:8642", "127.0.0.1:8642/x", "localhost:0"] {
            settings.customAddress = other
            #expect(settings.address == "127.0.0.1:8642", "\(other)")
        }
        settings.customAddress = "localhost:9000"
        #expect(settings.address == "localhost:9000")
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
