import DedupdClient
import Foundation

/// The canned daemons a UI test can ask for with `-uiTestFixture <name>`.
enum FixtureScenario: String, CaseIterable, Sendable {
    /// A daemon with five models, an attached archive and nothing in flight.
    case running
    /// The same daemon in the middle of a download, with one that needs
    /// attention and one that is done.
    case downloading
    /// No daemon answers.
    case notRunning
}

/// A daemon that answers from fixed data, for UI tests and previews. It never
/// touches the network. Its download queue is alive: what the app asks of it
/// shows up in the next event, as with the real daemon.
actor FixtureDaemonClient: DaemonClient {
    let scenario: FixtureScenario
    var jobs: [Job]
    private var settings = FixtureData.settings
    private var seq: UInt64 = 0
    var nextID: Int64 = 20
    var history: [ActivityEntry] = FixtureData.activity
    var favoriteList: Favorites = FixtureData.favorites
    /// The library as it is now. What the owner does to a model (activate,
    /// deactivate, archive, restore, forget) changes it here, and every
    /// answer about the library, the storage and the status is made from
    /// it, so the screens agree with each other as with the real daemon.
    var models = FixtureData.models
    /// The owner removed the orphans, or the local copies the archive has.
    private var cleaned = false
    var offloaded = false
    /// The last archive or restore, as the daemon would tell it.
    var moving: ModelMove?
    var library: UInt64 = 1
    private var listeners: [UUID: AsyncStream<DaemonSignal>.Continuation] = [:]

    init(scenario: FixtureScenario) {
        self.scenario = scenario
        jobs = scenario == .downloading ? FixtureData.jobs : []
    }

    /// The calls a test holds back, by name (`models(at:)`), and the ones
    /// that wait to go on.
    private var held: Set<String> = []
    private var waiting: [String: [CheckedContinuation<String?, Never>]] = [:]

    /// Hold every later call of this name until it is released, so a test
    /// can have answers come back late or in another order than asked.
    func hold(_ call: String) {
        held.insert(call)
    }

    /// How many calls of this name wait.
    func waiting(_ call: String) -> Int {
        waiting[call]?.count ?? 0
    }

    /// Let one waiting call go on: the oldest, or the newest with `last`.
    /// With `failing` it fails with that message instead of answering.
    func release(_ call: String, last: Bool = false, failing: String? = nil) {
        guard var queue = waiting[call], !queue.isEmpty else {
            return
        }
        let one = last ? queue.removeLast() : queue.removeFirst()
        waiting[call] = queue
        one.resume(returning: failing)
    }

    /// Stop holding calls of this name and let those that wait go on.
    func releaseAll(_ call: String) {
        held.remove(call)
        for one in waiting.removeValue(forKey: call) ?? [] {
            one.resume(returning: nil)
        }
    }

    func answering(_ address: String, call: String = #function) async throws {
        if held.contains(call) {
            let failure = await withCheckedContinuation { waiting[call, default: []].append($0) }
            if let failure {
                throw ClientError.Failed(message: failure)
            }
        }
        if scenario == .notRunning {
            throw ClientError.NotRunning(address: address)
        }
    }

    // MARK: - Following

    nonisolated func events(at address: String) -> AsyncStream<DaemonSignal> {
        AsyncStream { continuation in
            let id = UUID()
            continuation.onTermination = { _ in
                Task { await self.detach(id) }
            }
            Task { await self.attach(id, continuation) }
        }
    }

    private func attach(_ id: UUID, _ continuation: AsyncStream<DaemonSignal>.Continuation) {
        guard scenario != .notRunning else {
            continuation.yield(.disconnected)
            return
        }
        listeners[id] = continuation
        continuation.yield(.connected)
        continuation.yield(.event(event()))
    }

    private func detach(_ id: UUID) {
        listeners[id] = nil
    }

    private func event() -> DaemonEvent {
        DaemonEvent(seq: seq, library: library, checking: false, moving: moving, downloads: downloads())
    }

    private func downloads() -> Downloads {
        let running = jobs.filter { $0.state == .downloading }
        return Downloads(
            governor: Governor(
                connections: running.isEmpty ? 0 : 3,
                window: running.isEmpty ? 4 : 6,
                maxConnections: settings.maxConnections,
                holdSeconds: 0,
                speedLimit: settings.speedLimit,
                budgets: [
                    Budget(name: "api", quota: 500, remaining: 462, resetsIn: 212),
                    Budget(name: "resolvers", quota: 3000, remaining: 2871, resetsIn: 212),
                ]
            ),
            speed: running.map(\.speed).reduce(0, +),
            jobs: jobs
        )
    }

    /// Tell everyone who follows that the queue or the library changed.
    func changed() {
        seq += 1
        let signal = DaemonSignal.event(event())
        for listener in listeners.values {
            listener.yield(signal)
        }
    }

    // MARK: - Status and library

    func status(at address: String) async throws -> DaemonStatus {
        try await answering(address)
        return FixtureData.status(jobs: jobs, library: models, cleaned: cleaned, offloaded: offloaded)
    }

    func scan(at address: String) async throws -> ScanResult {
        try await answering(address)
        return ScanResult(
            forgotten: 0, adopted: 2, adoptedBytes: 4_920_000_000, recognised: 0, reclaimed: 2,
            reclaimedBytes: 4_920_000_000, archived: 2, errors: []
        )
    }

    func apps(at address: String) async throws -> [AppInfo] {
        try await answering(address)
        return RecordedProfiles.apps
    }

    func models(at address: String) async throws -> [LibraryModel] {
        try await answering(address)
        return models
    }

    /// The model the daemon holds under this name, or its refusal.
    func held(_ repo: String) throws -> LibraryModel {
        guard let model = models.first(where: { $0.repo == repo }) else {
            throw ClientError.Failed(message: "\(repo) is not in the library")
        }
        return model
    }

    /// Change the model the daemon holds under this name and say so.
    func change(_ repo: String, _ change: (inout LibraryModel) -> Void) {
        guard let index = models.firstIndex(where: { $0.repo == repo }) else {
            return
        }
        change(&models[index])
        library += 1
        changed()
    }

    func model(_ repo: String, at address: String) async throws -> ModelDetail {
        try await answering(address)
        return FixtureData.detail(of: try held(repo), offloaded: offloaded)
    }

    func locations(at address: String) async throws -> Locations {
        try await answering(address)
        return FixtureData.locations(of: models, cleaned: cleaned, offloaded: offloaded)
    }

    // MARK: - Hugging Face

    func search(_ query: SearchQuery, at address: String) async throws -> SearchPage {
        try await answering(address)
        if query.cursor != nil {
            return SearchPage(models: FixtureData.moreHits, next: nil)
        }
        let words = query.text.lowercased()
        let hits = FixtureData.hits.filter { hit in
            (words.isEmpty || hit.id.lowercased().contains(words))
                && (query.format.isEmpty || hit.meta.formats.contains(query.format))
        }
        return SearchPage(models: hits, next: words.isEmpty && query.format.isEmpty ? "page-2" : nil)
    }

    func hubModel(_ repo: String, at address: String) async throws -> HubModel {
        try await answering(address)
        // The provider has no such repository: the real daemon says so.
        guard FixtureData.isKnown(repo) else {
            throw ClientError.Failed(message: "\(repo) was not found on Hugging Face")
        }
        return FixtureData.hubModel(
            repo, downloading: jobs.contains { $0.repo == repo && $0.state.isActive }, library: models,
            offloaded: offloaded
        )
    }

    // MARK: - Storage

    func gcPlan(offload: Bool, at address: String) async throws -> GcPlan {
        try await answering(address)
        return FixtureData.gcPlan(offload: offload, library: models, cleaned: cleaned, offloaded: offloaded)
    }

    /// Removes what the owner agreed to; the next plan no longer lists it.
    func gcApply(orphans: [String], offload: [String], at address: String) async throws -> GcPlan {
        try await answering(address)
        let plan = FixtureData.gcPlan(offload: true, library: models, cleaned: cleaned, offloaded: offloaded)
        let gone = plan.orphans.filter { orphans.contains($0.sha256) }
        let away = plan.offload.filter { offload.contains($0.sha256) }
        cleaned = cleaned || !gone.isEmpty
        offloaded = offloaded || !away.isEmpty
        library += 1
        changed()
        return GcPlan(
            orphans: [], offload: [], orphanBytes: 0, offloadBytes: 0,
            freed: (gone + away).map(\.size).reduce(0, +)
        )
    }

    func startStoreCheck(at address: String) async throws {
        try await answering(address)
    }

    func storeCheck(at address: String) async throws -> StoreCheck {
        try await answering(address)
        return FixtureData.storeCheck(of: models, cleaned: cleaned, offloaded: offloaded)
    }

    // MARK: - The daemon itself

    func settings(at address: String) async throws -> DaemonSettings {
        try await answering(address)
        return settings
    }

    func updateSettings(_ changes: [SettingChange], at address: String) async throws -> DaemonSettings {
        try await answering(address)
        for change in changes {
            switch (change.key, change.value) {
            case let (.maxConnections, .number(value)): settings.maxConnections = value
            case let (.speedLimit, .number(value)): settings.speedLimit = value
            case let (.verifyDays, .number(value)): settings.verifyDays = value
            case let (.scanInterval, .number(value)): settings.scanInterval = value
            case let (.reclaim, .flag(value)): settings.reclaim = value
            case let (.setEnv, .flag(value)): settings.setEnv = value
            case let (.archive, .text(value)): settings.archive = value
            case (.archive, .unset): settings.archive = nil
            case let (.listen, .text(value)):
                settings.listen = value
                settings.restartNeeded = true
            default: settings.restartNeeded = true
            }
        }
        changed()
        return settings
    }

    func logTail(lines: UInt32, at address: String) async throws -> LogTail {
        try await answering(address)
        return FixtureData.log
    }

    func restart(at address: String) async throws {
        try await answering(address)
        settings.restartNeeded = false
    }
}
