import DedupdClient
import Testing

@testable import Dedupd

@Suite("The daemon store")
@MainActor
struct DaemonStoreTests {
    private let pulled = PullResult(
        repo: "org/repo", commit: "c", fromArchive: false, files: 2, bytes: 10, downloaded: 2, placed: [], notes: []
    )
    private let scanned = ScanResult(
        forgotten: 0, adopted: 1, adoptedBytes: 5, reclaimed: 0, reclaimedBytes: 0, archived: 0, errors: []
    )

    /// A store with the fake daemon behind it and the settings it reads.
    private struct Harness {
        let store: DaemonStore
        let fake: FakeDaemonClient
        let settings: AppSettings
    }

    private func makeStore(_ fake: FakeDaemonClient = FakeDaemonClient()) -> Harness {
        let settings = makeSettings()
        return Harness(store: DaemonStore(client: fake, settings: settings), fake: fake, settings: settings)
    }

    @Test("Before the first answer the store is connecting")
    func initial() {
        let store = makeStore().store
        #expect(store.connection == .connecting)
        #expect(store.status == nil)
        #expect(!store.isRunning)
        #expect(store.menuBarSymbol == "square.stack.3d.up")
        #expect(store.lastChecked == nil)
    }

    @Test("A status answer makes the store running and asks the configured address")
    func running() async {
        let harness = makeStore()
        let (store, fake) = (harness.store, harness.fake)
        await store.refresh()
        #expect(store.isRunning)
        #expect(store.status?.storeFiles == 61)
        #expect(store.summary == "Running, version 0.1.0")
        #expect(store.menuBarSymbol == "square.stack.3d.up.fill")
        #expect(store.downloadFraction == nil)
        #expect(store.lastChecked != nil)
        #expect(await fake.statusAddresses == ["127.0.0.1:8642"])
    }

    @Test("No answer at the address means the daemon is not running")
    func notRunning() async {
        let store = makeStore(FakeDaemonClient(status: .failure(.NotRunning(address: "127.0.0.1:8642")))).store
        await store.refresh()
        #expect(store.connection == .notRunning(address: "127.0.0.1:8642"))
        #expect(!store.isRunning)
        #expect(store.menuBarSymbol == "square.stack.3d.up.slash")
    }

    @Test("Any other failure is shown with its message")
    func failed() async {
        let store = makeStore(FakeDaemonClient(status: .failure(.Protocol(message: "missing field")))).store
        await store.refresh()
        #expect(store.connection == .failed(message: "Unexpected answer from the daemon: missing field"))
    }

    @Test("The daemon coming back is noticed on the next refresh")
    func recovers() async {
        let harness = makeStore(FakeDaemonClient(status: .failure(.NotRunning(address: "a"))))
        let (store, fake) = (harness.store, harness.fake)
        await store.refresh()
        #expect(!store.isRunning)
        await fake.answerStatus(.success(FixtureDaemonClient.status()))
        await store.refresh()
        #expect(store.isRunning)
    }

    @Test("A download in flight shows as a fraction and changes the icon")
    func downloading() async {
        let status = FixtureDaemonClient.status(downloadingTotal: 200, downloadingDone: 50)
        let store = makeStore(FakeDaemonClient(status: .success(status))).store
        await store.refresh()
        #expect(store.downloadFraction == 0.25)
        #expect(store.menuBarSymbol == "arrow.down.circle")
        #expect(store.pollInterval == .seconds(1))
    }

    @Test("More bytes than expected never reads as more than done")
    func overshoot() async {
        let status = FixtureDaemonClient.status(downloadingTotal: 100, downloadingDone: 150)
        let store = makeStore(FakeDaemonClient(status: .success(status))).store
        await store.refresh()
        #expect(store.downloadFraction == 1)
    }

    @Test("The store asks seldom while the panel is closed and often while it is open")
    func pollInterval() async {
        let store = makeStore().store
        await store.refresh()
        #expect(store.pollInterval == .seconds(15))
        store.panelVisible = true
        #expect(store.pollInterval == .seconds(5))
    }

    @Test("A pull is sent once, shows as running, and its outcome is kept")
    func pull() async {
        let harness = makeStore()
        let (store, fake) = (harness.store, harness.fake)
        await fake.answerPull(.success(pulled))
        await fake.hold(.pull)
        let request = PullRequest(model: "org/repo", apps: ["bionic"])

        let task = Task { await store.pull(request) }
        #expect(await eventually { await fake.pulls.count == 1 })
        #expect(store.pullActivity == .running(model: "org/repo"))
        #expect(store.isPulling)
        #expect(store.pollInterval == .seconds(1))

        // A second pull while one runs is ignored.
        await store.pull(PullRequest(model: "other/repo", apps: []))
        #expect(await fake.pulls == [request])

        await fake.release(.pull)
        await task.value
        #expect(store.pullActivity == .finished(pulled))
        #expect(!store.isPulling)
        // The status is asked again so the new model shows at once.
        #expect(await fake.statusAddresses.count == 1)

        store.dismissPull()
        #expect(store.pullActivity == .idle)
    }

    @Test("A failed pull keeps the daemon's message")
    func pullFails() async {
        let harness = makeStore()
        let (store, fake) = (harness.store, harness.fake)
        await fake.answerPull(.failure(.Failed(message: "org/nope was not found on Hugging Face")))
        await store.pull(PullRequest(model: "org/nope", apps: []))
        #expect(store.pullActivity == .failed(model: "org/nope", message: "org/nope was not found on Hugging Face"))
    }

    @Test("The outcome of a pull cannot be dismissed while it runs")
    func dismissWhileRunning() async {
        let harness = makeStore()
        let (store, fake) = (harness.store, harness.fake)
        await fake.answerPull(.success(pulled))
        await fake.hold(.pull)
        let task = Task { await store.pull(PullRequest(model: "org/repo", apps: [])) }
        #expect(await eventually { await fake.pulls.count == 1 })
        store.dismissPull()
        #expect(store.isPulling)
        await fake.release(.pull)
        await task.value
    }

    @Test("A scan runs once at a time and keeps its outcome")
    func scan() async {
        let harness = makeStore()
        let (store, fake) = (harness.store, harness.fake)
        await fake.answerScan(.success(scanned))
        await fake.hold(.scan)
        let task = Task { await store.scan() }
        #expect(await eventually { await fake.scans == 1 })
        #expect(store.isScanning)
        await store.scan()
        #expect(await fake.scans == 1)
        await fake.release(.scan)
        await task.value
        #expect(store.scanActivity == .finished(scanned))
        store.dismissScan()
        #expect(store.scanActivity == .idle)
    }

    @Test("A failed scan keeps its message")
    func scanFails() async {
        let harness = makeStore()
        let (store, fake) = (harness.store, harness.fake)
        await fake.answerScan(.failure(.NotRunning(address: "127.0.0.1:8642")))
        await store.scan()
        #expect(store.scanActivity == .failed(message: "The dedupd daemon is not running at 127.0.0.1:8642."))
    }

    @Test("An answer from an address the user has since changed is dropped")
    func staleAnswer() async {
        let harness = makeStore()
        let (store, fake, settings) = (harness.store, harness.fake, harness.settings)
        await fake.hold(.status)
        let task = Task { await store.refresh() }
        #expect(await eventually { await fake.statusAddresses.count == 1 })
        settings.customAddress = "127.0.0.1:9999"
        await fake.release(.status)
        await task.value
        #expect(store.connection == .connecting)
    }

    @Test("The timer asks without being told to, and stops when told")
    func polling() async {
        let harness = makeStore()
        let (store, fake) = (harness.store, harness.fake)
        store.startPolling()
        store.startPolling()
        #expect(await eventually { await fake.statusAddresses.count == 1 })
        #expect(await eventually { await MainActor.run { store.isRunning } })
        store.stopPolling()
    }
}
