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

    /// What the archive extension needs of the daemon.
    var daemonStatus: DaemonStatus? { daemon.status }
    var daemonClient: any DaemonClient { daemon.client }
    var daemonAddress: String { daemon.address }

    /// After the archive list changed: the status carries the list, and
    /// the places count what each archive holds.
    func refreshAfterArchives() async {
        await daemon.refreshStatus()
        await reload()
    }

    init(daemon: DaemonStore) {
        self.daemon = daemon
        daemon.whenAddressChanges { [weak self] in self?.forgetDaemon() }
    }

    /// The app turned to another daemon: the places and the clean-up list
    /// were the old one's, and nothing of them may be acted on.
    private func forgetDaemon() {
        generation += 1
        planGeneration += 1
        locations = .idle
        plan = .idle
        freed = nil
        check = nil
        confirming = false
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
        // A read that failed says nothing about the store: what was known
        // stays, and is not replaced by "never checked".
        guard mine == generation, let checked else {
            return
        }
        check = checked
    }

    func loadPlan() async {
        planGeneration += 1
        let mine = planGeneration
        let found: Loaded<GcPlan>
        let all = generation
        do {
            found = .ready(try await daemon.client.gcPlan(offload: includeOffload, at: daemon.address))
        } catch {
            found = .failed(describe(error))
        }
        guard mine == planGeneration, all == generation else {
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
        let mine = generation
        let answer = await daemon.attempt { client, address in
            try await client.gcApply(
                orphans: plan.orphans.map(\.sha256), offload: plan.offload.map(\.sha256), at: address
            )
        }
        isCleaning = false
        guard mine == generation else {
            return
        }
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
        let mine = generation
        // A read that failed leaves what was known.
        if let answer = try? await daemon.client.storeCheck(at: daemon.address), mine == generation {
            check = answer
        }
    }
}
