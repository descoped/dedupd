import DedupdClient
import Foundation
import Observation

/// What the app knows about the daemon as a whole: whether it answers, what
/// it holds, and its download queue.
///
/// Nothing is polled. One connection follows the daemon and delivers its
/// state whenever something changes; the status is read again only when the
/// daemon says the library changed.
@MainActor
@Observable
final class DaemonStore {
    enum Connection: Equatable {
        /// Nothing has been heard yet.
        case connecting
        /// Nothing answers at the address.
        case notRunning
        case running
    }

    enum ScanActivity: Equatable {
        case idle
        case running
        case finished(ScanResult)
        case failed(message: String)
    }

    private(set) var connection: Connection = .connecting
    /// The daemon's last status; kept while it is away, so the window does
    /// not go blank during a restart.
    private(set) var status: DaemonStatus?
    /// The download queue and the rate governor, as of the last event.
    private(set) var downloads: Downloads?
    /// The daemon is hashing its store.
    private(set) var checking = false
    /// The archive or restore of one model that is running, or the last one.
    private(set) var moving: ModelMove?
    /// Changes whenever the library may have changed or the daemon came
    /// back. Screens load their data again when it does.
    private(set) var revision = 0
    private(set) var scanActivity: ScanActivity = .idle
    /// Something the owner asked for failed; shown until dismissed.
    var problem: String?

    /// Called with the queue before and after each event, for notifications.
    @ObservationIgnored var onJobsChanged: (([Job], [Job]) -> Void)?

    @ObservationIgnored let client: any DaemonClient
    @ObservationIgnored let settings: AppSettings
    @ObservationIgnored private var follower: Task<Void, Never>?
    @ObservationIgnored private var following = ""
    @ObservationIgnored private var library: UInt64?

    init(client: any DaemonClient, settings: AppSettings) {
        self.client = client
        self.settings = settings
    }

    var address: String { settings.address }

    // MARK: - Derived state

    var isRunning: Bool { connection == .running }

    var jobs: [Job] { downloads?.jobs ?? [] }

    /// The downloads the daemon is working on or will get to.
    var activeJobs: [Job] { jobs.filter { $0.phase.isActive } }

    /// The downloads that wait for the owner.
    var attentionJobs: [Job] { jobs.filter { $0.phase == .needsAttention } }

    var isScanning: Bool { scanActivity == .running }

    /// How far the downloads in flight have come, from 0 to 1; nil when
    /// nothing is being downloaded or no size is known yet.
    var downloadFraction: Double? {
        let active = activeJobs
        let total = active.map(\.bytesTotal).reduce(0, +)
        guard !active.isEmpty, total > 0 else {
            return nil
        }
        return min(1, Double(active.map(\.bytesDone).reduce(0, +)) / Double(total))
    }

    /// The SF Symbol for the menu bar.
    var menuBarSymbol: String {
        switch connection {
        case .connecting:
            "square.stack.3d.up"
        case .notRunning:
            "square.stack.3d.up.slash"
        case .running:
            if !attentionJobs.isEmpty {
                "exclamationmark.triangle"
            } else if !activeJobs.isEmpty {
                "arrow.down.circle"
            } else {
                "square.stack.3d.up.fill"
            }
        }
    }

    /// One line on the daemon's state, for headers and for VoiceOver.
    var summary: String {
        switch connection {
        case .connecting:
            "Looking for the daemon…"
        case .notRunning:
            "The daemon is not running"
        case .running:
            status.map { "Running, version \($0.version)" } ?? "Running"
        }
    }

    /// One line on the downloads, for the menu bar panel; nil when the
    /// queue is empty.
    var downloadSummary: String? {
        let active = activeJobs
        if active.isEmpty {
            let waiting = attentionJobs.count
            return waiting == 0 ? nil : "\(waiting) download\(waiting == 1 ? "" : "s") need\(waiting == 1 ? "s" : "") attention"
        }
        let speed = downloads?.speed ?? 0
        let count = "\(active.count) download\(active.count == 1 ? "" : "s")"
        return speed > 0 ? "\(count) · \(ByteFormat.speed(speed))" : count
    }

    // MARK: - Following the daemon

    /// Start following the daemon at the address in the settings. Called
    /// again after the address changed, it follows the new one.
    func start() {
        let address = settings.address
        guard follower == nil || following != address else {
            return
        }
        follower?.cancel()
        following = address
        connection = .connecting
        status = nil
        downloads = nil
        library = nil
        follower = Task { [weak self, client] in
            for await signal in client.events(at: address) {
                guard let self, !Task.isCancelled else {
                    return
                }
                await self.receive(signal)
            }
        }
    }

    func stop() {
        follower?.cancel()
        follower = nil
    }

    /// Take in what the connection reports. Internal so tests can drive the
    /// store without a stream.
    func receive(_ signal: DaemonSignal) async {
        switch signal {
        case .connected:
            await connected()
        case .disconnected:
            connection = .notRunning
            downloads = nil
            checking = false
            moving = nil
            library = nil
        case let .event(event):
            if connection != .running {
                await connected()
            }
            let before = jobs
            downloads = event.downloads
            checking = event.checking
            moving = event.moving
            if before != event.downloads.jobs {
                onJobsChanged?(before, event.downloads.jobs)
            }
            // A job that is done put files into apps. The screens look again
            // then, whether or not the daemon also says the library changed.
            let first = library == nil
            let finished = event.downloads.jobs.contains { job in
                job.phase == .done && before.first { $0.id == job.id }?.phase != .done
            }
            let changed = library != event.library
            library = event.library
            if !first, changed || finished {
                await refreshStatus()
                revision += 1
            }
        }
    }

    private func connected() async {
        guard connection != .running else {
            return
        }
        connection = .running
        await refreshStatus()
        revision += 1
    }

    /// Read the daemon's status now.
    func refreshStatus() async {
        let address = settings.address
        guard let answer = try? await client.status(at: address), address == settings.address else {
            return
        }
        status = answer
    }

    // MARK: - Asking the daemon

    /// Run something the owner asked for; a failure is shown to them.
    @discardableResult
    func attempt<T: Sendable>(_ work: (any DaemonClient, String) async throws -> T) async -> T? {
        do {
            return try await work(client, settings.address)
        } catch {
            problem = describe(error)
            return nil
        }
    }

    /// Put a download in the queue. Returns whether the daemon took it.
    @discardableResult
    func enqueue(_ request: PullRequest) async -> Bool {
        await attempt { client, address in try await client.enqueue(request, at: address) } != nil
    }

    func act(_ action: JobAction, on job: Job) async {
        await attempt { client, address in try await client.downloadAction(action, id: job.id, at: address) }
    }

    func clearFinished() async {
        await attempt { client, address in try await client.clearDownloads(at: address) }
    }

    /// Have the daemon look at the app folders now.
    func scan() async {
        guard !isScanning else {
            return
        }
        scanActivity = .running
        do {
            scanActivity = .finished(try await client.scan(at: settings.address))
        } catch {
            scanActivity = .failed(message: describe(error))
        }
        await refreshStatus()
        revision += 1
    }

    /// Clear the outcome of the last scan.
    func dismissScan() {
        if !isScanning {
            scanActivity = .idle
        }
    }

    /// Ask the daemon to start again, for a changed setting to take effect.
    func restart() async {
        await attempt { client, address in try await client.restart(at: address) }
    }
}
