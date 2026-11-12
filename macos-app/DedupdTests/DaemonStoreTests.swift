import DedupdClient
import Foundation
import Testing

@testable import Dedupd

@Suite("Following the daemon")
@MainActor
struct DaemonStoreTests {
    @Test("Before anything is heard the app is looking for the daemon")
    func initial() {
        let daemon = makeDaemon()
        #expect(daemon.connection == .connecting)
        #expect(daemon.menuBarSymbol == "square.stack.3d.up")
        #expect(daemon.summary == "Looking for the daemon…")
        #expect(daemon.jobs.isEmpty)
        #expect(daemon.downloadFraction == nil)
    }

    @Test("A daemon that answers is running, and its status is read once")
    func connected() async {
        let daemon = makeDaemon()
        await daemon.receive(.connected)
        #expect(daemon.connection == .running)
        #expect(daemon.status?.version == "0.1.0")
        #expect(daemon.summary == "Running, version 0.1.0")
        #expect(daemon.revision == 1)
        #expect(daemon.menuBarSymbol == "square.stack.3d.up.fill")
    }

    @Test("A daemon that is away is reported, and the last status is kept for when it returns")
    func disconnected() async {
        let daemon = makeDaemon()
        await daemon.receive(.connected)
        await daemon.receive(.event(event([job(1, .downloading, done: 40_000_000)])))
        await daemon.receive(.disconnected)
        #expect(daemon.connection == .notRunning)
        #expect(daemon.jobs.isEmpty)
        #expect(daemon.status != nil)
        #expect(daemon.menuBarSymbol == "square.stack.3d.up.slash")
        #expect(daemon.summary == "The daemon is not running")
    }

    @Test("An event carries the downloads, and implies the daemon answers")
    func eventCarriesDownloads() async {
        let daemon = makeDaemon()
        await daemon.receive(.event(event([job(1, .downloading, done: 25_000_000), job(2, .queued)])))
        #expect(daemon.connection == .running)
        #expect(daemon.jobs.count == 2)
        #expect(daemon.activeJobs.count == 2)
        #expect(daemon.downloadFraction == 0.125)
        #expect(daemon.menuBarSymbol == "arrow.down.circle")
        #expect(daemon.downloadSummary == "2 downloads · 10.00 MB/s")
    }

    @Test("A download that needs the owner shows in the menu bar")
    func attention() async {
        let daemon = makeDaemon()
        await daemon.receive(.event(event([job(1, .needsAttention)])))
        #expect(daemon.menuBarSymbol == "exclamationmark.triangle")
        #expect(daemon.downloadSummary == "1 download needs attention")
        #expect(daemon.downloadFraction == nil)
    }

    @Test("The screens load again when the daemon says the library changed, and only then")
    func libraryChange() async {
        let daemon = makeDaemon()
        await daemon.receive(.connected)
        await daemon.receive(.event(event([], library: 4)))
        #expect(daemon.revision == 1, "the first event only says where the library stands")
        await daemon.receive(.event(event([job(1, .downloading)], library: 4, seq: 2)))
        #expect(daemon.revision == 1, "a download's progress is not a change to the library")
        await daemon.receive(.event(event([], library: 5, seq: 3)))
        #expect(daemon.revision == 2)
    }

    @Test("A daemon that comes back makes the screens load again")
    func reconnect() async {
        let daemon = makeDaemon()
        await daemon.receive(.connected)
        await daemon.receive(.event(event([], library: 4)))
        await daemon.receive(.disconnected)
        await daemon.receive(.connected)
        await daemon.receive(.event(event([], library: 0)))
        #expect(daemon.revision == 2)
    }

    @Test("The queue before and after each change is handed on, for notifications")
    func jobsChanged() async {
        let daemon = makeDaemon()
        var seen: [([Job], [Job])] = []
        daemon.onJobsChanged = { before, after in seen.append((before, after)) }
        await daemon.receive(.event(event([job(1, .downloading)])))
        await daemon.receive(.event(event([job(1, .downloading)], seq: 2)))
        await daemon.receive(.event(event([job(1, .done)], seq: 3)))
        #expect(seen.count == 2)
        #expect(seen.last?.0.first?.phase == .downloading)
        #expect(seen.last?.1.first?.phase == .done)
    }

    @Test("Following a daemon delivers its queue without asking for it")
    func follows() async {
        let daemon = await startedDaemon(.downloading)
        #expect(await eventually { await daemon.jobs.count == 4 })
        #expect(daemon.connection == .running)
        #expect(daemon.status?.downloadingTotal ?? 0 > 0)
        daemon.stop()
    }

    @Test("With no daemon the app says so instead of waiting")
    func nothingAnswers() async {
        let daemon = await startedDaemon(.notRunning)
        #expect(daemon.connection == .notRunning)
        daemon.stop()
    }

    @Test("A download the app asks for shows up through the event stream")
    func enqueue() async {
        let daemon = await startedDaemon()
        let taken = await daemon.enqueue(PullRequest(model: "unsloth/New-GGUF:Q4_K_M", apps: ["ollama"]))
        #expect(taken)
        #expect(await eventually { await daemon.jobs.first?.model == "unsloth/New-GGUF:Q4_K_M" })
        #expect(daemon.jobs.first?.phase == .queued)
        #expect(daemon.jobs.first?.apps == ["ollama"])
        daemon.stop()
    }

    @Test("Pause, resume, cancel and remove reach the daemon")
    func actions() async throws {
        let daemon = await startedDaemon(.downloading)
        _ = await eventually { await !daemon.jobs.isEmpty }
        let running = try #require(daemon.jobs.first { $0.phase == .downloading })

        await daemon.act(.pause, on: running)
        #expect(await eventually { await daemon.jobs.first { $0.id == running.id }?.phase == .paused })
        await daemon.act(.resume, on: running)
        #expect(await eventually { await daemon.jobs.first { $0.id == running.id }?.phase == .downloading })
        await daemon.act(.cancel, on: running)
        #expect(await eventually { await daemon.jobs.first { $0.id == running.id }?.phase == .cancelled })
        await daemon.clearFinished()
        #expect(await eventually { await !daemon.jobs.contains { $0.phase.isFinished } })
        #expect(daemon.problem == nil)
        daemon.stop()
    }

    @Test("What the daemon refuses is shown to the owner in the daemon's words")
    func refusal() async {
        let daemon = await startedDaemon()
        let taken = await daemon.enqueue(PullRequest(model: "not a model", apps: []))
        #expect(!taken)
        #expect(daemon.problem == "not a model is not a model name; use org/repo, org/repo:Q4_K_M or org/repo@revision")
        daemon.stop()
    }

    @Test("A scan reports what it changed")
    func scan() async {
        let daemon = await startedDaemon()
        let before = daemon.revision
        await daemon.scan()
        guard case let .finished(result) = daemon.scanActivity else {
            Issue.record("the scan did not finish: \(daemon.scanActivity)")
            return
        }
        #expect(result.summary == "Adopted 2 files (4.92 GB), freed 4.92 GB by sharing 2 files, backed up 2 files to the archive disk.")
        #expect(daemon.revision == before + 1)
        daemon.dismissScan()
        #expect(daemon.scanActivity == .idle)
        daemon.stop()
    }

    @Test("A scan with no daemon fails with the reason")
    func scanWithoutDaemon() async {
        let daemon = makeDaemon(.notRunning)
        await daemon.scan()
        #expect(daemon.scanActivity == .failed(message: "The dedupd daemon is not running at 127.0.0.1:8642."))
    }

    @Test("Another address in the settings is followed instead")
    func addressChange() async {
        let daemon = await startedDaemon()
        #expect(daemon.connection == .running)
        daemon.settings.customAddress = "127.0.0.1:9000"
        daemon.start()
        #expect(daemon.connection == .connecting)
        #expect(daemon.status == nil)
        #expect(await eventually { await daemon.connection == .running })
        daemon.stop()
    }
}
