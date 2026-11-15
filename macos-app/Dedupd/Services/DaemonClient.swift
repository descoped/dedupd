import DedupdClient
import Foundation

/// What the connection that follows the daemon reports.
enum DaemonSignal: Sendable, Equatable {
    /// The daemon answers.
    case connected
    /// The daemon is away. The connection keeps trying by itself.
    case disconnected
    /// The daemon's state as of now: its downloads, and a number that changes
    /// when the library does.
    case event(DaemonEvent)
    /// The daemon answers in a form this app cannot read: it is another
    /// build. The reason says what did not fit.
    case unreadable(String)
}

/// The daemon's control API as the app uses it. The live implementation calls
/// the Rust client; tests and UI tests use their own.
protocol DaemonClient: Sendable {
    /// What the daemon holds and what it is doing.
    func status(at address: String) async throws -> DaemonStatus
    /// Follow the daemon. The stream ends only when the caller stops reading.
    func events(at address: String) -> AsyncStream<DaemonSignal>
    /// Have the daemon look at the app folders now.
    func scan(at address: String) async throws -> ScanResult

    /// Every app, its installed version and the formats it takes.
    func apps(at address: String) async throws -> [AppInfo]
    func models(at address: String) async throws -> [LibraryModel]
    func model(_ repo: String, at address: String) async throws -> ModelDetail
    func locations(at address: String) async throws -> Locations

    func search(_ query: SearchQuery, at address: String) async throws -> SearchPage
    func hubModel(_ repo: String, at address: String) async throws -> HubModel

    /// One download with its files and its history.
    func download(_ id: Int64, at address: String) async throws -> JobDetail
    /// Put a download in the queue. Returns at once.
    func enqueue(_ request: PullRequest, at address: String) async throws -> Enqueued
    func downloadAction(_ action: JobAction, id: Int64, at address: String) async throws
    /// Take the finished downloads off the list; returns how many.
    func clearDownloads(at address: String) async throws -> UInt64
    /// What happened to the models in the apps, newest first.
    func activity(at address: String) async throws -> [ActivityEntry]

    func checkUpstream(_ repo: String, at address: String) async throws -> UpstreamCheck
    func forget(_ repo: String, at address: String) async throws
    /// What removing `repo` from `app` would delete; with `apply`, remove it.
    /// `paths` limits the removal to the files the owner saw and agreed to.
    func unplace(_ repo: String, app: ModelApp, paths: [String]?, apply: Bool, at address: String) async throws
        -> UnplacePlan
    /// What archiving a model would do: which apps it is taken out of and
    /// how much leaves the internal disk.
    func archivePlan(_ repo: String, at address: String) async throws -> ArchivePlan
    /// Start archiving a model. Returns at once; the events tell how it goes.
    func archive(_ repo: String, at address: String) async throws
    /// Start copying an archived model back. Returns at once.
    func restore(_ repo: String, at address: String) async throws

    func gcPlan(offload: Bool, at address: String) async throws -> GcPlan
    func gcApply(orphans: [String], offload: [String], at address: String) async throws -> GcPlan
    func startStoreCheck(at address: String) async throws
    func storeCheck(at address: String) async throws -> StoreCheck

    func settings(at address: String) async throws -> DaemonSettings
    func updateSettings(_ changes: [SettingChange], at address: String) async throws -> DaemonSettings
    func logTail(lines: UInt32, at address: String) async throws -> LogTail
    func restart(at address: String) async throws
}

/// Calls the daemon through the Rust client linked into the app.
struct LiveDaemonClient: DaemonClient {
    func status(at address: String) async throws -> DaemonStatus {
        try await Self.blocking { try DedupdClient.status(address: address) }
    }

    /// One thread in the Rust client sleeps in a read and wakes for each
    /// line; nothing is polled. Only the newest event is kept when the app
    /// is slower than the daemon, because each one carries the whole state.
    func events(at address: String) -> AsyncStream<DaemonSignal> {
        AsyncStream(bufferingPolicy: .bufferingNewest(4)) { continuation in
            let subscription = DedupdClient.subscribe(address: address, listener: SignalListener(continuation))
            continuation.onTermination = { _ in
                subscription.cancel()
            }
        }
    }

    func scan(at address: String) async throws -> ScanResult {
        try await Self.blocking { try DedupdClient.scan(address: address) }
    }

    func apps(at address: String) async throws -> [AppInfo] {
        try await Self.blocking { try DedupdClient.apps(address: address) }
    }

    func models(at address: String) async throws -> [LibraryModel] {
        try await Self.blocking { try DedupdClient.models(address: address) }
    }

    func model(_ repo: String, at address: String) async throws -> ModelDetail {
        try await Self.blocking { try DedupdClient.model(address: address, repo: repo) }
    }

    func locations(at address: String) async throws -> Locations {
        try await Self.blocking { try DedupdClient.locations(address: address) }
    }

    func search(_ query: SearchQuery, at address: String) async throws -> SearchPage {
        try await Self.blocking { try DedupdClient.search(address: address, query: query) }
    }

    func hubModel(_ repo: String, at address: String) async throws -> HubModel {
        try await Self.blocking { try DedupdClient.hubModel(address: address, repo: repo) }
    }

    func download(_ id: Int64, at address: String) async throws -> JobDetail {
        try await Self.blocking { try DedupdClient.download(address: address, id: id) }
    }

    func enqueue(_ request: PullRequest, at address: String) async throws -> Enqueued {
        try await Self.blocking {
            try DedupdClient.enqueue(
                address: address,
                model: request.model,
                apps: request.apps,
                include: request.include,
                offline: request.offline,
                kind: request.kind
            )
        }
    }

    func activity(at address: String) async throws -> [ActivityEntry] {
        try await Self.blocking { try DedupdClient.activity(address: address) }
    }

    func downloadAction(_ action: JobAction, id: Int64, at address: String) async throws {
        try await Self.blocking { try DedupdClient.downloadAction(address: address, id: id, action: action) }
    }

    func clearDownloads(at address: String) async throws -> UInt64 {
        try await Self.blocking { try DedupdClient.clearDownloads(address: address) }
    }

    func checkUpstream(_ repo: String, at address: String) async throws -> UpstreamCheck {
        try await Self.blocking { try DedupdClient.checkUpstream(address: address, repo: repo) }
    }

    func forget(_ repo: String, at address: String) async throws {
        try await Self.blocking { try DedupdClient.forget(address: address, repo: repo) }
    }

    func unplace(_ repo: String, app: ModelApp, paths: [String]?, apply: Bool, at address: String) async throws
        -> UnplacePlan {
        try await Self.blocking {
            try DedupdClient.unplace(address: address, repo: repo, app: app, apply: apply, paths: paths)
        }
    }

    func archivePlan(_ repo: String, at address: String) async throws -> ArchivePlan {
        try await Self.blocking { try DedupdClient.archivePlan(address: address, repo: repo) }
    }

    func archive(_ repo: String, at address: String) async throws {
        try await Self.blocking { try DedupdClient.archiveModel(address: address, repo: repo) }
    }

    func restore(_ repo: String, at address: String) async throws {
        try await Self.blocking { try DedupdClient.restoreModel(address: address, repo: repo) }
    }

    func gcPlan(offload: Bool, at address: String) async throws -> GcPlan {
        try await Self.blocking { try DedupdClient.gcPlan(address: address, offload: offload) }
    }

    func gcApply(orphans: [String], offload: [String], at address: String) async throws -> GcPlan {
        try await Self.blocking { try DedupdClient.gcApply(address: address, orphans: orphans, offload: offload) }
    }

    func startStoreCheck(at address: String) async throws {
        try await Self.blocking { try DedupdClient.startStoreCheck(address: address) }
    }

    func storeCheck(at address: String) async throws -> StoreCheck {
        try await Self.blocking { try DedupdClient.storeCheck(address: address) }
    }

    func settings(at address: String) async throws -> DaemonSettings {
        try await Self.blocking { try DedupdClient.settings(address: address) }
    }

    func updateSettings(_ changes: [SettingChange], at address: String) async throws -> DaemonSettings {
        try await Self.blocking { try DedupdClient.updateSettings(address: address, changes: changes) }
    }

    func logTail(lines: UInt32, at address: String) async throws -> LogTail {
        try await Self.blocking { try DedupdClient.logTail(address: address, lines: lines) }
    }

    func restart(at address: String) async throws {
        try await Self.blocking { try DedupdClient.restart(address: address) }
    }

    /// The Rust calls block until the daemon answers. They run on a dispatch
    /// queue so they never hold one of the few threads Swift concurrency
    /// shares between all tasks.
    private static func blocking<T: Sendable>(_ work: @escaping @Sendable () throws -> T) async throws -> T {
        try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                continuation.resume(with: Result { try work() })
            }
        }
    }
}

/// Hands what the Rust client's thread delivers to the stream the app reads.
private final class SignalListener: EventListener {
    private let continuation: AsyncStream<DaemonSignal>.Continuation

    init(_ continuation: AsyncStream<DaemonSignal>.Continuation) {
        self.continuation = continuation
    }

    func onEvent(event: DaemonEvent) {
        continuation.yield(.event(event))
    }

    func onConnection(connected: Bool) {
        continuation.yield(connected ? .connected : .disconnected)
    }

    func onUnreadable(reason: String) {
        continuation.yield(.unreadable(reason))
    }
}

extension ClientError {
    /// The message to show a person. The generated description is the Swift
    /// spelling of the case, which is of no use to them.
    var message: String {
        switch self {
        case let .NotRunning(address):
            "The dedupd daemon is not running at \(address)."
        case let .Failed(message):
            message
        case let .Protocol(message):
            "Unexpected answer from the daemon: \(message)"
        }
    }
}

/// The message for any error a client call can throw.
func describe(_ error: any Error) -> String {
    if let error = error as? ClientError {
        return error.message
    }
    if let error = error as? AgentError {
        return error.message
    }
    return error.localizedDescription
}
