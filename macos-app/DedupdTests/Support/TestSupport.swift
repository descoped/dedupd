import DedupdClient
import Foundation
import Synchronization

@testable import Dedupd

/// The preference suites the tests make. Their files are removed when the
/// test process ends; the few the system writes after that are removed at
/// the start of the next run, so they never pile up in ~/Library/Preferences.
private enum TestSuites {
    static let prefix = "io.descoped.dedupd.tests."
    private static let names = Mutex<[String]>([])
    private static let folder = FileManager.default.homeDirectoryForCurrentUser.appending(path: "Library/Preferences")

    static func add(_ name: String) {
        names.withLock { names in
            if names.isEmpty {
                removeLeftovers()
                atexit { TestSuites.removeOwn() }
            }
            names.append(name)
        }
    }

    private static func removeLeftovers() {
        let files = (try? FileManager.default.contentsOfDirectory(atPath: folder.path)) ?? []
        for file in files where file.hasPrefix(prefix) && file.hasSuffix(".plist") {
            try? FileManager.default.removeItem(at: folder.appending(path: file))
        }
    }

    private static func removeOwn() {
        for name in names.withLock({ $0 }) {
            // Have the system write what it holds first, so it is less
            // likely to write the file again after it is removed.
            UserDefaults(suiteName: name)?.synchronize()
            try? FileManager.default.removeItem(at: folder.appending(path: "\(name).plist"))
        }
    }
}

/// Preferences that live only for one test.
@MainActor
func makeSettings(address: String = "127.0.0.1:8642") -> AppSettings {
    let suite = TestSuites.prefix + UUID().uuidString
    let defaults = UserDefaults(suiteName: suite) ?? .standard
    TestSuites.add(suite)
    return AppSettings(defaults: defaults, defaultAddress: address)
}

/// A store on a canned daemon. The canned daemon keeps a download queue of
/// its own, so what a test asks of it comes back as an event, as with the
/// real one.
@MainActor
func makeDaemon(_ scenario: FixtureScenario = .running) -> DaemonStore {
    DaemonStore(client: FixtureDaemonClient(scenario: scenario), settings: makeSettings())
}

/// The same, already following its daemon.
@MainActor
func startedDaemon(_ scenario: FixtureScenario = .running) async -> DaemonStore {
    let daemon = makeDaemon(scenario)
    daemon.start()
    _ = await eventually { await daemon.connection != .connecting }
    return daemon
}

/// Wait until `condition` holds, giving other tasks time to run. Fails the
/// wait after two seconds rather than hanging the suite.
func eventually(_ condition: @Sendable () async -> Bool) async -> Bool {
    for _ in 0 ..< 400 {
        if await condition() {
            return true
        }
        try? await Task.sleep(for: .milliseconds(5))
    }
    return false
}

/// A download as the daemon would report it.
func job(_ id: Int64, _ state: JobState, model: String = "org/Repo-GGUF:Q4_K_M", done: UInt64 = 0) -> Job {
    FixtureData.job(
        id: id,
        request: PullRequest(model: model, apps: ["bionic", "ollama"]),
        state: state,
        total: 100_000_000,
        done: done,
        speed: state == .downloading ? 10_000_000 : 0
    )
}

/// An event that carries these downloads.
func event(_ jobs: [Job], library: UInt64 = 1, seq: UInt64 = 1) -> DaemonEvent {
    DaemonEvent(
        seq: seq,
        library: library,
        checking: false,
        moving: nil,
        downloads: Downloads(
            governor: Governor(connections: 1, window: 4, maxConnections: 8, holdSeconds: 0, speedLimit: 0, budgets: []),
            speed: jobs.map(\.speed).reduce(0, +),
            jobs: jobs
        )
    )
}
