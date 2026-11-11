import DedupdClient
import Foundation
import Testing

@testable import Dedupd

/// The whole path the app depends on: the daemon's answers, as the Rust
/// integration suite recorded them (`just fixtures`), read by the Rust client
/// linked into the app and handed to Swift.
///
/// A change to the control API that the app cannot read fails here.
@Suite("The app reads what the daemon sends")
struct ContractTests {
    private final class Marker {}

    private static func recorded(_ name: String) throws -> Data {
        let bundle = Bundle(for: Marker.self)
        let url = try #require(
            bundle.url(forResource: name, withExtension: "json", subdirectory: "control"),
            "the recorded answer \(name).json is not in the test bundle"
        )
        return try Data(contentsOf: url)
    }

    private static func daemon(_ routes: [String: FixtureServer.Answer]) async throws -> FixtureServer {
        let server = try FixtureServer(routes: routes)
        try await server.start()
        return server
    }

    @Test("Status")
    func status() async throws {
        let server = try await Self.daemon(["GET /dedupd/v1/status": .init(body: Self.recorded("status"))])
        defer { server.stop() }

        let status = try await LiveDaemonClient().status(at: server.address)

        #expect(status.version == "0.0.0")
        #expect(status.home == "/Users/example/home")
        #expect(status.endpoint == "https://huggingface.co")
        #expect(status.archivePath == nil)
        #expect(!status.archiveAttached)
        #expect(status.storeFiles == 2)
        #expect(status.storeBytes == 300_008)
        #expect(status.sharedBytes == 300_008)
        #expect(status.downloadingTotal == 0)
        #expect(status.apps == [AppUsage(app: "bionic", files: 2, bytes: 300_008)])
        let repo = try #require(status.repositories.first)
        #expect(repo.name == "org/Repo-GGUF")
        #expect(repo.provider == "hf")
        #expect(repo.commit == "0123456789abcdef0123456789abcdef01234567")
        #expect(repo.storedFiles == 2)
        #expect(repo.files == 2)
        #expect(repo.verified == 2)
        #expect(repo.health == .verified)
    }

    @Test("Pull: the request the daemon receives and the outcome the app gets")
    func pull() async throws {
        let server = try await Self.daemon(["POST /dedupd/v1/pull": .init(body: Self.recorded("pull"))])
        defer { server.stop() }

        let request = PullRequest(model: "org/Repo-GGUF", apps: ["bionic"], include: ["*.gguf"], offline: true)
        let result = try await LiveDaemonClient().pull(request, at: server.address)

        #expect(result.repo == "org/Repo-GGUF")
        #expect(result.files == 2)
        #expect(result.downloaded == 2)
        #expect(result.bytes == 300_008)
        #expect(!result.fromArchive)
        #expect(result.placed == [PlacedApp(app: "bionic", cloned: 2, present: 0, conflicts: [])])
        #expect(result.notes.isEmpty)

        let sent = try #require(server.requests.first)
        let body = try #require(try JSONSerialization.jsonObject(with: Data(sent.body.utf8)) as? [String: Any])
        #expect(body["model"] as? String == "org/Repo-GGUF")
        #expect(body["apps"] as? [String] == ["bionic"])
        #expect(body["include"] as? [String] == ["*.gguf"])
        #expect(body["offline"] as? Bool == true)
    }

    @Test("Scan")
    func scan() async throws {
        let server = try await Self.daemon(["POST /dedupd/v1/scan": .init(body: Self.recorded("scan"))])
        defer { server.stop() }

        let result = try await LiveDaemonClient().scan(at: server.address)

        #expect(result == ScanResult(
            forgotten: 0, adopted: 0, adoptedBytes: 0, reclaimed: 0, reclaimedBytes: 0, archived: 0, errors: []
        ))
    }

    @Test("A refusal carries the daemon's own words")
    func refusal() async throws {
        let server = try await Self.daemon(["POST /dedupd/v1/pull": .init(status: 400, body: Self.recorded("error"))])
        defer { server.stop() }

        await #expect(throws: ClientError.Failed(
            message: "not a model is not a model name; use org/repo, org/repo:Q4_K_M or org/repo@revision"
        )) {
            try await LiveDaemonClient().pull(PullRequest(model: "not a model", apps: []), at: server.address)
        }
    }

    @Test("An answer the client does not understand is a protocol error, not a crash")
    func garbage() async throws {
        let server = try await Self.daemon(["GET /dedupd/v1/status": .init(body: Data("{}".utf8))])
        defer { server.stop() }

        do {
            _ = try await LiveDaemonClient().status(at: server.address)
            Issue.record("an empty object was read as a status")
        } catch let ClientError.Protocol(message) {
            #expect(message.contains("missing field"))
        }
    }

    @Test("Nothing listening is reported as the daemon not running")
    func nothingListening() async throws {
        // Port 1 on the loopback interface refuses connections at once.
        await #expect(throws: ClientError.NotRunning(address: "127.0.0.1:1")) {
            try await LiveDaemonClient().status(at: "127.0.0.1:1")
        }
    }
}
