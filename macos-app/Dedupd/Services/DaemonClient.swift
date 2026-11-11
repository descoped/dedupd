import DedupdClient
import Foundation

/// The daemon's control API as the app uses it. The live implementation calls
/// the Rust client; tests and UI tests use their own.
protocol DaemonClient: Sendable {
    /// What the daemon holds and what it is doing.
    func status(at address: String) async throws -> DaemonStatus
    /// Fetch a model and place it. Returns when the daemon has finished.
    func pull(_ request: PullRequest, at address: String) async throws -> PullResult
    /// Have the daemon look at the app folders now.
    func scan(at address: String) async throws -> ScanResult
}

/// Calls the daemon through the Rust client linked into the app.
struct LiveDaemonClient: DaemonClient {
    func status(at address: String) async throws -> DaemonStatus {
        try await Self.blocking { try DedupdClient.status(address: address) }
    }

    func pull(_ request: PullRequest, at address: String) async throws -> PullResult {
        try await Self.blocking {
            try DedupdClient.pull(
                address: address,
                model: request.model,
                apps: request.apps,
                include: request.include,
                offline: request.offline
            )
        }
    }

    func scan(at address: String) async throws -> ScanResult {
        try await Self.blocking { try DedupdClient.scan(address: address) }
    }

    /// The Rust calls block until the daemon answers, which for a pull is as
    /// long as the download. They run on a dispatch queue so they never hold
    /// one of the few threads Swift concurrency shares between all tasks.
    private static func blocking<T: Sendable>(_ work: @escaping @Sendable () throws -> T) async throws -> T {
        try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                continuation.resume(with: Result { try work() })
            }
        }
    }
}

extension ClientError {
    /// The message to show a person. The generated description is the Swift
    /// spelling of the case, which is of no use to them.
    var message: String {
        switch self {
        case let .NotRunning(address):
            "The dedupd daemon is not running at \(address)."
        case let .Failed(message):
            message
        case let .Protocol(message):
            "Unexpected answer from the daemon: \(message)"
        }
    }
}

/// The message for any error a client call can throw.
func describe(_ error: any Error) -> String {
    (error as? ClientError)?.message ?? error.localizedDescription
}
