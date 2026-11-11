import DedupdClient
import Foundation
import Testing

@testable import Dedupd

/// The daemon's recorded answers (`just fixtures`), served by a loopback
/// server for the real Rust client to read.
enum Recorded {
    private final class Marker {}

    static func data(_ name: String) throws -> Data {
        let bundle = Bundle(for: Marker.self)
        let url = try #require(
            bundle.url(forResource: name, withExtension: "json", subdirectory: "control"),
            "the recorded answer \(name).json is not in the test bundle"
        )
        return try Data(contentsOf: url)
    }

    /// A daemon that answers each "METHOD /path" with the recorded answer
    /// of that name.
    static func daemon(_ routes: [String: String]) async throws -> FixtureServer {
        var answers: [String: FixtureServer.Answer] = [:]
        for (route, name) in routes {
            answers[route] = .init(body: try data(name))
        }
        let server = try FixtureServer(routes: answers)
        try await server.start()
        return server
    }
}

/// Every answer the screens depend on, read by the Rust client linked into
/// the app. A change to the control API that the app cannot read fails here.
@Suite("The app reads what the daemon sends for each screen")
struct ScreenContractTests {
    private let client = LiveDaemonClient()

    @Test("Library: the list and one model")
    func library() async throws {
        let server = try await Recorded.daemon([
            "GET /dedupd/v1/models": "models",
            "GET /dedupd/v1/models/org/Repo-GGUF": "model",
        ])
        defer { server.stop() }

        let models = try await client.models(at: server.address)
        let model = try #require(models.first)
        #expect(model.repo == "org/Repo-GGUF")
        #expect(model.meta.formats == ["gguf"])
        #expect(model.meta.parameters == 134_515_008)
        #expect(model.appNames == ["bionic", "hf"])
        #expect(model.health == .verified)

        let detail = try await client.model("org/Repo-GGUF", at: server.address)
        #expect(detail.model == model)
        #expect(detail.revisions.first?.refs == ["main"])
        #expect(detail.revisions.first?.files.map(\.path) == ["model.gguf", "sub/config.json"])
        #expect(detail.placements.count == 4)
        let variant = try #require(detail.variants.first)
        #expect(variant.kind(formats: model.meta.formats) == .gguf)
        #expect(variant.isStored)
        #expect(variant.placedIn == ["bionic", "hf"])
        #expect(detail.infoJson?.contains("\"siblings\"") == true)
    }

    @Test("Discover: a search and one model, with the query the daemon receives")
    func discover() async throws {
        let server = try await Recorded.daemon([
            "GET /dedupd/v1/hub/search": "search",
            "GET /dedupd/v1/hub/models/org/Repo-GGUF": "hub-model",
        ])
        defer { server.stop() }

        let query = SearchQuery(
            text: "smol lm", format: "gguf", task: "text-generation", license: "", author: "",
            sort: "downloads", cursor: nil
        )
        let page = try await client.search(query, at: server.address)
        #expect(page.models.map(\.id) == ["org/Repo-GGUF", "mlx-community/Repo-4bit"])
        #expect(page.models.first?.meta.license == "apache-2.0")
        #expect(page.models.first?.localState == nil)

        let asked = try #require(server.requests.first?.path)
        #expect(asked.contains("q=smol%20lm") || asked.contains("q=smol+lm"))
        #expect(asked.contains("format=gguf"))
        #expect(asked.contains("task=text-generation"))
        #expect(asked.contains("sort=downloads"))

        let model = try await client.hubModel("org/Repo-GGUF", at: server.address)
        #expect(model.id == "org/Repo-GGUF")
        #expect(model.meta.architecture == "llama")
        #expect(model.meta.contextLength == 8192)
        #expect(model.files.map(\.path) == ["model.gguf", "sub/config.json"])
        #expect(model.related.map(\.id) == ["mlx-community/Repo-4bit"])
        #expect(!model.variants.isEmpty)
        #expect(!model.archived)
    }

    @Test("Downloads: queueing, the queue, one download and an action")
    func downloads() async throws {
        let server = try await Recorded.daemon([
            "POST /dedupd/v1/downloads": "enqueue",
            "GET /dedupd/v1/downloads/2": "download",
            "POST /dedupd/v1/downloads/2/pause": "enqueue",
        ])
        defer { server.stop() }

        let request = PullRequest(model: "org/Repo-GGUF:model", apps: ["hf"], include: [], offline: true)
        let queued = try await client.enqueue(request, at: server.address)
        #expect(queued == Enqueued(id: 2, existing: false))
        let sent = try #require(server.requests.first)
        let body = try #require(try JSONSerialization.jsonObject(with: Data(sent.body.utf8)) as? [String: Any])
        #expect(body["model"] as? String == "org/Repo-GGUF:model")
        #expect(body["apps"] as? [String] == ["hf"])
        #expect(body["offline"] as? Bool == true)

        let detail = try await client.download(2, at: server.address)
        #expect(detail.job.phase == .done)
        #expect(detail.job.apps == ["hf"])
        #expect(detail.files.map(\.stage) == ["stored", "stored"])
        #expect(detail.events.first?.text == "queued")
        #expect(detail.events.last?.text == "done")

        try await client.downloadAction(.pause, id: 2, at: server.address)
        #expect(server.requests.last?.path == "/dedupd/v1/downloads/2/pause")
    }

    @Test("The event stream delivers the daemon's state as soon as it connects")
    func events() async throws {
        let line = try Recorded.data("event")
        let compact = try JSONSerialization.data(withJSONObject: JSONSerialization.jsonObject(with: line))
        let server = try FixtureServer(routes: ["GET /dedupd/v1/events": .init(body: compact + Data("\n".utf8))])
        try await server.start()
        defer { server.stop() }

        var signals: [DaemonSignal] = []
        for await signal in client.events(at: server.address) {
            signals.append(signal)
            if case .event = signal {
                break
            }
        }
        #expect(signals.first == .connected)
        guard case let .event(event) = signals.last else {
            Issue.record("no event arrived: \(signals)")
            return
        }
        #expect(event.seq == 1)
        #expect(event.downloads.jobs.isEmpty)
        #expect(event.downloads.governor.maxConnections == 8)
    }

    @Test("The event stream says so when nothing answers")
    func eventsWithoutDaemon() async {
        for await signal in client.events(at: "127.0.0.1:1") {
            #expect(signal == .disconnected)
            break
        }
    }

    @Test("Storage: the places, the clean-up plan and the store check")
    func storage() async throws {
        let server = try await Recorded.daemon([
            "GET /dedupd/v1/locations": "locations",
            "GET /dedupd/v1/gc": "gc",
            "GET /dedupd/v1/verify": "verify",
            "POST /dedupd/v1/unplace": "unplace",
            "POST /dedupd/v1/check": "check",
        ])
        defer { server.stop() }

        let locations = try await client.locations(at: server.address)
        #expect(locations.store.title == "Store")
        #expect(locations.archive == nil)
        #expect(locations.apps.map(\.id) == ["bionic", "ollama", "hf"])
        #expect(locations.apps.map(\.present) == [true, false, true])
        #expect(locations.volumeTotal == 1_000_000_000_000)

        let plan = try await client.gcPlan(offload: true, at: server.address)
        #expect(plan.orphans.isEmpty)
        #expect(plan.freed == nil)
        #expect(server.requests.last?.path == "/dedupd/v1/gc?offload=true")

        let check = try await client.storeCheck(at: server.address)
        #expect(!check.running)
        #expect(check.finishedAt == nil)

        let removal = try await client.unplace("org/Repo-GGUF", app: "bionic", apply: false, at: server.address)
        #expect(removal.remove.count == 2)
        #expect(removal.bytes == 300_008)
        #expect(removal.removed == nil)
        let sent = try #require(server.requests.last)
        let body = try #require(try JSONSerialization.jsonObject(with: Data(sent.body.utf8)) as? [String: Any])
        #expect(body["apply"] as? Bool == false)
        #expect(body["app"] as? String == "bionic")

        let upstream = try await client.checkUpstream("org/Repo-GGUF", at: server.address)
        #expect(upstream.upstream == "ok")
        #expect(!upstream.changed)
    }

    @Test("Daemon: the settings, a change to them, and the log")
    func service() async throws {
        let server = try await Recorded.daemon([
            "GET /dedupd/v1/config": "config",
            "PUT /dedupd/v1/config": "config",
            "GET /dedupd/v1/log": "log",
        ])
        defer { server.stop() }

        let settings = try await client.settings(at: server.address)
        #expect(settings.listen == "127.0.0.1:8642")
        #expect(settings.archive == nil)
        #expect(settings.verifyDays == 7)
        #expect(!settings.restartNeeded)
        #expect(SettingsDraft(settings).speedLimit == 0)

        _ = try await client.updateSettings(
            [
                SettingChange(key: "max_connections", value: .number(value: 4)),
                SettingChange(key: "archive", value: .unset),
                SettingChange(key: "reclaim", value: .flag(value: false)),
                SettingChange(key: "listen", value: .text(value: "127.0.0.1:9000")),
            ],
            at: server.address
        )
        let sent = try #require(server.requests.last)
        #expect(sent.method == "PUT")
        let body = try #require(try JSONSerialization.jsonObject(with: Data(sent.body.utf8)) as? [String: Any])
        #expect(body["max_connections"] as? Int == 4)
        #expect(body["archive"] is NSNull)
        #expect(body["reclaim"] as? Bool == false)
        #expect(body["listen"] as? String == "127.0.0.1:9000")

        let log = try await client.logTail(lines: 200, at: server.address)
        #expect(log.lines.isEmpty)
        #expect(log.path == "/Users/example/home/daemon.log")
    }
}
