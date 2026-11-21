import DedupdClient
import Foundation
import Testing
@testable import Dedupd

@Suite("One version, and the updater")
struct VersionTests {
    @Test("Versions compare by the numbers between the dots")
    func comparesVersions() {
        #expect(AppVersion.isOlder("0.2.0", than: "0.3.0"))
        #expect(AppVersion.isOlder("0.9.0", than: "0.10.0"))
        #expect(AppVersion.isOlder("1.2", than: "1.2.1"))
        #expect(!AppVersion.isOlder("1.2", than: "1.2.0"))
        #expect(!AppVersion.isOlder("0.3.0", than: "0.3.0"))
        #expect(!AppVersion.isOlder("0.10.0", than: "0.9.0"))
    }

    @Test("The app's version and build number are the one version")
    func oneVersion() {
        let info = Bundle.main.infoDictionary ?? [:]
        let version = info["CFBundleShortVersionString"] as? String
        #expect(version == AppVersion.current)
        #expect(info["CFBundleVersion"] as? String == version, "the build number is the version, so each release is greater")
        #expect(AppVersion.title == "dedupd \(AppVersion.current)")
    }

    @Test("A trial's feed is taken only from this Mac")
    func trialFeed() {
        #expect(UpdateStore.trialFeed("http://127.0.0.1:8000/appcast.xml") == "http://127.0.0.1:8000/appcast.xml")
        #expect(UpdateStore.trialFeed("http://localhost:8000/appcast.xml") != nil)
        #expect(UpdateStore.trialFeed("https://example.com/appcast.xml") == nil)
        #expect(UpdateStore.trialFeed("http://127.0.0.1.example.com/appcast.xml") == nil)
        #expect(UpdateStore.trialFeed("file:///tmp/appcast.xml") == nil)
        #expect(UpdateStore.trialFeed(nil) == nil)
        let options = LaunchOptions(arguments: ["Dedupd", "-updateFeed", "https://evil.example/appcast.xml"])
        #expect(options.updateFeed == nil)
    }

    @MainActor
    @Test("Without the update key, or in a canned run, the app does not check")
    func notAvailable() {
        #expect(!UpdateStore(isAllowed: true, publicKey: "", feedOverride: nil).isAvailable)
        #expect(!UpdateStore(isAllowed: true, publicKey: "$(SPARKLE_PUBLIC_KEY)", feedOverride: nil).isAvailable)
        #expect(!UpdateStore(isAllowed: false, publicKey: "abc=", feedOverride: nil).isAvailable)
    }

    @MainActor
    @Test("After the app is updated, an older installed daemon is replaced; another build of one version is not")
    func daemonFollowsTheApp() async {
        let older = RecordingAgent(installedVersion: "0.0.9")
        let service = ServiceStore(daemon: await startedDaemon(.running), control: older)
        await service.followAppVersion()
        #expect(await older.installs == 1)
        #expect(service.followed == "The daemon was updated from 0.0.9 to \(AppVersion.current) together with this app.")
        #expect(service.followed == UpdateWords.daemonFollowed(from: "0.0.9", to: AppVersion.current))

        let same = RecordingAgent(installedVersion: AppVersion.current)
        let untouched = ServiceStore(daemon: makeDaemon(.running), control: same)
        await untouched.followAppVersion()
        #expect(await same.installs == 0)
        #expect(untouched.followed == nil)

        let waiting = RecordingAgent(installedVersion: "0.0.9")
        let daemon = makeDaemon(.running)
        let patient = ServiceStore(daemon: daemon, control: waiting)
        let follow = Task { await patient.followAppVersion() }
        // Until the daemon has been heard from, a running archive or restore
        // cannot be ruled out, so nothing is installed.
        try? await Task.sleep(for: .milliseconds(200))
        #expect(await waiting.installs == 0)
        daemon.start()
        await follow.value
        #expect(await waiting.installs == 1)

        let newer = RecordingAgent(installedVersion: "99.0.0")
        await ServiceStore(daemon: makeDaemon(.running), control: newer).followAppVersion()
        #expect(await newer.installs == 0, "a newer installed daemon is never replaced by an older one")
    }
}

/// An agent whose installed daemon is of a given version, and that counts
/// the installs; an install brings it to the app's version.
private actor RecordingAgent: AgentControl {
    var installedVersion: String
    private(set) var installs = 0

    init(installedVersion: String) {
        self.installedVersion = installedVersion
    }

    func state() async throws -> AgentState {
        AgentState(
            label: "io.descoped.dedupd.daemon", plist: "/Users/example/Library/LaunchAgents/x.plist", installed: true,
            running: true, home: "/Users/example/.dedupd", program: "/Users/example/.dedupd/bin/dedupd",
            installedVersion: installedVersion, version: AppVersion.current, log: "/Users/example/.dedupd/daemon.log"
        )
    }

    func install(setEnvironment: Bool) async throws -> String {
        installs += 1
        installedVersion = AppVersion.current
        return "Installed and started the daemon."
    }

    func uninstall() async throws -> String { "" }
    func start() async throws -> String { "" }
    func stop() async throws -> String { "" }
}
