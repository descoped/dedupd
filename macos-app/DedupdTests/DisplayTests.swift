import DedupdClient
import Testing

@testable import Dedupd

@Suite("How models, pulls and scans are worded")
struct DisplayTests {
    private func repository(
        files: UInt64 = 4,
        stored: UInt64 = 4,
        commit: String? = "6067b15cf58100000000000000000000000000000",
        mismatch: UInt64 = 0,
        notUpstream: UInt64 = 0,
        unchecked: UInt64 = 0
    ) -> Repository {
        Repository(
            provider: "hf",
            name: "org/repo",
            commit: commit,
            storedFiles: stored,
            storedBytes: 100,
            files: files,
            bytes: 100,
            verified: files - mismatch - notUpstream - unchecked,
            mismatch: mismatch,
            notUpstream: notUpstream,
            unchecked: unchecked
        )
    }

    @Test("A model whose app files all match is verified")
    func verified() {
        let repo = repository()
        #expect(repo.health == .verified)
        #expect(repo.detail == "4 stored · 4 in apps · main@6067b15cf581")
    }

    @Test("A file that differs from upstream outranks everything else")
    func different() {
        let repo = repository(mismatch: 1, unchecked: 1)
        #expect(repo.health == .different)
        #expect(repo.detail == "4 stored · 4 in apps · main@6067b15cf581 · 1 different from upstream · 1 not checked")
    }

    @Test("Unlisted or unchecked files leave the model unconfirmed")
    func unconfirmed() {
        #expect(repository(notUpstream: 2).health == .unconfirmed)
        #expect(repository(unchecked: 1).health == .unconfirmed)
    }

    @Test("A model no app holds is in the store only, with no commit to show when none is known")
    func storedOnly() {
        let repo = repository(files: 0, stored: 8, commit: nil)
        #expect(repo.health == .storedOnly)
        #expect(repo.shortCommit == nil)
        #expect(repo.detail == "8 stored · 0 in apps")
    }

    @Test("A pull says what was downloaded and where it was placed")
    func pullSummary() {
        let result = PullResult(
            repo: "org/repo",
            commit: "c",
            fromArchive: false,
            files: 4,
            bytes: 106_070_000,
            downloaded: 1,
            placed: [
                PlacedApp(app: "bionic", cloned: 1, present: 3, conflicts: []),
                PlacedApp(app: "ollama", cloned: 0, present: 0, conflicts: ["/models/blobs/sha256-abc"]),
            ],
            notes: ["not placed into Ollama: it cannot use a GGUF that is split into parts"]
        )
        #expect(result.summary == "4 files, 106.07 MB. Downloaded 1; 3 already in the store.")
        #expect(result.placements == [
            "Bionic: 1 placed as clones, 3 already there",
            "Ollama: 0 placed as clones, 0 already there",
        ])
        #expect(result.warnings == [
            "not placed into Ollama: it cannot use a GGUF that is split into parts",
            "Ollama: left alone /models/blobs/sha256-abc",
        ])
    }

    @Test("A pull answered from the archive says so")
    func pullFromArchive() {
        let result = PullResult(
            repo: "org/repo", commit: "c", fromArchive: true, files: 1, bytes: 10, downloaded: 0, placed: [], notes: []
        )
        #expect(result.summary.hasSuffix("From the archive: Hugging Face could not be asked."))
        #expect(result.summary.hasPrefix("1 file, 10 B."))
    }

    @Test("A scan that changed nothing says so")
    func quietScan() {
        let scan = ScanResult(
            forgotten: 0, adopted: 0, adoptedBytes: 0, reclaimed: 0, reclaimedBytes: 0, archived: 0, errors: []
        )
        #expect(scan.summary == "Nothing new in the app folders.")
    }

    @Test("A scan lists what it did, as one sentence")
    func busyScan() {
        let scan = ScanResult(
            forgotten: 1,
            adopted: 2,
            adoptedBytes: 4_920_000_000,
            reclaimed: 2,
            reclaimedBytes: 4_920_000_000,
            archived: 2,
            errors: []
        )
        #expect(
            scan.summary == "Adopted 2 files (4.92 GB), freed 4.92 GB by sharing 2 files, "
                + "copied 2 files to the archive, forgot 1 file no longer there."
        )
    }

    @Test("App names the daemon uses map to titles; unknown names pass through")
    func appNames() {
        #expect(ModelApp.title(for: "hf") == "Hugging Face cache")
        #expect(ModelApp.title(for: "bionic") == "Bionic")
        #expect(ModelApp.title(for: "somethingelse") == "somethingelse")
        #expect(ModelApp.allCases.map(\.rawValue) == ["bionic", "ollama", "hf"])
    }
}
