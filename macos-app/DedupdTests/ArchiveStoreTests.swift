import DedupdClient
import Foundation
import Testing

@testable import Dedupd

@Suite("Archives: several cold locations, chosen per model")
@MainActor
struct ArchiveStoreTests {
    @Test("Archives are added, renamed, made the backup and removed, and take effect at once")
    func archiveList() async throws {
        let daemon = await startedDaemon()
        let storage = StorageStore(daemon: daemon)
        await storage.reload()
        #expect(storage.archives.map(\.name) == ["Archive", "Drawer"])
        #expect(storage.backupArchive?.name == "Archive")

        // A disk's top folder gets a dedupd folder, named after the disk.
        #expect(await storage.addArchive(folder: "/Volumes/Envoy", name: "", backup: false) == nil)
        let envoy = try #require(storage.archives.last)
        #expect((envoy.name, envoy.path, envoy.backup) == ("Envoy", "/Volumes/Envoy/dedupd", false))
        #expect(storage.locations.value?.archives.map(\.name) == ["Archive", "Drawer", "Envoy"])

        // One name twice is refused, and nothing changes.
        let refused = await storage.addArchive(folder: "/Volumes/Other", name: "drawer", backup: false)
        #expect(refused == "two archives are named drawer; give each its own name")
        #expect(storage.archives.count == 3)

        #expect(await storage.rename(envoy, to: "Fast SSD") == nil)
        #expect(storage.archives.last?.name == "Fast SSD")

        // The backup moves: the daemon lists the backup archive first.
        let fast = try #require(storage.archives.last)
        #expect(await storage.useAsBackup(fast) == nil)
        #expect(storage.archives.map(\.name) == ["Fast SSD", "Archive", "Drawer"])
        #expect(storage.archives.filter(\.backup).map(\.name) == ["Fast SSD"])

        // Removing the backup leaves no backup; the others stay.
        #expect(await storage.remove(fast) == nil)
        #expect(storage.archives.map(\.name) == ["Archive", "Drawer"])
        #expect(storage.backupArchive == nil)
        #expect(daemon.status?.archivePath == nil)
    }

    @Test("A model is archived to the archive the owner picks, and its detail says where its copies are")
    func archiveToChosen() async throws {
        let daemon = await startedDaemon()
        let library = LibraryStore(daemon: daemon)
        await library.reload()
        library.selection = try #require(library.models.first { $0.repo == FixtureData.qwen }).id
        #expect(library.archiveChoices.map(\.name) == ["Archive", "Drawer"])

        await library.planArchive()
        #expect(library.archiving?.archiveName == "Archive", "the backup archive unless the owner picks another")
        await library.planArchive(to: "Drawer")
        let plan = try #require(library.archiving)
        #expect((plan.archiveName, plan.archive) == ("Drawer", "/Volumes/Drawer/dedupd"))

        await library.applyArchive()
        #expect(await eventually { await daemon.moving?.archive == "Drawer" })
        let history = try await daemon.client.activity(at: daemon.address)
        #expect(history.first?.text.hasPrefix("Archived to Drawer: ") == true)
        await library.reload()
        await library.loadDetail()
        #expect(library.detail.value?.archives == ["Archive", "Drawer"], "the backup has it too")
        #expect(daemon.problem == nil)
    }
}
