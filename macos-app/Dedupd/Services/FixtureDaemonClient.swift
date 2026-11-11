import DedupdClient
import Foundation

/// The canned daemons a UI test can ask for with `-uiTestFixture <name>`.
enum FixtureScenario: String, CaseIterable, Sendable {
    /// A daemon with four models, an attached archive and nothing in flight.
    case running
    /// The same daemon in the middle of a download.
    case downloading
    /// No daemon answers.
    case notRunning
}

/// A daemon that answers from fixed data, for UI tests and previews. It never
/// touches the network.
struct FixtureDaemonClient: DaemonClient {
    let scenario: FixtureScenario

    func status(at address: String) async throws -> DaemonStatus {
        switch scenario {
        case .running:
            return Self.status()
        case .downloading:
            return Self.status(downloadingTotal: 106_070_000, downloadingDone: 41_940_000)
        case .notRunning:
            throw ClientError.NotRunning(address: address)
        }
    }

    func pull(_ request: PullRequest, at address: String) async throws -> PullResult {
        if scenario == .notRunning {
            throw ClientError.NotRunning(address: address)
        }
        return PullResult(
            repo: request.model,
            commit: "09816acd5d99c8a1c1b2f6d0d0a1b2c3d4e5f607",
            fromArchive: false,
            files: 4,
            bytes: 106_070_000,
            downloaded: 4,
            placed: request.apps.map { PlacedApp(app: $0, cloned: 4, present: 0, conflicts: []) },
            notes: []
        )
    }

    func scan(at address: String) async throws -> ScanResult {
        if scenario == .notRunning {
            throw ClientError.NotRunning(address: address)
        }
        return ScanResult(
            forgotten: 0,
            adopted: 2,
            adoptedBytes: 4_920_000_000,
            reclaimed: 2,
            reclaimedBytes: 4_920_000_000,
            archived: 2,
            errors: []
        )
    }

    /// The status of a daemon that holds four models.
    static func status(downloadingTotal: UInt64 = 0, downloadingDone: UInt64 = 0) -> DaemonStatus {
        DaemonStatus(
            version: "0.1.0",
            home: "/Users/example/.dedupd",
            endpoint: "https://huggingface.co",
            archivePath: "/Volumes/Trunk/dedupd",
            archiveAttached: true,
            storeFiles: 61,
            storeBytes: 290_040_000_000,
            sharedBytes: 290_070_000_000,
            downloadingTotal: downloadingTotal,
            downloadingDone: downloadingDone,
            apps: [
                AppUsage(app: "bionic", files: 61, bytes: 290_070_000_000),
                AppUsage(app: "hf", files: 8, bytes: 79_220_000),
                AppUsage(app: "ollama", files: 4, bytes: 105_460_000),
            ],
            repositories: [
                repository("lmstudio-community/DeepSeek-V4-Flash-0731-GGUF", bytes: 156_380_000_000, files: 4, commit: "e5d9d9689670"),
                repository("lmstudio-community/Qwen3.8-27B-MLX-4bit", bytes: 16_080_000_000, files: 13, commit: "6067b15cf581"),
                repository(
                    "mlx-community/Nemotron-3-Super-120B-A12B-MLX-6bit",
                    bytes: 98_160_000_000,
                    files: 30,
                    commit: "2405499bc926",
                    unchecked: 2
                ),
                repository("mlx-community/SmolLM-135M-Instruct-4bit", bytes: 79_220_000, files: 0, stored: 8, commit: "642e06afe3fa"),
            ]
        )
    }

    private static func repository(
        _ name: String,
        bytes: UInt64,
        files: UInt64,
        stored: UInt64? = nil,
        commit: String,
        unchecked: UInt64 = 0
    ) -> Repository {
        Repository(
            provider: "huggingface",
            name: name,
            commit: commit + String(repeating: "0", count: 28),
            storedFiles: stored ?? files,
            storedBytes: bytes,
            files: files,
            bytes: files == 0 ? 0 : bytes,
            verified: files - unchecked,
            mismatch: 0,
            notUpstream: 0,
            unchecked: unchecked
        )
    }
}
