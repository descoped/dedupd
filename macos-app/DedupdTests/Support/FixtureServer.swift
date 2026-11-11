import Foundation
import Network

/// A small HTTP server on the loopback interface that answers each route with
/// a fixed body, and remembers what it was sent. It stands in for the daemon
/// so the tests can run the real Rust client against recorded answers.
final class FixtureServer: @unchecked Sendable {
    struct Answer: Sendable {
        var status = 200
        var body: Data
    }

    struct Request: Sendable, Equatable {
        var method: String
        var path: String
        var body: String
    }

    private let listener: NWListener
    private let routes: [String: Answer]
    private let queue = DispatchQueue(label: "io.descoped.dedupd.tests.fixture-server")
    private let lock = NSLock()
    private var received: [Request] = []

    /// `host:port`, the form the client takes.
    private(set) var address = ""

    /// The requests seen so far, oldest first.
    var requests: [Request] {
        lock.withLock { received }
    }

    /// `routes` maps "GET /path" to its answer. Anything else gets a 404.
    init(routes: [String: Answer]) throws {
        self.routes = routes
        let parameters = NWParameters.tcp
        parameters.requiredLocalEndpoint = .hostPort(host: "127.0.0.1", port: .any)
        listener = try NWListener(using: parameters)
    }

    /// Start listening and learn the port the system chose.
    func start() async throws {
        listener.newConnectionHandler = { [weak self] connection in
            self?.serve(connection)
        }
        let port: UInt16 = try await withCheckedThrowingContinuation { continuation in
            let resumed = NSLock()
            nonisolated(unsafe) var done = false
            listener.stateUpdateHandler = { [listener] state in
                let first = resumed.withLock {
                    if done {
                        return false
                    }
                    switch state {
                    case .ready, .failed, .cancelled:
                        done = true
                        return true
                    default:
                        return false
                    }
                }
                guard first else {
                    return
                }
                switch state {
                case .ready:
                    continuation.resume(returning: listener.port?.rawValue ?? 0)
                case let .failed(error):
                    continuation.resume(throwing: error)
                default:
                    continuation.resume(throwing: CancellationError())
                }
            }
            listener.start(queue: queue)
        }
        address = "127.0.0.1:\(port)"
    }

    func stop() {
        listener.cancel()
    }

    private func serve(_ connection: NWConnection) {
        connection.start(queue: queue)
        read(connection, buffer: Data())
    }

    /// Read until the whole request has arrived, then answer and close.
    private func read(_ connection: NWConnection, buffer: Data) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, isComplete, error in
            guard let self else {
                return
            }
            var buffer = buffer
            if let data {
                buffer.append(data)
            }
            if let request = Self.parse(buffer) {
                self.answer(connection, request)
            } else if isComplete || error != nil {
                connection.cancel()
            } else {
                self.read(connection, buffer: buffer)
            }
        }
    }

    /// The request in `buffer`, once its headers and its whole body are there.
    private static func parse(_ buffer: Data) -> Request? {
        guard let headerEnd = buffer.range(of: Data("\r\n\r\n".utf8)) else {
            return nil
        }
        let head = String(bytes: buffer[..<headerEnd.lowerBound], encoding: .utf8) ?? ""
        let lines = head.components(separatedBy: "\r\n")
        let parts = lines.first?.split(separator: " ") ?? []
        guard parts.count >= 2 else {
            return nil
        }
        let length = lines
            .first { $0.lowercased().hasPrefix("content-length:") }
            .flatMap { Int($0.split(separator: ":")[1].trimmingCharacters(in: .whitespaces)) } ?? 0
        let body = buffer[headerEnd.upperBound...]
        guard body.count >= length else {
            return nil
        }
        return Request(method: String(parts[0]), path: String(parts[1]), body: String(bytes: body, encoding: .utf8) ?? "")
    }

    private func answer(_ connection: NWConnection, _ request: Request) {
        lock.withLock { received.append(request) }
        // A route may leave out the query, to answer whatever is asked of
        // the path.
        let plain = request.path.split(separator: "?", maxSplits: 1).first.map(String.init) ?? request.path
        let answer = routes["\(request.method) \(request.path)"]
            ?? routes["\(request.method) \(plain)"]
            ?? Answer(status: 404, body: Data(#"{"error":"no such route"}"#.utf8))
        let head = "HTTP/1.1 \(answer.status) Fixture\r\n"
            + "Content-Type: application/json\r\n"
            + "Content-Length: \(answer.body.count)\r\n"
            + "Connection: close\r\n\r\n"
        connection.send(content: Data(head.utf8) + answer.body, completion: .contentProcessed { _ in
            connection.cancel()
        })
    }
}
