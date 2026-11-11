import DedupdClient
import Foundation
import Observation

/// What the app knows about the daemon, and the two things it can ask of it.
///
/// The store asks the daemon for its status on a timer, because the menu bar
/// icon shows the daemon's state also while the panel is closed.
@MainActor
@Observable
final class DaemonStore {
    /// Whether the daemon answers, and what it said last.
    enum Connection: Equatable {
        /// Nothing has been asked yet.
        case connecting
        /// Nothing answered at the address.
        case notRunning(address: String)
        /// Something answered, but not with a status.
        case failed(message: String)
        case running(DaemonStatus)
    }

    enum PullActivity: Equatable {
        case idle
        case running(model: String)
        case finished(PullResult)
        case failed(model: String, message: String)
    }

    enum ScanActivity: Equatable {
        case idle
        case running
        case finished(ScanResult)
        case failed(message: String)
    }

    private(set) var connection: Connection = .connecting
    private(set) var pullActivity: PullActivity = .idle
    private(set) var scanActivity: ScanActivity = .idle
    /// When the daemon was last asked, whatever it answered.
    private(set) var lastChecked: Date?

    /// Set by the panel while it is on screen; the store asks more often then.
    var panelVisible = false

    @ObservationIgnored private let client: any DaemonClient
    @ObservationIgnored private let settings: AppSettings
    @ObservationIgnored private var pollTask: Task<Void, Never>?

    init(client: any DaemonClient, settings: AppSettings) {
        self.client = client
        self.settings = settings
    }

    // MARK: - Derived state

    /// The daemon's last status, when it is running.
    var status: DaemonStatus? {
        if case let .running(status) = connection {
            return status
        }
        return nil
    }

    var isRunning: Bool { status != nil }

    var isPulling: Bool {
        if case .running = pullActivity {
            return true
        }
        return false
    }

    var isScanning: Bool { scanActivity == .running }

    /// How far the downloads in flight have come, from 0 to 1; nil when
    /// nothing is being downloaded.
    var downloadFraction: Double? {
        guard let status, status.downloadingTotal > 0 else {
            return nil
        }
        return min(1, Double(status.downloadingDone) / Double(status.downloadingTotal))
    }

    /// The SF Symbol for the menu bar.
    var menuBarSymbol: String {
        switch connection {
        case .connecting:
            "square.stack.3d.up"
        case .notRunning, .failed:
            "square.stack.3d.up.slash"
        case .running:
            downloadFraction != nil || isPulling ? "arrow.down.circle" : "square.stack.3d.up.fill"
        }
    }

    /// One line on the daemon's state, for the panel header and for VoiceOver.
    var summary: String {
        switch connection {
        case .connecting:
            "Looking for the daemon…"
        case .notRunning:
            "The daemon is not running"
        case .failed:
            "The daemon did not answer as expected"
        case let .running(status):
            "Running, version \(status.version)"
        }
    }

    /// How long to wait before asking again: often while something is being
    /// downloaded, seldom while nobody is looking.
    var pollInterval: Duration {
        if downloadFraction != nil || isPulling || isScanning {
            return .seconds(1)
        }
        return panelVisible ? .seconds(5) : .seconds(15)
    }

    // MARK: - Asking the daemon

    /// Ask the daemon for its status now.
    func refresh() async {
        let address = settings.address
        let answer: Connection
        do {
            answer = .running(try await client.status(at: address))
        } catch let ClientError.NotRunning(address) {
            answer = .notRunning(address: address)
        } catch {
            answer = .failed(message: describe(error))
        }
        // The address may have been changed in Settings while this was on its
        // way; an answer from the old address says nothing about the new one.
        guard address == settings.address else {
            return
        }
        connection = answer
        lastChecked = Date()
    }

    /// Fetch a model and place it into the chosen apps. Returns when the
    /// daemon has finished; the timer shows the download's progress meanwhile.
    func pull(_ request: PullRequest) async {
        guard !isPulling else {
            return
        }
        pullActivity = .running(model: request.model)
        do {
            pullActivity = .finished(try await client.pull(request, at: settings.address))
        } catch {
            pullActivity = .failed(model: request.model, message: describe(error))
        }
        await refresh()
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
        await refresh()
    }

    /// Clear the outcome of the last pull from the panel.
    func dismissPull() {
        if !isPulling {
            pullActivity = .idle
        }
    }

    /// Clear the outcome of the last scan from the panel.
    func dismissScan() {
        if !isScanning {
            scanActivity = .idle
        }
    }

    // MARK: - Timer

    /// Start asking the daemon for its status on a timer.
    func startPolling() {
        guard pollTask == nil else {
            return
        }
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else {
                    return
                }
                await self.refresh()
                try? await Task.sleep(for: self.pollInterval)
            }
        }
    }

    func stopPolling() {
        pollTask?.cancel()
        pollTask = nil
    }
}
