import DedupdClient
import Foundation
import Synchronization

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

    /// The favorite models, and how the likes on Hugging Face are brought in.
    func favorites(at address: String) async throws -> Favorites
    func setFavorite(_ repo: String, _ favorite: Bool, at address: String) async throws -> Favorites
    /// Read the likes on Hugging Face now.
    func syncFavorites(at address: String) async throws -> Favorites

    func checkUpstream(_ repo: String, at address: String) async throws -> UpstreamCheck
    func forget(_ repo: String, at address: String) async throws
    /// What removing `repo` from `app` would delete; with `apply`, remove it.
    /// `paths` limits the removal to the files the owner saw and agreed to.
    func unplace(_ repo: String, app: ModelApp, paths: [String]?, apply: Bool, at address: String) async throws
        -> UnplacePlan
    /// What archiving a model to the archive named `to` would do (nil: the
    /// backup archive, or the only one): which apps it is taken out of and
    /// how much leaves the internal disk.
    func archivePlan(_ repo: String, to archive: String?, at address: String) async throws -> ArchivePlan
    /// Start archiving a model. Returns at once; the events tell how it goes.
    func archive(_ repo: String, to archive: String?, at address: String) async throws
    /// Start copying an archived model back. Returns at once.
    func restore(_ repo: String, at address: String) async throws

    func gcPlan(offload: Bool, at address: String) async throws -> GcPlan
    func gcApply(orphans: [String], offload: [String], at address: String) async throws -> GcPlan
    func startStoreCheck(at address: String) async throws
    func storeCheck(at address: String) async throws -> StoreCheck

    func settings(at address: String) async throws -> DaemonSettings
    func updateSettings(_ changes: [SettingChange], at address: String) async throws -> DaemonSettings
    /// Set the whole list of archives and which one is the backup (by its
    /// folder). It takes effect at once.
    func setArchives(_ archives: [ArchiveEntry], backup: String?, at address: String) async throws -> DaemonSettings
    func logTail(lines: UInt32, at address: String) async throws -> LogTail
    func restart(at address: String) async throws
}

extension DaemonClient {
    /// Archive to the backup archive, or the only one.
    func archive(_ repo: String, at address: String) async throws {
        try await archive(repo, to: nil, at: address)
    }

    func archivePlan(_ repo: String, at address: String) async throws -> ArchivePlan {
        try await archivePlan(repo, to: nil, at: address)
    }
}

/// Calls the daemon through the Rust client linked into the app.
struct LiveDaemonClient: DaemonClient {
    func status(at address: String) async throws -> DaemonStatus {
        try await Self.blocking { try DedupdClient.status(address: address) }
    }

    /// One thread in the Rust client sleeps in a read and wakes for each
    /// line; nothing is polled. When the app is slower than the daemon, an
    /// event replaces the one still waiting, because each carries the whole
    /// state; a connection made or lost is never dropped (`SignalMailbox`).
    func events(at address: String) -> AsyncStream<DaemonSignal> {
        let following = Following(address: address)
        return AsyncStream(unfolding: { await following.mailbox.next() })
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

    func favorites(at address: String) async throws -> Favorites {
        try await Self.blocking { try DedupdClient.favorites(address: address) }
    }

    func setFavorite(_ repo: String, _ favorite: Bool, at address: String) async throws -> Favorites {
        try await Self.blocking { try DedupdClient.setFavorite(address: address, repo: repo, favorite: favorite) }
    }

    func syncFavorites(at address: String) async throws -> Favorites {
        try await Self.blocking { try DedupdClient.syncFavorites(address: address) }
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

    func archivePlan(_ repo: String, to archive: String?, at address: String) async throws -> ArchivePlan {
        try await Self.blocking { try DedupdClient.archivePlan(address: address, repo: repo, to: archive) }
    }

    func archive(_ repo: String, to archive: String?, at address: String) async throws {
        try await Self.blocking { try DedupdClient.archiveModel(address: address, repo: repo, to: archive) }
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

    func setArchives(_ archives: [ArchiveEntry], backup: String?, at address: String) async throws -> DaemonSettings {
        try await Self.blocking { try DedupdClient.setArchives(address: address, archives: archives, backup: backup) }
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

/// What the connection delivered and the app has not read yet.
///
/// The Rust client's thread puts; the app takes one at a time. An event
/// carries the daemon's whole state, so a newer one replaces one that still
/// waits. The other signals are edges (the daemon came, went, or cannot be
/// read): each is kept, in order, so the app never misses that the daemon
/// went away and came back. Two equal edges in a row are one.
final class SignalMailbox: Sendable {
    private struct Held {
        var waiting: [DaemonSignal] = []
        var reader: CheckedContinuation<DaemonSignal?, Never>?
        var closed = false
    }

    private let held = Mutex(Held())

    func put(_ signal: DaemonSignal) {
        let reader = held.withLock { held -> CheckedContinuation<DaemonSignal?, Never>? in
            guard !held.closed else {
                return nil
            }
            if let reader = held.reader {
                held.reader = nil
                return reader
            }
            switch (held.waiting.last, signal) {
            case (.event, .event):
                held.waiting[held.waiting.count - 1] = signal
            case let (last?, _) where last == signal:
                break
            default:
                held.waiting.append(signal)
            }
            return nil
        }
        reader?.resume(returning: signal)
    }

    /// The next signal; nil once the mailbox is closed. One reader at a
    /// time.
    func next() async -> DaemonSignal? {
        await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                let ready = held.withLock { held -> DaemonSignal?? in
                    if !held.waiting.isEmpty {
                        return .some(held.waiting.removeFirst())
                    }
                    if held.closed {
                        return .some(nil)
                    }
                    held.reader = continuation
                    return nil
                }
                if let ready {
                    continuation.resume(returning: ready)
                }
            }
        } onCancel: {
            close()
        }
    }

    /// Nothing more is delivered; a reader that waits gets nil.
    func close() {
        let reader = held.withLock { held -> CheckedContinuation<DaemonSignal?, Never>? in
            held.closed = true
            held.waiting = []
            defer { held.reader = nil }
            return held.reader
        }
        reader?.resume(returning: nil)
    }
}

/// One subscription to the daemon's events, for as long as its stream is
/// read: it ends when the stream is let go or its reader is cancelled.
private final class Following: Sendable {
    let mailbox = SignalMailbox()
    private let subscription: EventSubscription

    init(address: String) {
        subscription = DedupdClient.subscribe(address: address, listener: SignalListener(mailbox))
    }

    deinit {
        subscription.cancel()
        mailbox.close()
    }
}

/// Hands what the Rust client's thread delivers to the mailbox the app reads.
private final class SignalListener: EventListener {
    private let mailbox: SignalMailbox

    init(_ mailbox: SignalMailbox) {
        self.mailbox = mailbox
    }

    func onEvent(event: DaemonEvent) {
        mailbox.put(.event(event))
    }

    func onConnection(connected: Bool) {
        mailbox.put(connected ? .connected : .disconnected)
    }

    func onUnreadable(reason: String) {
        mailbox.put(.unreadable(reason))
    }
}

extension ClientError {
    /// The message to show a person: the sentence the Rust client has for
    /// the error (the command line prints the same one), with a capital
    /// and a full stop. What the daemon said itself is shown as it came.
    var message: String {
        if case let .Failed(message) = self {
            return message
        }
        let sentence = errorMessage(error: self)
        let capital = sentence.prefix(1).uppercased() + sentence.dropFirst()
        return capital.hasSuffix(".") ? capital : capital + "."
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
