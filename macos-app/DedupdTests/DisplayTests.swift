import DedupdClient
import Foundation
import Testing

@testable import Dedupd

@Suite("What the app says about models and downloads")
struct DisplayTests {
    @Test(
        "Counts and parameters are written as people say them",
        arguments: [(950, "950"), (1200, "1.2k"), (104_300, "104k"), (2_410_000, "2.4M"), (1_000_000, "1M")]
    )
    func counts(value: UInt64, text: String) {
        #expect(ByteFormat.count(value) == text)
    }

    @Test("Parameters", arguments: [(134_515_008, "135M"), (1_700_000_000, "1.7B"), (27_400_000_000, "27B")])
    func parameters(value: UInt64, text: String) {
        #expect(ByteFormat.parameters(value) == text)
    }

    @Test("Durations", arguments: [(45, "45 s"), (60, "1 min"), (700, "12 min"), (3600, "1 h"), (7500, "2 h 5 min")])
    func durations(seconds: UInt64, text: String) {
        #expect(ByteFormat.duration(seconds: seconds) == text)
    }

    @Test("Speed")
    func speed() {
        #expect(ByteFormat.speed(11_800_000) == "11.80 MB/s")
    }

    @Test("The provider's timestamps are read with and without fractions of a second")
    func timestamps() {
        #expect(TimeFormat.date(iso: "2026-01-02T03:04:05.000Z") == Date(timeIntervalSince1970: 1_767_323_045))
        #expect(TimeFormat.date(iso: "2026-01-02T03:04:05Z") == Date(timeIntervalSince1970: 1_767_323_045))
        #expect(TimeFormat.date(iso: "yesterday") == nil)
        let now = Date(timeIntervalSince1970: 1_767_323_045)
        #expect(TimeFormat.relative(now.addingTimeInterval(-20), to: now) == "just now")
    }

    @Test("A state the app does not know is shown as unknown, not dropped")
    func unknownState() {
        #expect(JobState("queued") == .queued)
        #expect(JobState("waiting_server") == .waitingServer)
        #expect(JobState("something new") == .unknown)
        #expect(!JobState.unknown.isActive)
    }

    @Test("A running download reads its progress, speed and time left")
    func running() {
        let running = job(1, .downloading, done: 40_000_000)
        #expect(running.fraction == 0.4)
        #expect(running.secondsLeft == 6)
        #expect(running.progressText == "40.00 MB of 100.00 MB")
        #expect(running.statusText == "10.00 MB/s · 6 s left")
        #expect(running.appList == "Bionic and Ollama")
    }

    @Test("A waiting download gives the daemon's reason; a finished one says where it went")
    func otherStates() {
        var waiting = job(1, .waitingServer)
        waiting.detail = "The server asked for a pause of 30 s"
        #expect(waiting.statusText == "The server asked for a pause of 30 s")
        #expect(waiting.secondsLeft == nil)

        let done = job(2, .done)
        #expect(done.fraction == 1)
        #expect(done.progressText == "100.00 MB")
        #expect(done.statusText == "Placed into Bionic and Ollama")

        var unsized = job(3, .resolving)
        unsized.bytesTotal = 0
        #expect(unsized.fraction == nil)
        #expect(unsized.progressText == "Looking up")

        var storeOnly = job(4, .done)
        storeOnly.apps = []
        #expect(storeOnly.appList == "the store only")
    }

    @Test("A model's state against Hugging Face")
    func health() {
        let models = Dictionary(uniqueKeysWithValues: FixtureData.models.map { ($0.repo, $0) })
        #expect(models[FixtureData.smolGGUF]?.health == .verified)
        #expect(models[FixtureData.nemotron]?.health == .goneUpstream)
        #expect(models[FixtureData.smolMLX]?.health == .storedOnly)
        var changed = FixtureData.models[0]
        changed.mismatch = 1
        #expect(changed.health == .different)
        changed.mismatch = 0
        changed.unchecked = 2
        #expect(changed.health == .unconfirmed)
    }

    @Test("A model's name, maker and one-line description")
    func modelWords() {
        let model = FixtureData.models[0]
        #expect(model.name == "SmolLM2-135M-Instruct-GGUF")
        #expect(model.organisation == "bartowski")
        #expect(model.summary == "GGUF · 135M · apache-2.0")
        #expect(model.shortCommit == "09816acd5d99")
        #expect(model.meta.plainTags == ["conversational"])
    }

    @Test("Each kind of model has its own apps")
    func kinds() {
        let single = Variant(
            kind: "gguf", name: "Q4_K_M", spec: "a/b:Q4_K_M", files: 1, bytes: 10, storedBytes: 10,
            usableBy: ["bionic", "ollama", "hf"], placedIn: ["ollama"],
            decisions: FixtureData.decisions(usableBy: ["bionic", "ollama", "hf"])
        )
        var split = single
        split.usableBy = ["bionic", "hf"]
        var files = single
        files.kind = "files"
        #expect(single.kind(formats: ["gguf"]) == .gguf)
        #expect(split.kind(formats: ["gguf"]) == .splitGGUF)
        #expect(files.kind(formats: ["mlx", "safetensors"]) == .mlx)
        #expect(files.kind(formats: ["safetensors"]) == .other)
        #expect(single.isStored)
        #expect(single.isPlaced(in: .ollama))
        #expect(!single.isPlaced(in: .bionic))
        #expect(split.usableApps == [.bionic, .huggingFace])
        #expect(ModelKind.gguf.defaultApps == [.bionic, .ollama])
        #expect(ModelKind.mlx.defaultApps == [.bionic])
    }

    @Test("An app without the model says why, in the daemon's words")
    func notes() {
        var mlx = Variant(
            kind: "files", name: "all files", spec: "a/b", files: 8, bytes: 10, storedBytes: 10,
            usableBy: ["bionic", "hf"], placedIn: [],
            decisions: FixtureData.decisions(usableBy: ["bionic", "hf"], mlx: true)
        )
        #expect([mlx].note(for: .bionic) == "Does not have it")
        #expect([mlx].note(for: .ollama) == "Ollama can share this model with the store, but dedupd does not do that yet")
        #expect(mlx.decision(for: .ollama)?.cost == "megabytes")
        #expect([Variant]().note(for: .ollama) == "Does not have it")

        // A version of the app the daemon was not tested with is named.
        mlx.decisions[0].version = "9.9"
        mlx.decisions[0].versionState = "untested"
        #expect([mlx].note(for: .bionic) == "Does not have it · dedupd was not tested with version 9.9")

        // A daemon from before the profiles sends no decisions.
        mlx.decisions = []
        #expect([mlx].note(for: .ollama) == "Cannot use this model")
    }

    @Test(
        "Text that names a repository is told from words to search for",
        arguments: [
            ("org/repo", "org/repo"),
            ("  https://huggingface.co/org/repo  ", "org/repo"),
            ("https://huggingface.co/org/repo/tree/main", "org/repo"),
            ("hf.co/org/repo", "org/repo"),
        ]
    )
    func repositoryNames(text: String, repo: String) {
        #expect(ModelSpec.repository(in: text) == repo)
    }

    @Test("Words are not a repository", arguments: ["qwen", "qwen gguf", "org/repo:Q4_K_M", "a/b c", ""])
    func words(text: String) {
        #expect(ModelSpec.repository(in: text) == nil)
    }

    @Test("A README is split into headings, paragraphs, lists and code, without its metadata block")
    func markdown() {
        let blocks = Markdown.parse("""
        ---
        license: mit
        ---
        # Title

        First line
        second line.

        - one
        * two
        ```sh
        ollama run x
        ```
        ## Next
        """)
        #expect(blocks == [
            .heading(level: 1, text: "Title"),
            .paragraph("First line second line."),
            .item("one"),
            .item("two"),
            .code("ollama run x"),
            .heading(level: 2, text: "Next"),
        ])
    }

    @Test("Only what the owner changed is sent to the daemon")
    func settingsDraft() {
        let original = SettingsDraft(FixtureData.settings)
        #expect(original.changes(from: original).isEmpty)

        var draft = original
        draft.maxConnections = 4
        draft.speedLimit = 2.5
        draft.archive = ""
        draft.reclaim = false
        draft.listen = " 127.0.0.1:9000 "
        #expect(draft.changes(from: original) == [
            SettingChange(key: "listen", value: .text(value: "127.0.0.1:9000")),
            SettingChange(key: "archive", value: .unset),
            SettingChange(key: "reclaim", value: .flag(value: false)),
            SettingChange(key: "max_connections", value: .number(value: 4)),
            SettingChange(key: "speed_limit", value: .number(value: 2_500_000)),
        ])
    }

    @Test("The owner hears about a download that finished or needs them, once")
    func notices() {
        let before = [job(1, .downloading), job(2, .downloading), job(3, .queued)]
        var stuck = job(2, .needsAttention)
        stuck.detail = "The disk is full."
        let after = [job(1, .done), stuck, job(3, .downloading), job(4, .done)]

        let notices = Notice.notices(from: before, to: after)

        #expect(notices == [
            Notice(
                id: "download-1",
                title: "Downloaded org/Repo-GGUF:Q4_K_M",
                body: "100.00 MB, placed into Bionic and Ollama."
            ),
            Notice(id: "download-2", title: "org/Repo-GGUF:Q4_K_M needs attention", body: "The disk is full."),
        ])
        #expect(Notice.notices(from: after, to: after).isEmpty)
        #expect(Notice.notices(from: [], to: after).isEmpty, "what was already done when the app started is old news")
    }

    @Test("The agent's state is read from what the command prints")
    func agentState() throws {
        let json = """
        {"label": "io.descoped.dedupd.daemon", "plist": "/p.plist", "installed": true, "running": false, "home": "/h",
         "program": "/h/bin/dedupd", "installed_version": "0.0.9", "version": "0.1.0", "log": "/h/daemon.log"}
        """
        let state = try JSONDecoder().decode(AgentState.self, from: Data(json.utf8))
        #expect(state.installed)
        #expect(!state.running)
        #expect(state.installedVersion == "0.0.9")
        #expect(state.needsUpdate)

        let missing = try JSONDecoder().decode(
            AgentState.self,
            from: Data(json.replacingOccurrences(of: "\"0.0.9\"", with: "null").utf8)
        )
        #expect(!missing.needsUpdate)
    }

    @Test("A trial's home folder and label go on the command line")
    func agentArguments() {
        #expect(BundledAgent().arguments(["agent", "--json"]) == ["agent", "--json"])
        #expect(
            BundledAgent(home: "/tmp/home", label: "io.descoped.dedupd.trial").arguments(["install", "--env"])
                == ["--home", "/tmp/home", "install", "--env", "--label", "io.descoped.dedupd.trial"]
        )
    }
}
