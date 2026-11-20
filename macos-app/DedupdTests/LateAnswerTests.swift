import DedupdClient
import Foundation
import Testing

@testable import Dedupd

/// The canned daemon can hold a call back until the test lets it go, so an
/// answer can be made to come back late, or after a newer one.
@Suite("Answers that come back late or from another daemon")
@MainActor
struct LateAnswerTests {
    private static let models = "models(at:)"

    /// A store on a running canned daemon, with the client to hold calls on.
    private func running() async -> (FixtureDaemonClient, DaemonStore) {
        let client = FixtureDaemonClient(scenario: .running)
        let daemon = DaemonStore(client: client, settings: makeSettings())
        daemon.start()
        _ = await eventually { await daemon.connection == .running }
        return (client, daemon)
    }

    private func waits(_ client: FixtureDaemonClient, _ call: String, _ count: Int) async -> Bool {
        await eventually { await client.waiting(call) == count }
    }

    @Test("The answer to an older reload of the library does not replace a newer one")
    func reloadsOutOfOrder() async {
        let (client, daemon) = await running()
        let library = LibraryStore(daemon: daemon)
        await client.hold(Self.models)
        let first = Task { await library.reload() }
        #expect(await waits(client, Self.models, 1))
        let second = Task { await library.reload() }
        #expect(await waits(client, Self.models, 2))

        await client.release(Self.models, last: true)
        await second.value
        #expect(library.models.count == 5)
        await client.release(Self.models, failing: "too late")
        await first.value
        #expect(library.failure == nil, "the older answer was dropped")
        #expect(library.models.count == 5)
    }

    @Test("A model's details that arrive after another model was selected are not shown")
    func detailOfAnEarlierSelection() async {
        let (client, daemon) = await running()
        let library = LibraryStore(daemon: daemon)
        await library.reload()
        let call = "model(_:at:)"
        await client.hold(call)
        library.select(repo: FixtureData.smolGGUF)
        let asked = Task { await library.loadDetail() }
        #expect(await waits(client, call, 1))
        library.select(repo: FixtureData.smolMLX)
        await client.release(call)
        await asked.value
        #expect(library.detail.value == nil, "the answer was for the model selected before")
    }

    @Test("A plan that arrives after another model was selected opens no sheet")
    func planOfAnEarlierSelection() async {
        let (client, daemon) = await running()
        let library = LibraryStore(daemon: daemon)
        await library.reload()
        let call = "unplace(_:app:paths:apply:at:)"
        await client.hold(call)
        library.select(repo: FixtureData.smolGGUF)
        let asked = Task { await library.planRemoval(from: .bionic) }
        #expect(await waits(client, call, 1))
        library.select(repo: FixtureData.smolMLX)
        await client.release(call)
        await asked.value
        #expect(library.removal == nil)

        // Still selected when it answers: the sheet is for this model.
        await client.releaseAll(call)
        await library.planRemoval(from: .bionic)
        #expect(library.removal?.repo == FixtureData.smolMLX)
    }

    @Test("A search that is overtaken by a newer one does not replace its hits")
    func searchOvertaken() async {
        let (client, daemon) = await running()
        let discover = DiscoverStore(daemon: daemon)
        let call = "search(_:at:)"
        await client.hold(call)
        discover.text = "smol"
        let first = Task { await discover.search() }
        #expect(await waits(client, call, 1))
        discover.text = "qwen"
        let second = Task { await discover.search() }
        #expect(await waits(client, call, 2))

        await client.release(call, last: true)
        await second.value
        let newer = discover.hits
        #expect(!newer.isEmpty && newer.allSatisfy { $0.id.lowercased().contains("qwen") })
        await client.release(call)
        await first.value
        #expect(discover.hits == newer)
        #expect(!discover.isSearching)
    }

    @Test("Turning to a daemon at another address empties every screen at once, and drops what the old one still answers")
    func anotherAddress() async {
        let (client, daemon) = await running()
        let library = LibraryStore(daemon: daemon)
        let discover = DiscoverStore(daemon: daemon)
        let storage = StorageStore(daemon: daemon)
        let activity = ActivityStore(daemon: daemon)
        let favorites = FavoritesStore(daemon: daemon)
        await library.reload()
        library.select(repo: FixtureData.smolGGUF)
        await discover.search()
        await storage.reload()
        await activity.reload()
        await favorites.reload()
        #expect(!library.models.isEmpty && !discover.hits.isEmpty && storage.plan.value != nil)
        #expect(!activity.entries.isEmpty && !favorites.favorites.isEmpty)

        // A reload of the old daemon's library is still out.
        await client.hold(Self.models)
        let old = Task { await library.reload() }
        #expect(await waits(client, Self.models, 1))
        let before = daemon.revision
        daemon.settings.customAddress = "127.0.0.1:9999"
        daemon.start()

        #expect(library.models.isEmpty && library.selection == nil && !library.hasLoaded)
        #expect(discover.hits.isEmpty && !discover.hasSearched)
        #expect(storage.locations == .idle && storage.plan == .idle && storage.check == nil)
        #expect(activity.entries.isEmpty && favorites.favorites.isEmpty)
        #expect(daemon.revision == before + 1, "the screens read again")
        #expect(daemon.moving == nil && !daemon.checking && daemon.scanActivity == .idle)

        await client.releaseAll(Self.models)
        await old.value
        #expect(library.models.isEmpty, "the old daemon's list is not shown as the new one's")
    }

    @Test("An event of the daemon followed before is not written once the app turned to another")
    func eventOfTheOldDaemon() async {
        let client = FixtureDaemonClient(scenario: .running)
        let daemon = DaemonStore(client: client, settings: makeSettings())
        let call = "status(at:)"
        await client.hold(call)
        // The first event makes the store ask for the status, which waits.
        let old = Task { await daemon.receive(.event(event([job(1, .downloading)]))) }
        #expect(await waits(client, call, 1))
        daemon.settings.customAddress = "127.0.0.1:9999"
        daemon.start()
        await client.releaseAll(call)
        await old.value
        _ = await eventually { await daemon.connection == .running }
        #expect(!daemon.jobs.contains { $0.id == 1 }, "the old daemon's queue was not written")
    }

    @Test("The history read at one address is not shown after the address changed")
    func historyOfAnotherAddress() async {
        let (client, daemon) = await running()
        let activity = ActivityStore(daemon: daemon)
        let call = "activity(at:)"
        await client.hold(call)
        let asked = Task { await activity.reload() }
        #expect(await waits(client, call, 1))
        daemon.settings.customAddress = "127.0.0.1:9999"
        await client.releaseAll(call)
        await asked.value
        #expect(activity.entries.isEmpty)
    }

    @Test("A model asked for by name before the list was read is selected when the list has it")
    func selectedOnceTheListIsRead() async {
        let (_, daemon) = await running()
        let library = LibraryStore(daemon: daemon)
        library.select(repo: FixtureData.smolMLX)
        #expect(library.selection == nil)
        await library.reload()
        #expect(library.selected?.repo == FixtureData.smolMLX)
        // A name the library does not have selects nothing.
        library.select(repo: "org/not-here")
        await library.reload()
        #expect(library.selected?.repo == FixtureData.smolMLX)
    }

    @Test("A model that is archived and restored shows as each in turn, as the events come")
    func archivedAndRestored() async {
        let (_, daemon) = await running()
        let library = LibraryStore(daemon: daemon)
        await library.reload()
        library.select(repo: FixtureData.qwen)
        var seen = daemon.revision
        // What the window does: read again when the daemon says it changed.
        func follow() async {
            let before = seen
            _ = await eventually { await daemon.revision != before }
            seen = daemon.revision
            await library.reload()
        }
        await library.planArchive()
        await library.applyArchive()
        await follow()
        #expect(daemon.problem == nil)
        #expect(library.selected?.isArchived == true)
        await library.restore()
        await follow()
        #expect(daemon.problem == nil)
        #expect(library.selected?.isArchived == false)
        #expect(!library.isMoving)
    }

    @Test("A failed reload keeps the list and says so")
    func failedReloadKeepsTheList() async {
        let (client, daemon) = await running()
        let library = LibraryStore(daemon: daemon)
        await library.reload()
        await client.hold(Self.models)
        let asked = Task { await library.reload() }
        #expect(await waits(client, Self.models, 1))
        await client.release(Self.models, failing: "the database is busy")
        await asked.value
        #expect(library.models.count == 5)
        #expect(library.failure == "the database is busy")
    }

    @Test("What this Mac has of a model in Discover's list is read again when the library changes")
    func discoverRowsFollowTheLibrary() async {
        let (client, daemon) = await running()
        let discover = DiscoverStore(daemon: daemon)
        await discover.search()
        let row = { discover.hits.first { $0.id == FixtureData.smolGGUF } }
        #expect(row()?.apps.isEmpty == false)
        // The owner archives it: no app has it any more.
        try? await client.archive(FixtureData.smolGGUF, at: daemon.address)
        await discover.refreshLocal()
        #expect(row()?.apps.isEmpty == true)
        #expect(row()?.archived == true, "dedupd still keeps it")
    }
}
