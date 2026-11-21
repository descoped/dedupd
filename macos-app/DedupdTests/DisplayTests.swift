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

    @Test("Where the likes on Hugging Face stand, in a sentence")
    func likes() {
        let none = LikesSync(hasToken: false, user: nil, syncedAt: nil, problem: nil)
        #expect(none.summary.contains("hf auth login"))
        let waiting = LikesSync(hasToken: true, user: nil, syncedAt: nil, problem: nil)
        #expect(waiting.summary == "Your likes on Hugging Face have not been read yet.")
        let failed = LikesSync(hasToken: true, user: "example", syncedAt: 1, problem: "Your likes could not be read.")
        #expect(failed.summary == "Your likes could not be read.")
        let read = LikesSync(hasToken: true, user: "example", syncedAt: Int64(Date().timeIntervalSince1970), problem: nil)
        #expect(read.summary == "Likes of example on Hugging Face, read just now.")
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
        #expect(done.statusText == "Activated in Bionic and Ollama")

        var unsized = job(3, .resolving)
        unsized.bytesTotal = 0
        #expect(unsized.fraction == nil)
        #expect(unsized.progressText == "Looking up")

        var storeOnly = job(4, .done)
        storeOnly.apps = []
        #expect(storeOnly.appList == "the store only")
        #expect(storeOnly.statusText == "In the store only")
    }

    @Test("An archived model says so, and where its files are")
    func archivedModel() {
        var model = FixtureData.models[0]
        #expect(!model.isArchived)
        #expect(model.storedText == "4 files, 105.46 MB")
        model.apps = []
        model.archivedAt = 1_794_000_000
        #expect(model.health == .archived)
        #expect(model.storedText == "4 files, 105.46 MB, in an archive only")
        model.upstream = .gone
        #expect(model.health == .goneUpstream, "that the provider lost the model matters more")
    }

    @Test("Archiving is described step by step before it is done")
    func archivePlan() {
        var plan = ArchivePlan(
            repo: "org/Repo", archive: "/Volumes/Drawer/dedupd", archiveName: "Drawer",
            deactivate: [
                DeactivateStep(app: .bionic, files: 13, bytes: 16_080_000_000, keep: []),
                DeactivateStep(app: .ollama, files: 5, bytes: 16_100_000_000, keep: []),
            ],
            files: 13, bytes: 16_080_000_000, copyFiles: 0, copyBytes: 0, keep: []
        )
        #expect(plan.appList == "Bionic and Ollama")
        #expect(plan.steps == [
            "Every copy in Drawer is read in full and checked. Nothing is removed unless all are good.",
            "The model is taken out of Bionic and Ollama.",
            "13 files, 16.08 GB, are removed from this Mac. "
                + "The copy in Drawer (/Volumes/Drawer/dedupd) stays, and Restore brings the model back.",
        ])
        plan.copyFiles = 2
        plan.copyBytes = 1_200_000_000
        #expect(plan.steps.first == "2 files, 1.20 GB, are copied to Drawer first, because it does not have them yet.")
        #expect(!plan.isEmpty)
        plan.deactivate = []
        plan.files = 0
        #expect(plan.isEmpty)
    }

    @Test("A model on its way to the archive disk or back says how far it is")
    func modelMove() {
        var move = ModelMove(
            kind: .archive, repo: "org/Repo", archive: "Drawer", running: true, startedAt: 1, finishedAt: nil,
            bytesTotal: 16_000_000_000, bytesDone: 4_000_000_000, files: 0, kept: 0, error: nil
        )
        #expect(move.fraction == 0.25)
        #expect(move.statusText == "Archiving to Drawer: 4.00 GB of 16.00 GB checked there")
        move.kind = .restore
        #expect(move.statusText == "Restoring: 4.00 GB of 16.00 GB copied back")
        move.running = false
        move.files = 13
        move.bytesDone = 16_000_000_000
        #expect(move.fraction == nil)
        #expect(move.statusText == "Restored: 13 files, 16.00 GB")
        move.error = "the archive disk is not attached (/Volumes/Archive not found)"
        #expect(move.statusText == "Restoring failed: the archive disk is not attached (/Volumes/Archive not found)")
    }

    @Test("Archive and Restore say why they cannot be used")
    func archiveHelp() {
        var status = FixtureData.status()
        #expect(ArchiveWords.canMove(status))
        #expect(ArchiveWords.archiveHelp(status).hasSuffix("You choose the archive; its copy stays."))
        // One disk away: the other archive still serves.
        status.archives[0].attached = false
        #expect(ArchiveWords.canMove(status))
        status.archives[1].attached = false
        #expect(!ArchiveWords.canMove(status))
        #expect(ArchiveWords.archiveHelp(status) == "No archive's disk is attached (Archive and Drawer)")
        #expect(ArchiveWords.restoreHelp(status) == "No archive's disk is attached (Archive and Drawer)")
        status.archives = []
        #expect(ArchiveWords.archiveHelp(status) == "Add an archive on the Storage screen first")
        #expect(ArchiveWords.archiveHelp(nil) == "Add an archive on the Storage screen first")
        #expect(ArchiveWords.restoreHelp(status) == "No archive is set")
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

    @Test("A model's name and maker")
    func modelWords() {
        let model = FixtureData.models[0]
        #expect(model.name == "SmolLM2-135M-Instruct-GGUF")
        #expect(model.organisation == "bartowski")
        #expect(model.shortCommit == "09816acd5d99")
        #expect(model.meta.plainTags == ["conversational"])
    }

    /// A variant of 10 bytes with the recorded answers of the profiles.
    private static func variant(
        _ format: ModelFormat, _ decisions: [Decision], storedBytes: UInt64 = 10, weightsStored: Bool? = nil,
        placedIn: [ModelApp] = []
    ) -> Variant {
        let judged = FixtureData.Judged(format: format, decisions: decisions)
        return Variant(
            kind: format == .gguf ? .gguf : .files, format: format, name: "v", spec: "a/b", files: 1, bytes: 10,
            storedBytes: storedBytes, weightsStored: weightsStored ?? (storedBytes >= 10), usableBy: judged.usableBy,
            placedIn: placedIn, decisions: decisions, copies: []
        )
    }

    @Test("A variant whose weights are stored can be activated; the small files it lacks are fetched")
    func smallFilesMissing() {
        let whole = Self.variant(.mlx, RecordedProfiles.mlx4)
        let lacking = Self.variant(.mlx, RecordedProfiles.mlx4, storedBytes: 7, weightsStored: true)
        let none = Self.variant(.mlx, RecordedProfiles.mlx4, storedBytes: 0)
        #expect(whole.canActivate && !whole.placeHelp(for: .ollama).contains("fetched"))
        #expect(lacking.canActivate && !lacking.isStored)
        #expect(lacking.missingBytes == 3)
        #expect(lacking.placeHelp(for: .ollama).hasSuffix("First the 3 B of small files it lacks are fetched."))
        #expect(!none.canActivate)
    }

    @Test("Which apps take a variant is the profiles' answer")
    func usable() {
        let single = Self.variant(.gguf, RecordedProfiles.gguf, storedBytes: 10, placedIn: [.ollama])
        let split = Self.variant(.gguf, RecordedProfiles.splitGGUF, storedBytes: 0)
        #expect(single.usableApps == [.bionic, .ollama, .huggingFace])
        #expect(single.isStored)
        #expect(single.isPlaced(in: .ollama))
        #expect(!single.isPlaced(in: .bionic))
        #expect(split.format == .gguf, "a GGUF in parts is still a GGUF")
        #expect(split.usableApps == [.bionic, .huggingFace])
        #expect(!split.isStored)
    }

    @Test("Which apps take a format, and where a new model goes, come from the daemon")
    func formats() {
        let apps = RecordedProfiles.apps
        #expect(apps.apps(taking: .gguf) == [.bionic, .ollama, .huggingFace])
        #expect(apps.apps(taking: .mlx) == [.bionic, .ollama, .huggingFace])
        #expect(apps.apps(taking: .other) == [.huggingFace])
        // At no cost, and the Hugging Face cache only when nothing else takes it.
        #expect(apps.defaultApps(for: .gguf) == [.bionic, .ollama])
        #expect(apps.defaultApps(for: .mlx) == [.bionic], "Ollama takes MLX for some megabytes, so not by default")
        #expect(apps.defaultApps(for: .other) == [.huggingFace])
        #expect([AppInfo]().defaultApps(for: .gguf).isEmpty)
    }

    @Test("An app without the model says why, in the daemon's words")
    func notes() {
        var mlx = Self.variant(.mlx, RecordedProfiles.mlx6)
        #expect([mlx].note(for: .bionic) == "Does not have it")
        #expect(
            [mlx].note(for: .ollama)
                == "Ollama 0.40.16 cannot run 6-bit MLX models; it runs 4-bit and 8-bit ones"
        )
        mlx = Self.variant(.mlx, RecordedProfiles.mlx4)
        #expect([mlx].note(for: .ollama) == "Does not have it")
        #expect(mlx.decision(for: .ollama)?.cost == .megabytes)
        #expect(mlx.placeHelp(for: .ollama).hasSuffix("It takes a few megabytes."))
        #expect(mlx.placeHelp(for: .bionic).hasSuffix("It takes no extra space."))
        #expect([Variant]().note(for: .ollama) == "Does not have it")

        // A version of the app the daemon was not tested with is named.
        mlx.decisions[0].version = "9.9"
        mlx.decisions[0].versionState = .untested
        #expect([mlx].note(for: .bionic) == "Does not have it · dedupd was not tested with version 9.9")

        // A daemon from before the profiles sends no decisions.
        mlx.usableBy = [.bionic, .huggingFace]
        mlx.decisions = []
        #expect([mlx].note(for: .ollama) == "Cannot use this model")
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
        draft.hfCache = ""
        draft.reclaim = false
        draft.listen = " 127.0.0.1:9000 "
        #expect(draft.changes(from: original) == [
            SettingChange(key: .listen, value: .text(value: "127.0.0.1:9000")),
            SettingChange(key: .hfCache, value: .unset),
            SettingChange(key: .reclaim, value: .flag(value: false)),
            SettingChange(key: .maxConnections, value: .number(value: 4)),
            SettingChange(key: .speedLimit, value: .number(value: 2_500_000)),
        ])
    }

    @Test("The owner hears about a download that finished or needs them, once")
    func notices() {
        let before = [job(1, .downloading), job(2, .downloading), job(3, .queued)]
        var stuck = job(2, .needsAttention)
        stuck.detail = "The disk is full."
        let after = [job(1, .done), stuck, job(3, .downloading)]

        let notices = Notice.notices(from: before, to: after)

        #expect(notices == [
            Notice(
                id: "download-1",
                title: "Downloaded org/Repo-GGUF:Q4_K_M",
                body: "100.00 MB. Activated in Bionic and Ollama."
            ),
            Notice(id: "download-2", title: "org/Repo-GGUF:Q4_K_M needs attention", body: "The disk is full."),
        ])
        #expect(Notice.notices(from: after, to: after).isEmpty)
        #expect(Notice.notices(from: nil, to: after).isEmpty, "what was already done when the app started is old news")
        // A job quick enough never to be seen running is told about too.
        let quick = Notice.notices(from: after, to: after + [job(4, .done), job(5, .queued)])
        #expect(quick.map(\.id) == ["download-4"])
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

/// What the bundled command says about the installed daemon.
struct AgentStateTests {
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

        // Two builds can carry one version number; the command says whether
        // the installed daemon is the build inside this app.
        let sameVersion = json.replacingOccurrences(of: "\"0.0.9\"", with: "\"0.1.0\"")
        let unknown = try JSONDecoder().decode(AgentState.self, from: Data(sameVersion.utf8))
        #expect(!unknown.needsUpdate)
        let build = { (same: Bool) in
            try JSONDecoder().decode(
                AgentState.self,
                from: Data(sameVersion.replacingOccurrences(of: "{\"label\"", with: "{\"same_build\": \(same), \"label\"").utf8)
            )
        }
        #expect(try !build(true).needsUpdate)
        #expect(try build(false).needsUpdate)
        #expect(try build(false).updateReason == "The installed daemon is another build of version 0.1.0 than the one in this app.")
        #expect(state.updateReason == "The installed daemon is version 0.0.9; this app carries 0.1.0.")
    }
}

@Suite("What comes from outside is cut to what is safe")
struct OutsideValueTests {
    @Test("Only a link to the web is opened from a README")
    func readmeLinks() throws {
        for good in ["https://huggingface.co/org/name", "http://example.com/paper.pdf", "HTTPS://example.com"] {
            #expect(Markdown.mayOpen(try #require(URL(string: good))), "\(good)")
        }
        for bad in [
            "file:///Users/example/Documents", "smb://host/share", "ssh://host", "x-apple.systempreferences:x",
            "javascript:alert(1)", "mailto:someone@example.com", "https:///no-host", "relative/path", "ollama://run/x",
        ] {
            #expect(!Markdown.mayOpen(try #require(URL(string: bad))), "\(bad)")
        }
    }

    @Test("A speed limit of any size is sent as a number the daemon can read", arguments: [
        (1e15, UInt64(1_000_000_000_000)), (.infinity, UInt64(1_000_000_000_000)), (-5, UInt64(0)),
        (.nan, UInt64(0)), (0.0000004, UInt64(0)), (12.3456789, UInt64(12_345_679)),
    ])
    func speedLimit(typed: Double, sent: UInt64) {
        let original = SettingsDraft(FixtureData.settings)
        var draft = original
        draft.speedLimit = typed
        let changes = draft.changes(from: original)
        #expect(changes == [SettingChange(key: .speedLimit, value: .number(value: sent))])
    }
}
