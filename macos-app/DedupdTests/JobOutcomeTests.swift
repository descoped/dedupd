import DedupdClient
import Foundation
import Testing

@testable import Dedupd

/// What a finished job says about the apps it was asked for.
struct JobOutcomeTests {
    @Test("A finished download says which apps took the model and why the others did not")
    func outcome() {
        let request = PullRequest(model: "org/Repo-MLX", apps: ["bionic", "ollama"])
        let all = FixtureData.job(id: 1, request: request, state: .done)
        #expect(all.outcomeText == "Activated in Bionic and Ollama")
        #expect(all.symbol == JobState.done.symbol)

        let refusal = "Ollama 0.40.16 can't run this model: it doesn't support the model type X for MLX models."
        let one = FixtureData.job(id: 2, request: request, state: .done, notTakenBy: ["ollama": refusal])
        #expect(one.missed.map(\.app) == ["ollama"])
        #expect(one.outcomeText == "Activated in Bionic · not in Ollama: \(refusal)")
        #expect(one.symbol == "exclamationmark.circle.fill")

        let none = FixtureData.job(
            id: 3,
            request: PullRequest(model: "org/Repo-MLX", apps: ["ollama"]),
            state: .done,
            notTakenBy: ["ollama": refusal]
        )
        #expect(none.statusText == "Not activated in Ollama: \(refusal)")

        // Two apps, each with its own reason.
        let two = FixtureData.job(
            id: 6, request: request, state: .done,
            notTakenBy: ["bionic": "files in its folder with other content were left alone (1)", "ollama": refusal]
        )
        #expect(two.outcomeText == "Not activated in Bionic and Ollama: "
            + "Bionic: files in its folder with other content were left alone (1); Ollama: \(refusal)")

        // A download that is still running has nothing to say about apps.
        #expect(FixtureData.job(id: 4, request: request, state: .downloading).missed.isEmpty)
        #expect(FixtureData.job(id: 5, request: PullRequest(model: "org/Repo-MLX", apps: []), state: .done)
            .outcomeText == "In the store only")

        // A daemon older than the app says nothing per app: its jobs are not
        // called failures.
        var older = FixtureData.job(id: 7, request: request, state: .done)
        older.kind = ""
        older.activated = []
        #expect(older.missed.isEmpty)
        #expect(older.outcomeText == "Done")
        #expect(older.symbol == JobState.done.symbol)
    }
}
