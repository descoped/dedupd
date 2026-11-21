import DedupdClient
import Foundation
import Testing

@testable import Dedupd

@Suite("Stores: one on each disk that holds app folders")
@MainActor
struct StoreListTests {
    @Test("Stores are added, renamed and removed, and take effect at once; the home's store stays")
    func storeList() async throws {
        let daemon = await startedDaemon()
        let storage = StorageStore(daemon: daemon)
        await storage.reload()
        #expect(storage.stores.map(\.name) == ["Internal", "Studio"])
        #expect(storage.locations.value?.stores.map(\.name) == ["Studio"])

        // A disk's top folder gets a dedupd folder, named after the disk.
        #expect(await storage.addStore(folder: "/Volumes/Envoy", name: "") == nil)
        let envoy = try #require(storage.stores.last)
        #expect((envoy.name, envoy.path, envoy.primary) == ("Envoy", "/Volumes/Envoy/dedupd", false))

        // The home's name, a name twice and a folder twice are refused.
        #expect(await storage.addStore(folder: "/Volumes/Other", name: "internal")
            == "two stores are named internal; give each its own name")
        #expect(await storage.addStore(folder: "/Volumes/Envoy/dedupd", name: "Again")
            == "/Volumes/Envoy/dedupd is listed twice")
        #expect(storage.stores.count == 3)
        // A store and an archive never share a folder.
        #expect(await storage.addStore(folder: "/Volumes/Drawer", name: "")?
            .hasSuffix("a store and an archive cannot share a folder") == true)
        #expect(await storage.addArchive(folder: "/Volumes/Studio", name: "", backup: false)?
            .hasSuffix("a store and an archive cannot share a folder") == true)

        #expect(await storage.rename(envoy, to: "Fast SSD") == nil)
        #expect(storage.stores.last?.name == "Fast SSD")
        let renamed = try #require(storage.stores.last)
        #expect(await storage.remove(renamed) == nil)
        #expect(storage.stores.map(\.name) == ["Internal", "Studio"])
    }

    @Test("A store says which apps it serves, and is offered for Restore with its free space")
    func storeWords() {
        let stores = FixtureData.stores
        #expect(stores[0].note.hasPrefix("In dedupd's home. Serves"))
        #expect(stores[1].note == "Serves no app yet: no app keeps its models on this disk.")
        #expect(StoreWords.restoreChoice(stores[1]) == "Studio (\(ByteFormat.string(stores[1].free)) free)")
    }

    @Test("An archived model is restored into the store the owner picks")
    func restoreInto() async throws {
        let client = FixtureDaemonClient(scenario: .running)
        let first = await client.models.first?.repo
        let repo = try #require(first)
        try await client.archive(repo, at: FixtureData.address)
        try await client.restore(repo, into: "Studio", at: FixtureData.address)
        let moved = await client.moving?.archive
        #expect(moved == "Studio")
        try await client.archive(repo, at: FixtureData.address)
        await #expect(throws: ClientError.self) {
            try await client.restore(repo, into: "Nowhere", at: FixtureData.address)
        }
    }

    /// A GGUF variant of 10 bytes, stored, with the recorded answers of the
    /// profiles.
    private static func variant(copies: [StoreCopy]) -> Variant {
        let judged = FixtureData.Judged(format: .gguf, decisions: RecordedProfiles.gguf)
        return Variant(
            kind: .gguf, format: .gguf, name: "v", spec: "a/b", files: 1, bytes: 10, storedBytes: 10,
            weightsStored: true, usableBy: judged.usableBy, placedIn: [], decisions: RecordedProfiles.gguf,
            copies: copies
        )
    }

    @Test("An app on a disk with no store is not offered the model; one whose store lacks it is told the copy")
    func storesOnOtherDisks() {
        let copies = [
            StoreCopy(app: .bionic, store: "Studio", files: 1, bytes: 10),
            StoreCopy(app: .huggingFace, store: "Internal", files: 0, bytes: 0),
            StoreCopy(app: .ollama, store: nil, files: 0, bytes: 0),
        ]
        let variant = Self.variant(copies: copies)
        #expect([variant].activatable(in: .ollama).isEmpty)
        #expect([variant].note(for: .ollama) == "No store on the disk of its folder · add one under Storage")
        #expect([variant].activatable(in: .bionic).count == 1)
        #expect(variant.copyBytes(for: .bionic) == 10)
        #expect(variant.copyQuestion(for: .bionic).contains("into the store Studio"))
        #expect(variant.placeHelp(for: .bionic).hasSuffix("is copied into the store Studio, on the disk of its folder."))
        #expect(variant.copyBytes(for: .huggingFace) == 0)
        #expect(!variant.placeHelp(for: .huggingFace).contains("copied"))
        // A daemon that does not say which store serves an app counts as one.
        #expect(Self.variant(copies: []).hasStore(for: .ollama))
    }
}
