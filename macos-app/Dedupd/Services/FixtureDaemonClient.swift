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

    private func answering(_ address: String) throws {
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
        let running = jobs.filter { $0.phase == .downloading }
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
        try answering(address)
        return FixtureData.status(jobs: jobs)
    }

    func scan(at address: String) async throws -> ScanResult {
        try answering(address)
        return ScanResult(
            forgotten: 0, adopted: 2, adoptedBytes: 4_920_000_000, reclaimed: 2,
            reclaimedBytes: 4_920_000_000, archived: 2, errors: []
        )
    }

    func models(at address: String) async throws -> [LibraryModel] {
        try answering(address)
        return FixtureData.models.map(asHeld)
    }

    /// The model as the canned daemon holds it now: an archived one is in
    /// no app.
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
        try answering(address)
        guard var detail = FixtureData.detail(repo) else {
            throw ClientError.Failed(message: "\(repo) is not in the library")
        }
        if archived.contains(repo) {
            detail.model = asHeld(detail.model)
            detail.placements = []
            detail.variants = detail.variants.map { variant in
                var variant = variant
                variant.placedIn = []
                variant.storedBytes = 0
                return variant
            }
        }
        return detail
    }

    func locations(at address: String) async throws -> Locations {
        try answering(address)
        return FixtureData.locations
    }

    // MARK: - Hugging Face

    func search(_ query: SearchQuery, at address: String) async throws -> SearchPage {
        try answering(address)
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
        try answering(address)
        return FixtureData.hubModel(repo, downloading: jobs.contains { $0.repo == repo && $0.phase.isActive })
    }

    // MARK: - Downloads

    func download(_ id: Int64, at address: String) async throws -> JobDetail {
        try answering(address)
        guard let job = jobs.first(where: { $0.id == id }) else {
            throw ClientError.Failed(message: "no download \(id)")
        }
        return FixtureData.jobDetail(job)
    }

    func enqueue(_ request: PullRequest, at address: String) async throws -> Enqueued {
        try answering(address)
        guard ModelSpec.isValid(request.model) else {
            throw ClientError.Failed(
                message: "\(request.model) is not a model name; use org/repo, org/repo:Q4_K_M or org/repo@revision"
            )
        }
        if let existing = jobs.first(where: { $0.model == request.model && !$0.phase.isFinished }) {
            return Enqueued(id: existing.id, existing: true)
        }
        let id = nextID
        nextID += 1
        // What is already stored is placed at once; anything else waits its
        // turn, as it would behind a running download.
        let stored = FixtureData.models.contains { $0.repo == request.model.split(separator: ":").first.map(String.init) }
        jobs.insert(FixtureData.job(id: id, request: request, state: stored ? .done : .queued), at: 0)
        changed()
        return Enqueued(id: id, existing: false)
    }

    func downloadAction(_ action: JobAction, id: Int64, at address: String) async throws {
        try answering(address)
        guard let index = jobs.firstIndex(where: { $0.id == id }) else {
            throw ClientError.Failed(message: "no download \(id)")
        }
        switch action {
        case .pause:
            jobs[index].state = JobState.paused.rawValue
            jobs[index].speed = 0
        case .resume, .retry, .start:
            jobs[index].state = JobState.downloading.rawValue
            jobs[index].detail = nil
            jobs[index].speed = 11_800_000
        case .cancel:
            jobs[index].state = JobState.cancelled.rawValue
            jobs[index].speed = 0
        case .remove:
            jobs.remove(at: index)
        }
        changed()
    }

    func clearDownloads(at address: String) async throws -> UInt64 {
        try answering(address)
        let before = jobs.count
        jobs.removeAll { $0.phase.isFinished }
        changed()
        return UInt64(before - jobs.count)
    }

    // MARK: - Models

    func checkUpstream(_ repo: String, at address: String) async throws -> UpstreamCheck {
        try answering(address)
        let model = FixtureData.models.first { $0.repo == repo }
        return UpstreamCheck(
            upstream: model?.upstream ?? "ok", commit: model?.commit, latest: model?.commit, changed: false
        )
    }

    func forget(_ repo: String, at address: String) async throws {
        try answering(address)
        if let model = FixtureData.models.first(where: { $0.repo == repo }), !model.apps.isEmpty {
            throw ClientError.Failed(message: "\(repo) still has files in \(model.appNames.joined(separator: ", "))")
        }
    }

    func unplace(_ repo: String, app: String, apply: Bool, at address: String) async throws -> UnplacePlan {
        try answering(address)
        let files = FixtureData.detail(repo)?.placements.filter { $0.app == app } ?? []
        return UnplacePlan(
            repo: repo,
            app: app,
            remove: files.map { PlannedFile(path: $0.path, size: $0.size) },
            links: app == "ollama" ? ["\(FixtureData.folder(of: app))/manifests/hf.co/\(repo)/Q4_K_M"] : [],
            keep: [],
            bytes: files.map(\.size).reduce(0, +),
            removed: apply ? UInt64(files.count) : nil
        )
    }

    func archivePlan(_ repo: String, at address: String) async throws -> ArchivePlan {
        try answering(address)
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
        try answering(address)
        let plan = try await archivePlan(repo, at: address)
        archived.insert(repo)
        moved(kind: "archive", repo: repo, files: plan.files, bytes: plan.bytes)
    }

    func restore(_ repo: String, at address: String) async throws {
        try answering(address)
        guard archived.remove(repo) != nil, let model = FixtureData.models.first(where: { $0.repo == repo }) else {
            throw ClientError.Failed(message: "\(repo) is not archived")
        }
        moved(kind: "restore", repo: repo, files: model.storedFiles, bytes: model.storedBytes)
    }

    /// The canned daemon moves a model at once and says so.
    private func moved(kind: String, repo: String, files: UInt64, bytes: UInt64) {
        moving = ModelMove(
            kind: kind, repo: repo, running: false, startedAt: 1_794_000_000, finishedAt: 1_794_000_000,
            bytesTotal: bytes, bytesDone: bytes, files: files, kept: 0, error: nil
        )
        library += 1
        changed()
    }

    // MARK: - Storage

    func gcPlan(offload: Bool, at address: String) async throws -> GcPlan {
        try answering(address)
        return FixtureData.gcPlan(offload: offload)
    }

    func gcApply(orphans: [String], offload: [String], at address: String) async throws -> GcPlan {
        try answering(address)
        let plan = FixtureData.gcPlan(offload: !offload.isEmpty)
        return GcPlan(
            orphans: [], offload: [], orphanBytes: 0, offloadBytes: 0, freed: plan.orphanBytes + plan.offloadBytes
        )
    }

    func startStoreCheck(at address: String) async throws {
        try answering(address)
    }

    func storeCheck(at address: String) async throws -> StoreCheck {
        try answering(address)
        return FixtureData.storeCheck
    }

    // MARK: - The daemon itself

    func settings(at address: String) async throws -> DaemonSettings {
        try answering(address)
        return settings
    }

    func updateSettings(_ changes: [SettingChange], at address: String) async throws -> DaemonSettings {
        try answering(address)
        for change in changes {
            switch (change.key, change.value) {
            case let ("max_connections", .number(value)): settings.maxConnections = value
            case let ("speed_limit", .number(value)): settings.speedLimit = value
            case let ("verify_days", .number(value)): settings.verifyDays = value
            case let ("scan_interval", .number(value)): settings.scanInterval = value
            case let ("reclaim", .flag(value)): settings.reclaim = value
            case let ("set_env", .flag(value)): settings.setEnv = value
            case let ("archive", .text(value)): settings.archive = value
            case ("archive", .unset): settings.archive = nil
            case let ("listen", .text(value)):
                settings.listen = value
                settings.restartNeeded = true
            default: settings.restartNeeded = true
            }
        }
        changed()
        return settings
    }

    func logTail(lines: UInt32, at address: String) async throws -> LogTail {
        try answering(address)
        return FixtureData.log
    }

    func restart(at address: String) async throws {
        try answering(address)
        settings.restartNeeded = false
    }
}
