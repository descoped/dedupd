import DedupdClient
import Foundation
import Testing

@testable import Dedupd

@Suite("What the screens say and decide from the daemon's answers")
struct WordsTests {
    private func variant(
        stored: UInt64 = 0, weights: Bool = false, usable: [ModelApp] = [.bionic, .ollama], placed: [ModelApp] = []
    ) -> Variant {
        Variant(
            kind: .gguf, format: .gguf, name: "Q4_K_M", spec: "org/Repo-GGUF:Q4_K_M", files: 4, bytes: 1000,
            storedBytes: stored, weightsStored: weights, usableBy: usable, placedIn: placed, decisions: []
        )
    }

    @Test("A stored variant goes only to the chosen apps that lack it; one to download goes to all of them")
    func targets() {
        let chosen: [ModelApp] = [.bionic, .ollama]
        #expect(variant().targets(of: chosen) == chosen)
        #expect(variant(stored: 1000, placed: [.bionic]).targets(of: chosen) == [.ollama])
        #expect(variant(stored: 1000, placed: chosen).targets(of: chosen).isEmpty)
        #expect(variant(stored: 990, weights: true, placed: [.ollama]).targets(of: chosen) == [.bionic])
    }

    @Test("An app can be given the variants it can use, lacks, and whose weights are stored")
    func activatable() {
        let here = variant(stored: 1000)
        let placed = variant(stored: 1000, placed: [.ollama])
        let away = variant()
        let notForIt = variant(stored: 1000, usable: [.bionic])
        #expect([here, placed, away, notForIt].activatable(in: .ollama) == [here])
    }

    @Test("Where a variant's files are is said in words")
    func storedWords() {
        #expect(variant(stored: 1000).storedWords(archived: false) == "stored")
        #expect(variant().storedWords(archived: false) == "not on this Mac")
        #expect(variant().storedWords(archived: true) == "on the archive disk")
        #expect(variant(stored: 400).storedWords(archived: false) == "400 B stored")
        #expect(variant(stored: 990, weights: true).storedWords(archived: false).hasPrefix("stored; 10 B of small files"))
        #expect(variant(stored: 1000, placed: [.bionic]).hubDetail == "1.00 KB · 4 files · in Bionic")
        #expect(variant(stored: 1000).hubDetail == "1.00 KB · 4 files · in the store")
        #expect(variant().fetchHelp(into: []) == "Downloads 1.00 KB and keeps it in the store")
        #expect(variant(stored: 1000).fetchHelp(into: [.ollama])
            == "Already stored: activates it in Ollama as a clone, without downloading")
    }

    @Test("The job of a variant, and whether an app is being activated")
    func jobs() {
        let jobs = [job(1, .done), job(2, .paused), job(3, .downloading, model: "org/Other-GGUF:Q8_0")]
        #expect(jobs.queued(for: variant())?.id == 2, "a finished job is not in the queue")
        #expect(jobs.isActivating("org/Other-GGUF", in: .ollama))
        #expect(!jobs.isActivating("org/Other-GGUF", in: .huggingFace))
        #expect(!jobs.isActivating("org/Repo-GGUF", in: .ollama), "paused is not under way")
        #expect(job(4, .paused, done: 40_000_000).line.hasPrefix("Paused · "))
    }

    @Test("Start Now on an activation speaks of the activation")
    func startHelp() {
        #expect(JobAction.start.help(activation: true) == "Start this activation ahead of the queue")
        #expect(JobAction.start.help(activation: false) == "Start this download ahead of the queue")
    }

    @Test("A count that rounds to a thousand of its unit is one of the next")
    func roundedCounts() {
        #expect(ByteFormat.count(999_950) == "1M")
        #expect(ByteFormat.count(999_499) == "999k")
        #expect(ByteFormat.count(999_950_000) == "1B")
        #expect(ByteFormat.parameters(999_999_999_999) == "1T")
        #expect(ByteFormat.count(999) == "999")
    }

    @Test("The name and the organisation of a repository")
    func repoName() {
        #expect(RepoName.name("org/Repo-GGUF") == "Repo-GGUF")
        #expect(RepoName.organisation("org/Repo-GGUF") == "org")
        #expect(RepoName.name("alone") == "alone")
        #expect(HuggingFace.page("org/Repo-GGUF")?.absoluteString == "https://huggingface.co/org/Repo-GGUF")
    }

    @Test("What a deactivation removes, a folder's note and the token line")
    func sentences() {
        let nothing = UnplacePlan(repo: "org/r", app: .bionic, remove: [], links: [], keep: [], bytes: 0, removed: nil)
        #expect(nothing.summary.hasPrefix("Nothing can be removed"))
        let some = UnplacePlan(
            repo: "org/r", app: .bionic, remove: [PlannedFile(path: "/a", size: 1000)], links: [], keep: [],
            bytes: 1000, removed: nil
        )
        #expect(some.summary.hasPrefix("1 file, 1.00 KB, are deleted from the app's folder."))
        #expect(FixtureData.locations.modelBytes
            == FixtureData.locations.store.bytes + FixtureData.locations.appBytes - FixtureData.locations.sharedBytes)
        #expect(AgentState(
            label: "l", plist: "p", installed: false, running: true, home: "h", program: "x", installedVersion: nil,
            version: "0.2.0", log: "g"
        ).words == "The daemon is running, but its agent file is gone")
        #expect(UpstreamCheck(upstream: .gone, commit: nil, latest: nil, changed: false).words.hasPrefix("Hugging Face no longer"))
        #expect(UpstreamCheck(upstream: .ok, commit: "a", latest: "0123456789abcdef", changed: true).words
            == "Hugging Face has a newer revision, 0123456789ab. Download it from Discover.")
    }

    @Test("Markdown's inline marks are read, and JSON is indented with its keys in order")
    func markdown() {
        #expect(String(Markdown.inline("a **b** c").characters) == "a b c")
        #expect(Markdown.indented(json: #"{"b":1,"a":2}"#) == "{\n  \"a\" : 2,\n  \"b\" : 1\n}")
        #expect(Markdown.indented(json: "not json") == "not json")
    }
}
