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
    /// What the app did by itself to the installed daemon after it was
    /// updated, in a sentence.
    private(set) var followed: String?

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

    /// After the app was updated, put the daemon it carries in place of the
    /// older one installed, so the two are one version again. Only an older
    /// daemon is replaced; another build of the same version is offered with
    /// Update instead. An archive or restore that runs is waited for, because
    /// the daemon starts again. Downloads continue after the restart.
    func followAppVersion() async {
        await loadAgent()
        guard case let .ready(agent) = agent, agent.installed, let installed = agent.installedVersion,
              AppVersion.isOlder(installed, than: agent.version) else {
            return
        }
        while daemon.moving?.running == true {
            try? await Task.sleep(for: .seconds(10))
        }
        await run(.install(setEnvironment: false))
        if case let .ready(now) = self.agent, now.installedVersion == agent.version {
            followed = "The daemon was updated from \(installed) to \(agent.version) together with this app."
        }
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
