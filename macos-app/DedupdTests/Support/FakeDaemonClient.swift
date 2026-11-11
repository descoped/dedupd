import DedupdClient
import Foundation

@testable import Dedupd

/// A daemon whose answers a test sets, and which records what it was asked.
actor FakeDaemonClient: DaemonClient {
    enum Call: Hashable {
        case status, pull, scan
    }

    private var statusAnswer: Result<DaemonStatus, ClientError>
    private var pullAnswer: Result<PullResult, ClientError> = .failure(.Failed(message: "no pull answer set"))
    private var scanAnswer: Result<ScanResult, ClientError> = .failure(.Failed(message: "no scan answer set"))

    private(set) var statusAddresses: [String] = []
    private(set) var pulls: [PullRequest] = []
    private(set) var scans = 0

    private var held: Set<Call> = []
    private var waiting: [Call: [CheckedContinuation<Void, Never>]] = [:]

    init(status: Result<DaemonStatus, ClientError> = .success(FixtureDaemonClient.status())) {
        statusAnswer = status
    }

    func answerStatus(_ answer: Result<DaemonStatus, ClientError>) {
        statusAnswer = answer
    }

    func answerPull(_ answer: Result<PullResult, ClientError>) {
        pullAnswer = answer
    }

    func answerScan(_ answer: Result<ScanResult, ClientError>) {
        scanAnswer = answer
    }

    /// Make calls of this kind wait until `release` is called, so a test can
    /// look at the store while the call is in flight.
    func hold(_ call: Call) {
        held.insert(call)
    }

    func release(_ call: Call) {
        held.remove(call)
        for continuation in waiting.removeValue(forKey: call) ?? [] {
            continuation.resume()
        }
    }

    private func pause(_ call: Call) async {
        guard held.contains(call) else {
            return
        }
        await withCheckedContinuation { continuation in
            waiting[call, default: []].append(continuation)
        }
    }

    func status(at address: String) async throws -> DaemonStatus {
        statusAddresses.append(address)
        await pause(.status)
        return try statusAnswer.get()
    }

    func pull(_ request: PullRequest, at address: String) async throws -> PullResult {
        pulls.append(request)
        await pause(.pull)
        return try pullAnswer.get()
    }

    func scan(at address: String) async throws -> ScanResult {
        scans += 1
        await pause(.scan)
        return try scanAnswer.get()
    }
}

/// Preferences that live only for one test.
@MainActor
func makeSettings(address: String = "127.0.0.1:8642") -> AppSettings {
    let suite = "io.descoped.dedupd.tests.\(UUID().uuidString)"
    let defaults = UserDefaults(suiteName: suite) ?? .standard
    defaults.removePersistentDomain(forName: suite)
    return AppSettings(defaults: defaults, defaultAddress: address)
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
