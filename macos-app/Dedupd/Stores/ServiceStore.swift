import DedupdClient
import Foundation
import Observation

/// The daemon as a service: its launchd agent, its settings and its log.
@MainActor
@Observable
final class ServiceStore {
    private(set) var agent: Loaded<AgentState> = .idle
    /// An install, uninstall, start or stop is running.
    private(set) var isWorking = false
    /// What the last such command said.
    private(set) var lastOutput: String?
    private(set) var settings: Loaded<DaemonSettings> = .idle
    private(set) var log: LogTail?

    @ObservationIgnored private let daemon: DaemonStore
    @ObservationIgnored private let control: any AgentControl
    @ObservationIgnored private var generation = 0

    init(daemon: DaemonStore, control: any AgentControl) {
        self.daemon = daemon
        self.control = control
        daemon.whenAddressChanges { [weak self] in
            // The settings and the log were the other daemon's.
            self?.generation += 1
            self?.settings = .idle
            self?.log = nil
        }
    }

    func reload() async {
        generation += 1
        let mine = generation
        await loadAgent()
        guard mine == generation else {
            return
        }
        guard daemon.isRunning else {
            settings = .idle
            log = nil
            return
        }
        let found: Loaded<DaemonSettings>
        do {
            found = .ready(try await daemon.client.settings(at: daemon.address))
        } catch {
            found = .failed(describe(error))
        }
        // An answer to an older reload does not replace a newer one.
        guard mine == generation else {
            return
        }
        settings = found
        await loadLog()
    }

    func loadAgent() async {
        do {
            agent = .ready(try await control.state())
        } catch {
            agent = .failed(describe(error))
        }
    }

    func loadLog() async {
        let mine = generation
        // A read that failed leaves the lines already shown.
        if let answer = try? await daemon.client.logTail(lines: 200, at: daemon.address), mine == generation {
            log = answer
        }
    }

    enum Command {
        case install(setEnvironment: Bool)
        case uninstall
        case start
        case stop
    }

    /// Run one of the agent commands and show what it said.
    func run(_ command: Command) async {
        guard !isWorking else {
            return
        }
        isWorking = true
        defer { isWorking = false }
        do {
            switch command {
            case let .install(setEnvironment):
                lastOutput = try await control.install(setEnvironment: setEnvironment)
            case .uninstall:
                lastOutput = try await control.uninstall()
            case .start:
                lastOutput = try await control.start()
            case .stop:
                lastOutput = try await control.stop()
            }
        } catch {
            daemon.problem = describe(error)
        }
        await loadAgent()
    }

    /// Change settings in the daemon's config file. The daemon says whether
    /// it must start again for them to take effect.
    func update(_ changes: [SettingChange]) async {
        guard !changes.isEmpty else {
            return
        }
        let mine = generation
        let answer = await daemon.attempt { client, address in
            try await client.updateSettings(changes, at: address)
        }
        if let answer, mine == generation {
            settings = .ready(answer)
        }
    }

    func restart() async {
        await daemon.restart()
    }
}
