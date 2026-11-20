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
    }
}
