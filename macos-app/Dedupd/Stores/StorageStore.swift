import DedupdClient
import Foundation
import Observation

/// Where the files are, what can be cleaned up, and the check of the store.
@MainActor
@Observable
final class StorageStore {
    private(set) var locations: Loaded<Locations> = .idle
    /// What a clean-up would remove. Nothing is removed before the owner
    /// has seen this list and confirmed.
    private(set) var plan: Loaded<GcPlan> = .idle
    /// Also list files that could be moved off this Mac because the
    /// archive disk has them and no app uses them.
    /// A plan made for the other setting is taken off the screen at once,
    /// so Clean Up can never remove what a stale list shows.
    var includeOffload = false {
        didSet {
            guard includeOffload != oldValue else {
                return
            }
            planGeneration += 1
            plan = .loading
        }
    }
    /// The bytes the last clean-up freed.
    private(set) var freed: UInt64?
    private(set) var check: StoreCheck?
    private(set) var isCleaning = false
    /// Shown while the owner is asked to confirm a clean-up.
    var confirming = false

    @ObservationIgnored private let daemon: DaemonStore
    /// Counts the reloads and the plans asked for; an answer to an older
    /// one is dropped, so it cannot replace a newer one.
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var planGeneration = 0

    init(daemon: DaemonStore) {
        self.daemon = daemon
    }

    func reload() async {
        generation += 1
        let mine = generation
        let client = daemon.client
        let address = daemon.address
        let found: Loaded<Locations>
        do {
            found = .ready(try await client.locations(at: address))
        } catch {
            found = .failed(describe(error))
        }
        guard mine == generation else {
            return
        }
        locations = found
        await loadPlan()
        let checked = try? await client.storeCheck(at: address)
        guard mine == generation else {
            return
        }
        check = checked
    }

    func loadPlan() async {
        planGeneration += 1
        let mine = planGeneration
        let found: Loaded<GcPlan>
        do {
            found = .ready(try await daemon.client.gcPlan(offload: includeOffload, at: daemon.address))
        } catch {
            found = .failed(describe(error))
        }
        guard mine == planGeneration else {
            return
        }
        plan = found
    }

    /// Whether the plan lists anything.
    var hasSomethingToClean: Bool {
        guard let plan = plan.value else {
            return false
        }
        return !plan.orphans.isEmpty || !plan.offload.isEmpty
    }

    /// Remove exactly what the plan on screen lists.
    func clean() async {
        guard let plan = plan.value, !isCleaning else {
            return
        }
        isCleaning = true
        let answer = await daemon.attempt { client, address in
            try await client.gcApply(
                orphans: plan.orphans.map(\.sha256), offload: plan.offload.map(\.sha256), at: address
            )
        }
        isCleaning = false
        if let answer {
            freed = answer.freed
        }
        await reload()
    }

    /// Have the daemon hash every stored file now.
    func startCheck() async {
        await daemon.attempt { client, address in try await client.startStoreCheck(at: address) }
        await refreshCheck()
    }

    func refreshCheck() async {
        check = try? await daemon.client.storeCheck(at: daemon.address)
    }
}

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
    private(set) var startsAtLogin = false

    @ObservationIgnored private let daemon: DaemonStore
    @ObservationIgnored private let control: any AgentControl
    @ObservationIgnored private var generation = 0

    init(daemon: DaemonStore, control: any AgentControl) {
        self.daemon = daemon
        self.control = control
    }

    func reload() async {
        generation += 1
        let mine = generation
        startsAtLogin = LoginItem.isEnabled
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
        startsAtLogin = LoginItem.isEnabled
        do {
            agent = .ready(try await control.state())
        } catch {
            agent = .failed(describe(error))
        }
    }

    func loadLog() async {
        log = try? await daemon.client.logTail(lines: 200, at: daemon.address)
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
        let answer = await daemon.attempt { client, address in
            try await client.updateSettings(changes, at: address)
        }
        if let answer {
            settings = .ready(answer)
        }
    }

    func restart() async {
        await daemon.restart()
    }

    func setStartsAtLogin(_ enabled: Bool) {
        do {
            try LoginItem.set(enabled)
        } catch {
            daemon.problem = "macOS did not change the login item: \(error.localizedDescription)"
        }
        startsAtLogin = LoginItem.isEnabled
    }
}
