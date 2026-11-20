import DedupdClient
import Foundation
import Testing

@testable import Dedupd

@Suite("The connection's mailbox, the bundled command and the canned daemon")
struct ServicesTests {
    private final class Marker {}

    @Test("A newer event replaces one that waits; the daemon going and coming is never dropped")
    func mailbox() async {
        let mailbox = SignalMailbox()
        mailbox.put(.event(event([], seq: 1)))
        mailbox.put(.disconnected)
        mailbox.put(.disconnected)
        mailbox.put(.connected)
        for seq in 2 ... 9 {
            mailbox.put(.event(event([], seq: UInt64(seq))))
        }
        mailbox.put(.unreadable("another build"))
        #expect(await mailbox.next() == .event(event([], seq: 1)))
        #expect(await mailbox.next() == .disconnected)
        #expect(await mailbox.next() == .connected)
        #expect(await mailbox.next() == .event(event([], seq: 9)), "only the newest of a run of events")
        #expect(await mailbox.next() == .unreadable("another build"))

        // A reader that waits gets the next signal as it comes, and nil
        // once the mailbox is closed.
        let waiting = Task { await mailbox.next() }
        try? await Task.sleep(for: .milliseconds(20))
        mailbox.put(.connected)
        #expect(await waiting.value == .connected)
        mailbox.close()
        mailbox.put(.disconnected)
        #expect(await mailbox.next() == nil)
    }

    @Test("A reader that is cancelled while it waits is let go")
    func mailboxCancelled() async {
        let mailbox = SignalMailbox()
        let waiting = Task { await mailbox.next() }
        try? await Task.sleep(for: .milliseconds(20))
        waiting.cancel()
        #expect(await waiting.value == nil)
    }

    @Test("A command that does not finish is stopped and reported; one that fails says what it wrote")
    func commandBound() throws {
        let shell = URL(fileURLWithPath: "/bin/sh")
        #expect(try BundledAgent.launch(shell, ["-c", "echo done"]) == "done")
        #expect(throws: AgentError.failed("no such home")) {
            try BundledAgent.launch(shell, ["-c", "echo no such home >&2; exit 3"])
        }
        // More than a pipe holds on both streams at once.
        let long = try BundledAgent.launch(shell, ["-c", "head -c 300000 /dev/zero | tr '\\0' x; head -c 300000 /dev/zero >&2"])
        #expect(long.count == 300_000)
        // Something the command started that outlives it and holds its
        // output open is not waited for.
        let before = Date()
        #expect(try BundledAgent.launch(shell, ["-c", "sleep 20 & echo done"]) == "done")
        #expect(Date().timeIntervalSince(before) < 10)
        let started = Date()
        #expect(throws: AgentError.self) {
            try BundledAgent.launch(shell, ["-c", "sleep 30"], patience: 0.3)
        }
        #expect(Date().timeIntervalSince(started) < 10, "it was stopped, not waited for")
    }

    @Test("The app reads what `dedupd agent --json` prints")
    func agentState() throws {
        let url = try #require(
            Bundle(for: Marker.self).url(forResource: "agent", withExtension: "json", subdirectory: "control"),
            "the recorded agent.json is not in the test bundle"
        )
        let state = try JSONDecoder().decode(AgentState.self, from: Data(contentsOf: url))
        #expect(state.label == "io.descoped.dedupd.daemon")
        #expect(state.installed && state.running)
        #expect(state.installedVersion == "0.2.0" && state.version == "0.2.0")
        #expect(state.sameBuild == false)
        #expect(state.needsUpdate, "the same version, another build")
        #expect(state.program == "/Users/example/.dedupd/bin/dedupd")
        #expect(state.log == "/Users/example/.dedupd/daemon.log")
    }

    @Test("The canned daemon's library changes with what is done to it, and every screen's figures agree")
    func cannedLibrary() async throws {
        let client = FixtureDaemonClient(scenario: .running)
        func model(_ repo: String) async throws -> LibraryModel {
            try #require(try await client.models(at: "a").first { $0.repo == repo })
        }
        /// The status, the places and the library say the same.
        func agree() async throws {
            let models = try await client.models(at: "a")
            let status = try await client.status(at: "a")
            let places = try await client.locations(at: "a")
            #expect(places.store.files == status.storeFiles && places.store.bytes == status.storeBytes)
            #expect(places.store.sharedBytes == 0 && places.archive?.bytes == 0, "as the daemon counts them")
            for place in places.apps {
                let held = models.compactMap { model in model.apps.first { $0.app == place.app } }
                #expect(place.files == held.map(\.files).reduce(0, +))
                #expect(place.bytes == held.map(\.bytes).reduce(0, +))
                #expect(place.models == UInt64(held.count))
                let usage = status.apps.first { $0.app == place.app }
                #expect((usage?.files ?? 0, usage?.bytes ?? 0) == (place.files, place.bytes))
            }
            #expect(status.sharedBytes == places.sharedBytes)
        }
        try await agree()
        #expect(try await client.status(at: "a").storeFiles == 61)

        // Deactivated: the app no longer has it, and the history says so
        // in the daemon's sentence.
        _ = try await client.unplace(FixtureData.smolGGUF, app: .ollama, paths: nil, apply: false, at: "a")
        #expect(try await model(FixtureData.smolGGUF).appNames == [.bionic, .ollama], "a plan changes nothing")
        let removed = try await client.unplace(FixtureData.smolGGUF, app: .ollama, paths: nil, apply: true, at: "a")
        #expect(removed.removed == 4)
        #expect(try await model(FixtureData.smolGGUF).appNames == [.bionic])
        #expect(try await model(FixtureData.smolGGUF).ollamaTags.isEmpty)
        #expect(try await client.activity(at: "a").first?.text == "Deactivated in Ollama")
        try await agree()

        // Activated: the apps that take it have it; the job reads as an
        // activation, and the history has the app's own reason for a no.
        let asked = try await client.enqueue(
            PullRequest(model: FixtureData.smolMLX, apps: [.huggingFace, .ollama]), at: "a"
        )
        let job = try await client.download(asked.id, at: "a").job
        #expect(job.kind == .activation && job.state == .done)
        #expect(try await model(FixtureData.smolMLX).appNames == job.activated)
        let lines = try await client.activity(at: "a").prefix(2).map(\.text)
        #expect(lines.contains("Activated in the Hugging Face cache"))
        try await agree()

        // Archived: in no app, off the internal disk. Restored: in the
        // store again, in no app.
        let before = try await client.status(at: "a").storeBytes
        try await client.archive(FixtureData.qwen, at: "a")
        #expect(try await client.activity(at: "a").first?.text == "Moved to the archive disk: 13 files, 16.08 GB freed on this Mac")
        #expect(try await client.status(at: "a").storeBytes == before - 16_080_000_000)
        #expect(try await client.hubModel(FixtureData.qwen, at: "a").variants.allSatisfy { !$0.weightsStored })
        try await agree()
        try await client.restore(FixtureData.qwen, at: "a")
        #expect(try await client.activity(at: "a").first?.text == "Brought back from the archive disk: 13 files, 16.08 GB")
        #expect(try await model(FixtureData.qwen).apps.isEmpty)
        #expect(try await client.status(at: "a").storeBytes == before)

        // Forgotten: gone from the library.
        try await client.forget(FixtureData.qwen, at: "a")
        #expect(try await client.models(at: "a").count == 4)
        await #expect(throws: ClientError.self) { try await client.model(FixtureData.qwen, at: "a") }
        try await agree()

        // Cleaned up: what was removed is not offered again.
        let plan = try await client.gcPlan(offload: false, at: "a")
        let done = try await client.gcApply(orphans: plan.orphans.map(\.sha256), offload: [], at: "a")
        #expect(done.freed == FixtureData.orphanBytes)
        #expect(try await client.gcPlan(offload: false, at: "a").orphans.isEmpty)
        try await agree()
    }

    @Test("The canned history agrees with the canned library")
    func cannedHistory() {
        for entry in FixtureData.activity where entry.action == .activate {
            let model = FixtureData.models.first { $0.repo == entry.repo }
            #expect(model?.appNames.contains { $0 == entry.app } == entry.ok, "\(entry.text)")
        }
    }

    @Test("A canned download that finished left its model in the library, in the apps it names")
    func cannedFinishedDownloads() {
        for job in FixtureData.jobs where job.state == .done {
            let model = FixtureData.models.first { $0.repo == job.repo }
            #expect(model != nil, "\(job.model)")
            #expect(job.activated.allSatisfy { model?.appNames.contains($0) == true }, "\(job.model)")
        }
    }

    @Test("The canned daemon refuses where the real one does")
    func cannedRefusals() async throws {
        let client = FixtureDaemonClient(scenario: .running)
        // A repository the provider does not have.
        await #expect(throws: ClientError.self) {
            try await client.hubModel("org/not-on-hugging-face", at: "a")
        }
        // A name that is none.
        await #expect(throws: ClientError.self) {
            try await client.enqueue(PullRequest(model: "org/re..po", apps: []), at: "a")
        }
        // An app the profiles say no for does not take a stored model.
        let asked = try await client.enqueue(
            PullRequest(model: FixtureData.nemotron, apps: [.bionic, .ollama], kind: .activation), at: "a"
        )
        let done = try await client.download(asked.id, at: "a").job
        #expect(done.activated == [.bionic])
        #expect(done.notActivated.map(\.app) == [.ollama])
        #expect(done.notActivated.first?.reason.isEmpty == false)
        // A revision in the spec does not hide that the model is stored.
        let pinned = try await client.enqueue(PullRequest(model: "\(FixtureData.smolGGUF)@main", apps: [.bionic]), at: "a")
        #expect(try await client.download(pinned.id, at: "a").job.state == .done)
        // A model in an app is not forgotten; archived, it is in no app.
        await #expect(throws: ClientError.self) {
            try await client.forget(FixtureData.smolGGUF, at: "a")
        }
        try await client.archive(FixtureData.smolGGUF, at: "a")
        try await client.forget(FixtureData.smolGGUF, at: "a")
        // An action a job's state does not allow, in the queue's words.
        do {
            try await client.downloadAction(.pause, id: done.id, at: "a")
            Issue.record("a finished job was paused")
        } catch {
            #expect(describe(error) == "download \(done.id) is done; it cannot be paused now")
        }
    }
}
