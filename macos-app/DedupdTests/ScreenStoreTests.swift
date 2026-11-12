import DedupdClient
import Foundation
import Testing

@testable import Dedupd

@Suite("The library")
@MainActor
struct LibraryStoreTests {
    @Test("The list is read from the daemon and counted")
    func reload() async {
        let library = LibraryStore(daemon: await startedDaemon())
        #expect(!library.hasLoaded)
        await library.reload()
        #expect(library.hasLoaded)
        #expect(library.models.count == 5)
        #expect(library.countText == "5 models · 270.80 GB")
        #expect(library.formats == ["gguf", "mlx", "safetensors"])
    }

    @Test("Filters narrow the list by name, format and app")
    func filters() async {
        let library = LibraryStore(daemon: await startedDaemon())
        await library.reload()

        library.filterText = "smol"
        #expect(library.visible.map(\.repo) == [FixtureData.smolGGUF, FixtureData.smolMLX])
        #expect(library.countText == "2 of 5 models · 184.68 MB")

        library.filterFormat = "mlx"
        #expect(library.visible.map(\.repo) == [FixtureData.smolMLX])

        library.filterText = ""
        library.filterFormat = ""
        library.filterApp = "ollama"
        #expect(library.visible.map(\.repo) == [FixtureData.smolGGUF])
    }

    @Test("The list sorts by the column the owner chose")
    func sorting() async {
        let library = LibraryStore(daemon: await startedDaemon())
        await library.reload()
        library.sortOrder = [KeyPathComparator(\LibraryModel.bytes, order: .reverse)]
        #expect(library.visible.first?.repo == FixtureData.deepSeek)
        #expect(library.visible.last?.repo == FixtureData.smolMLX)
    }

    @Test("Selecting a model reads its details")
    func detail() async throws {
        let library = LibraryStore(daemon: await startedDaemon())
        await library.reload()
        library.selection = try #require(library.models.first { $0.repo == FixtureData.smolGGUF }).id
        #expect(library.detail == .idle)
        await library.loadDetail()
        let detail = try #require(library.detail.value)
        #expect(detail.model.repo == FixtureData.smolGGUF)
        #expect(detail.variants.first?.spec == "\(FixtureData.smolGGUF):Q4_K_M")
        #expect(detail.placements.count == 8)
    }

    @Test("Removing a model from an app shows what would be deleted before anything is")
    func removal() async throws {
        let library = LibraryStore(daemon: await startedDaemon())
        await library.reload()
        library.selection = try #require(library.models.first { $0.repo == FixtureData.smolGGUF }).id

        await library.planRemoval(from: "ollama")
        let plan = try #require(library.removal)
        #expect(plan.app == "ollama")
        #expect(plan.remove.count == 4)
        #expect(plan.removed == nil, "nothing is removed by asking")

        await library.applyRemoval()
        #expect(library.removal == nil)
    }

    @Test("Archiving a model shows what it does first; restoring brings it back into no app")
    func archiveAndRestore() async throws {
        let daemon = await startedDaemon()
        let library = LibraryStore(daemon: daemon)
        await library.reload()
        library.selection = try #require(library.models.first { $0.repo == FixtureData.qwen }).id

        await library.planArchive()
        let plan = try #require(library.archiving)
        #expect(plan.appList == "Bionic")
        #expect((plan.files, plan.bytes) == (13, 16_080_000_000))
        #expect(library.selected?.isArchived == false, "nothing is archived by asking")

        await library.applyArchive()
        #expect(library.archiving == nil)
        #expect(await eventually { await daemon.moving?.kind == "archive" })
        #expect(library.move == nil, "a move that ended well needs no line of its own")
        await library.reload()
        let archived = try #require(library.selected)
        #expect(archived.health == .archived)
        #expect(archived.apps.isEmpty)
        await library.loadDetail()
        #expect(library.detail.value?.placements.isEmpty == true)
        #expect(library.detail.value?.variants.first?.isStored == false)

        await library.restore()
        #expect(await eventually { await daemon.moving?.kind == "restore" })
        await library.reload()
        #expect(library.selected?.isArchived == false)
        #expect(library.selected?.apps.isEmpty == false, "the canned daemon knows the model as it was")
        #expect(daemon.problem == nil)

        // A model that is not archived cannot be restored; the daemon says so.
        await library.restore()
        #expect(daemon.problem == "\(FixtureData.qwen) is not archived")
    }

    @Test("A model an app still has cannot be forgotten; the daemon says why")
    func forgetRefused() async throws {
        let daemon = await startedDaemon()
        let library = LibraryStore(daemon: daemon)
        await library.reload()
        library.selection = try #require(library.models.first { $0.repo == FixtureData.smolGGUF }).id
        await library.forget()
        #expect(daemon.problem == "\(FixtureData.smolGGUF) still has files in bionic, ollama")
        #expect(library.selection != nil)
    }

    @Test("A model no app has can be forgotten")
    func forget() async throws {
        let daemon = await startedDaemon()
        let library = LibraryStore(daemon: daemon)
        await library.reload()
        library.selection = try #require(library.models.first { $0.repo == FixtureData.smolMLX }).id
        await library.forget()
        #expect(daemon.problem == nil)
        #expect(library.selection == nil)
    }

    @Test("Activating a stored variant asks the daemon for that app only")
    func place() async throws {
        let daemon = await startedDaemon()
        let library = LibraryStore(daemon: daemon)
        let variant = try #require(FixtureData.detail(FixtureData.smolMLX)?.variants.first)
        await library.place(variant, into: .bionic)
        #expect(await eventually { await daemon.jobs.first?.model == FixtureData.smolMLX })
        #expect(daemon.jobs.first?.apps == ["bionic"])
        #expect(daemon.jobs.first?.phase == .done, "what is stored is activated without a download")
    }

    @Test("With no daemon the failure is kept for the screen to show")
    func failure() async {
        let library = LibraryStore(daemon: makeDaemon(.notRunning))
        await library.reload()
        #expect(library.models.isEmpty)
        #expect(library.failure == "The dedupd daemon is not running at 127.0.0.1:8642.")
    }
}

@Suite("Searching Hugging Face")
@MainActor
struct DiscoverStoreTests {
    @Test("A search lists models and says there is more")
    func search() async {
        let discover = DiscoverStore(daemon: await startedDaemon())
        await discover.search()
        #expect(discover.hasSearched)
        #expect(discover.hits.count == 7)
        #expect(discover.next == "page-2")
        #expect(discover.hits.first?.localState == "In Bionic and Ollama")
    }

    @Test("Words and a format narrow the search")
    func filters() async {
        let discover = DiscoverStore(daemon: await startedDaemon())
        discover.text = "qwen"
        discover.format = "mlx"
        await discover.search()
        #expect(discover.hits.map(\.id) == ["mlx-community/Qwen3.8-9B-Instruct-4bit"])
        #expect(discover.next == nil)
    }

    @Test("The next page is appended, without repeating a model")
    func more() async {
        let discover = DiscoverStore(daemon: await startedDaemon())
        await discover.search()
        await discover.loadMore()
        #expect(discover.hits.count == 9)
        #expect(discover.next == nil)
        await discover.loadMore()
        #expect(discover.hits.count == 9)
    }

    @Test(
        "A model's name or its address opens that model instead of searching",
        arguments: [
            "bartowski/SmolLM2-135M-Instruct-GGUF",
            "https://huggingface.co/bartowski/SmolLM2-135M-Instruct-GGUF",
            "hf.co/bartowski/SmolLM2-135M-Instruct-GGUF/tree/main",
        ]
    )
    func direct(text: String) async throws {
        let discover = DiscoverStore(daemon: await startedDaemon())
        discover.text = text
        await discover.search()
        #expect(discover.selection == FixtureData.smolGGUF)
        #expect(discover.hits.isEmpty)
        await discover.loadDetail()
        let model = try #require(discover.detail.value)
        #expect(model.id == FixtureData.smolGGUF)
        #expect(model.variants.count == 6)
        #expect(model.variants.first { $0.name == "Q4_K_M" }?.isStored == true)
    }

    @Test("A model shown by name replaces the list and the text of the field")
    func show() async {
        let discover = DiscoverStore(daemon: await startedDaemon())
        await discover.search()
        discover.show(FixtureData.smolGGUF)
        #expect(discover.selection == FixtureData.smolGGUF)
        #expect(discover.text == FixtureData.smolGGUF)
        #expect(discover.hits.isEmpty)
        #expect(discover.hasSearched)
    }

    @Test(
        "An address that is not a model's on Hugging Face is not sent to the daemon",
        arguments: [
            ("https://modelscope.cn/models/Qwen/Qwen3.8-27B", "ModelScope is not supported yet."),
            ("https://evil.example/org/repo", "That is not a model's name"),
            ("https://huggingface.co@evil.example/org/repo", "That is not a model's name"),
        ]
    )
    func refused(text: String, says: String) async {
        // A daemon that is away: a question to it would come back as a failure.
        let discover = DiscoverStore(daemon: await startedDaemon(.notRunning))
        discover.open("org/earlier")
        discover.text = text
        await discover.search()
        #expect(discover.notice?.hasPrefix(says) == true)
        #expect(discover.failure == nil, "the daemon was not asked")
        #expect(discover.selection == nil)
        #expect(discover.hits.isEmpty)

        discover.text = ""
        await discover.search()
        #expect(discover.notice == nil, "the note goes when the text does")
    }

    @Test("Downloading a variant queues it for the chosen apps")
    func download() async throws {
        let daemon = await startedDaemon()
        let discover = DiscoverStore(daemon: daemon)
        discover.open("unsloth/Qwen3.8-9B-Instruct-GGUF")
        await discover.loadDetail()
        let variant = try #require(discover.detail.value?.variants.first { $0.name == "Q8_0" })

        await discover.download(variant, into: [.ollama])

        #expect(await eventually { await daemon.jobs.first?.model == "unsloth/Qwen3.8-9B-Instruct-GGUF:Q8_0" })
        #expect(daemon.jobs.first?.apps == ["ollama"])
        #expect(discover.detail.value?.downloading == true, "the model's page says it is being downloaded")
    }

    @Test("A search that fails keeps the reason")
    func failure() async {
        let discover = DiscoverStore(daemon: makeDaemon(.notRunning))
        await discover.search()
        #expect(discover.hits.isEmpty)
        #expect(discover.failure == "The dedupd daemon is not running at 127.0.0.1:8642.")
    }
}

@Suite("Storage and the service")
@MainActor
struct MaintenanceStoreTests {
    @Test("The storage screen reads the places, the clean-up plan and the last check")
    func storage() async throws {
        let storage = StorageStore(daemon: await startedDaemon())
        await storage.reload()
        let locations = try #require(storage.locations.value)
        #expect(locations.apps.map(\.id) == ["bionic", "ollama", "hf"])
        #expect(storage.plan.value?.orphans.count == 2)
        #expect(storage.plan.value?.offload.isEmpty == true)
        #expect(storage.hasSomethingToClean)
        #expect(storage.check?.checked == 61)
    }

    @Test("Files on the archive disk are listed for removal only when asked for")
    func offload() async {
        let storage = StorageStore(daemon: await startedDaemon())
        storage.includeOffload = true
        await storage.loadPlan()
        #expect(storage.plan.value?.offload.count == 1)
    }

    @Test("A clean-up removes what the plan lists and reports what it freed")
    func clean() async {
        let daemon = await startedDaemon()
        let storage = StorageStore(daemon: daemon)
        await storage.loadPlan()
        await storage.clean()
        #expect(storage.freed == 4_920_001_840)
        #expect(daemon.problem == nil)
    }

    @Test("The service screen reads the agent, the settings and the log")
    func service() async throws {
        let daemon = await startedDaemon()
        let service = ServiceStore(daemon: daemon, control: FixtureAgent())
        await service.reload()
        let agent = try #require(service.agent.value)
        #expect(agent.installed)
        #expect(!agent.needsUpdate)
        #expect(service.settings.value?.maxConnections == 8)
        #expect(service.log?.lines.count == 4)
    }

    @Test("A changed setting is sent to the daemon, which answers with the file's new state")
    func update() async {
        let daemon = await startedDaemon()
        let service = ServiceStore(daemon: daemon, control: FixtureAgent())
        await service.reload()
        await service.update([SettingChange(key: "speed_limit", value: .number(value: 5_000_000))])
        #expect(service.settings.value?.speedLimit == 5_000_000)
        #expect(service.settings.value?.restartNeeded == false, "the download limits take effect at once")
        await service.update([SettingChange(key: "listen", value: .text(value: "127.0.0.1:9000"))])
        #expect(service.settings.value?.restartNeeded == true)
    }

    @Test("Installing reports what the command said")
    func install() async {
        let service = ServiceStore(daemon: makeDaemon(.notRunning), control: FixtureAgent(installed: false))
        await service.loadAgent()
        #expect(service.agent.value?.installed == false)
        await service.run(.install(setEnvironment: false))
        #expect(service.lastOutput == "Installed and started the daemon.")
        #expect(!service.isWorking)
    }
}
