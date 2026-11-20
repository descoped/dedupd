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
    private var jobs: [Job]
    private var settings = FixtureData.settings
    private var seq: UInt64 = 0
    private var nextID: Int64 = 20
    private var history: [ActivityEntry] = FixtureData.activity
    private var favoriteList: Favorites = FixtureData.favorites
    /// The models the owner archived, and the last move, as the daemon
    /// would tell it.
    private var archived: Set<String> = []
    private var moving: ModelMove?
    private var library: UInt64 = 1
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

    private func answering(_ address: String, call: String = #function) async throws {
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

    /// Tell everyone who follows that the queue changed.
    private func changed() {
        seq += 1
        let signal = DaemonSignal.event(event())
        for listener in listeners.values {
            listener.yield(signal)
        }
    }

    // MARK: - Status and library

    func status(at address: String) async throws -> DaemonStatus {
        try await answering(address)
        return FixtureData.status(jobs: jobs)
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
        return FixtureData.models.map(asHeld)
    }

    /// As the daemon holds it now: an archived one is in no app or store.
    private func asHeld(_ model: LibraryModel) -> LibraryModel {
        guard archived.contains(model.repo) else {
            return model
        }
        var model = model
        model.apps = []
        model.archivedAt = 1_794_000_000
        return model
    }

    func model(_ repo: String, at address: String) async throws -> ModelDetail {
        try await answering(address)
        guard var detail = FixtureData.detail(repo) else {
            throw ClientError.Failed(message: "\(repo) is not in the library")
        }
        if archived.contains(repo) {
            detail.model = asHeld(detail.model)
            detail.placements = []
            detail.variants = detail.variants.map { variant in
                var variant = variant
                variant.placedIn = []
                (variant.storedBytes, variant.weightsStored) = (0, false)
                return variant
            }
        }
        return detail
    }

    func locations(at address: String) async throws -> Locations {
        try await answering(address)
        return FixtureData.locations
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
        return FixtureData.hubModel(repo, downloading: jobs.contains { $0.repo == repo && $0.state.isActive })
    }

    // MARK: - Models

    func checkUpstream(_ repo: String, at address: String) async throws -> UpstreamCheck {
        try await answering(address)
        let model = FixtureData.models.first { $0.repo == repo }
        return UpstreamCheck(
            upstream: model?.upstream ?? .ok, commit: model?.commit, latest: model?.commit, changed: false
        )
    }

    func forget(_ repo: String, at address: String) async throws {
        try await answering(address)
        if let model = FixtureData.models.first(where: { $0.repo == repo }).map(asHeld), !model.apps.isEmpty {
            throw ClientError.Failed(message: "\(repo) still has files in \(model.appNames.map(\.name).joined(separator: ", "))")
        }
    }

    func unplace(_ repo: String, app: ModelApp, paths: [String]?, apply: Bool, at address: String) async throws
        -> UnplacePlan {
        try await answering(address)
        let files = (FixtureData.detail(repo)?.placements.filter { $0.app == app } ?? [])
            .filter { paths?.contains($0.path) ?? true }
        let plan = UnplacePlan(
            repo: repo,
            app: app,
            remove: files.map { PlannedFile(path: $0.path, size: $0.size) },
            links: app == .ollama ? ["\(FixtureData.folder(of: app))/manifests/hf.co/\(repo)/Q4_K_M"] : [],
            keep: [],
            bytes: files.map(\.size).reduce(0, +),
            removed: apply ? UInt64(files.count) : nil
        )
        if apply {
            note(.init(action: .deactivate, repo: repo, app: app, ok: true, text: "Deactivated in \(app.title)"))
            library += 1
            changed()
        }
        return plan
    }

    func archivePlan(_ repo: String, at address: String) async throws -> ArchivePlan {
        try await answering(address)
        guard let model = FixtureData.models.first(where: { $0.repo == repo }), !archived.contains(repo) else {
            throw ClientError.Failed(message: "\(repo) is not in the library")
        }
        return ArchivePlan(
            repo: repo,
            archive: FixtureData.archiveFolder,
            deactivate: model.apps.map { DeactivateStep(app: $0.app, files: $0.files, bytes: $0.bytes, keep: []) },
            files: model.storedFiles,
            bytes: model.storedBytes,
            copyFiles: 0,
            copyBytes: 0,
            keep: []
        )
    }

    func archive(_ repo: String, at address: String) async throws {
        try await answering(address)
        let plan = try await archivePlan(repo, at: address)
        archived.insert(repo)
        moved(kind: .archive, repo: repo, files: plan.files, bytes: plan.bytes)
    }

    func restore(_ repo: String, at address: String) async throws {
        try await answering(address)
        guard archived.remove(repo) != nil, let model = FixtureData.models.first(where: { $0.repo == repo }) else {
            throw ClientError.Failed(message: "\(repo) is not archived")
        }
        moved(kind: .restore, repo: repo, files: model.storedFiles, bytes: model.storedBytes)
    }

    /// The canned daemon moves a model at once and says so.
    private func moved(kind: MoveKind, repo: String, files: UInt64, bytes: UInt64) {
        moving = ModelMove(
            kind: kind, repo: repo, running: false, startedAt: 1_794_000_000, finishedAt: 1_794_000_000,
            bytesTotal: bytes, bytesDone: bytes, files: files, kept: 0, error: nil
        )
        library += 1
        changed()
    }

    // MARK: - Storage

    func gcPlan(offload: Bool, at address: String) async throws -> GcPlan {
        try await answering(address)
        return FixtureData.gcPlan(offload: offload)
    }

    func gcApply(orphans: [String], offload: [String], at address: String) async throws -> GcPlan {
        try await answering(address)
        let plan = FixtureData.gcPlan(offload: !offload.isEmpty)
        return GcPlan(
            orphans: [], offload: [], orphanBytes: 0, offloadBytes: 0, freed: plan.orphanBytes + plan.offloadBytes
        )
    }

    func startStoreCheck(at address: String) async throws {
        try await answering(address)
    }

    func storeCheck(at address: String) async throws -> StoreCheck {
        try await answering(address)
        return FixtureData.storeCheck
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

// MARK: - Downloads

extension FixtureDaemonClient {

    func download(_ id: Int64, at address: String) async throws -> JobDetail {
        try await answering(address)
        guard let job = jobs.first(where: { $0.id == id }) else {
            throw ClientError.Failed(message: "no download \(id)")
        }
        return FixtureData.jobDetail(job)
    }

    func enqueue(_ request: PullRequest, at address: String) async throws -> Enqueued {
        try await answering(address)
        // Only the name is checked here; the rest of a spec is the real
        // daemon's rule, which is not written a second time.
        guard let repo = ModelSpec.repository(ofSpec: request.model) else {
            throw ClientError.Failed(
                message: "\(request.model) is not a model name; use org/repo, org/repo:Q4_K_M or org/repo@revision"
            )
        }
        if let existing = jobs.first(where: { $0.model == request.model && !$0.state.isFinished }) {
            return Enqueued(id: existing.id, existing: true)
        }
        let id = nextID
        nextID += 1
        // What is already stored is placed at once; anything else waits its
        // turn, as it would behind a running download. An app the recorded
        // profiles say no for does not take the model, with their reason.
        let stored = FixtureData.models.first { $0.repo == repo }
        var notTakenBy: [ModelApp: String] = [:]
        if let stored {
            let decisions = FixtureData.judged(repo, formats: stored.meta.formats, stored: true).decisions
            for decision in decisions where decision.state != .ready && request.apps.contains(decision.app) {
                notTakenBy[decision.app] = decision.reason
            }
        }
        jobs.insert(
            FixtureData.job(id: id, request: request, state: stored == nil ? .queued : .done, notTakenBy: notTakenBy),
            at: 0
        )
        if stored != nil {
            for app in request.apps {
                let reason = notTakenBy[app]
                note(.init(
                    action: .activate, repo: repo, app: app, ok: reason == nil,
                    text: reason.map { "Not activated in \(app.title): \($0)" } ?? "Activated in \(app.title)"
                ))
            }
        }
        changed()
        return Enqueued(id: id, existing: false)
    }

    func downloadAction(_ action: JobAction, id: Int64, at address: String) async throws {
        try await answering(address)
        guard let index = jobs.firstIndex(where: { $0.id == id }) else {
            throw ClientError.Failed(message: "no download \(id)")
        }
        guard jobs[index].actions.contains(action) else {
            throw ClientError.Failed(message: "download \(id) cannot \(action.name) now")
        }
        switch action {
        case .pause:
            jobs[index].state = .paused
            jobs[index].speed = 0
        case .resume, .retry, .start:
            jobs[index].state = .downloading
            jobs[index].detail = nil
            jobs[index].speed = 11_800_000
        case .cancel:
            jobs[index].state = .cancelled
            jobs[index].speed = 0
        case .remove:
            jobs.remove(at: index)
            changed()
            return
        }
        jobs[index].actions = jobActions(state: jobs[index].state)
        changed()
    }

    func activity(at address: String) async throws -> [ActivityEntry] {
        try await answering(address)
        return history
    }

    func favorites(at address: String) async throws -> Favorites {
        try await answering(address)
        return favoriteList
    }

    func setFavorite(_ repo: String, _ favorite: Bool, at address: String) async throws -> Favorites {
        try await answering(address)
        favoriteList.favorites.removeAll { $0.repo == repo }
        if favorite {
            favoriteList.favorites.insert(Favorite(repo: repo, addedAt: FixtureData.now, source: .dedupd), at: 0)
        }
        changed()
        return favoriteList
    }

    func syncFavorites(at address: String) async throws -> Favorites {
        try await answering(address)
        favoriteList.likes.syncedAt = FixtureData.now
        changed()
        return favoriteList
    }

    /// Add to the canned activity list, newest first.
    private func note(_ entry: FixtureData.Happened) {
        let id = (history.first?.id ?? 0) + 1
        history.insert(
            ActivityEntry(
                id: id, at: Int64(Date().timeIntervalSince1970), action: entry.action, repo: entry.repo,
                app: entry.app, ok: entry.ok, text: entry.text
            ),
            at: 0
        )
    }

    func clearDownloads(at address: String) async throws -> UInt64 {
        try await answering(address)
        let before = jobs.count
        jobs.removeAll { $0.state.isFinished }
        changed()
        return UInt64(before - jobs.count)
    }
}
