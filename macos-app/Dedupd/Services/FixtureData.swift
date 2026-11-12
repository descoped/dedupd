import DedupdClient
import Foundation

/// The fixed data the canned daemon answers with: one Mac with five models,
/// an attached archive and, in one scenario, a download in flight.
enum FixtureData {
    static let address = "127.0.0.1:8642"
    static let smolGGUF = "bartowski/SmolLM2-135M-Instruct-GGUF"
    static let deepSeek = "lmstudio-community/DeepSeek-V4-Flash-0731-GGUF"
    static let qwen = "lmstudio-community/Qwen3.8-27B-MLX-4bit"
    static let nemotron = "mlx-community/Nemotron-3-Super-120B-A12B-MLX-6bit"
    static let smolMLX = "mlx-community/SmolLM-135M-Instruct-4bit"
    static let archiveFolder = "/Volumes/Trunk/dedupd"

    // MARK: - Status

    static func status(jobs: [Job] = []) -> DaemonStatus {
        let active = jobs.filter { $0.phase.isActive }
        return DaemonStatus(
            version: "0.1.0",
            home: "/Users/example/.dedupd",
            endpoint: "https://huggingface.co",
            archivePath: archiveFolder,
            archiveAttached: true,
            storeFiles: 61,
            storeBytes: 290_040_000_000,
            sharedBytes: 290_070_000_000,
            downloadingTotal: active.map(\.bytesTotal).reduce(0, +),
            downloadingDone: active.map(\.bytesDone).reduce(0, +),
            apps: [
                AppUsage(app: "bionic", files: 61, bytes: 290_070_000_000),
                AppUsage(app: "hf", files: 8, bytes: 79_220_000),
                AppUsage(app: "ollama", files: 4, bytes: 105_460_000),
            ],
            repositories: [],
            daemon: DaemonInfo(
                pid: 4242,
                startedAt: 1_794_000_000,
                listen: address,
                executable: "/Users/example/.dedupd/bin/dedupd",
                managedBy: "io.descoped.dedupd.daemon",
                token: true,
                tokenSource: "/Users/example/.cache/huggingface/token",
                maxConnections: 8,
                speedLimit: 0,
                checking: false,
                lastCheckAt: 1_793_900_000
            )
        )
    }

    // MARK: - Library

    static func meta(
        _ repo: String,
        formats: [String],
        parameters: UInt64,
        license: String = "apache-2.0",
        architecture: String? = nil,
        context: UInt64? = nil,
        downloads: UInt64 = 0,
        likes: UInt64 = 0,
        base: String? = nil
    ) -> ModelMeta {
        ModelMeta(
            author: repo.split(separator: "/").first.map(String.init),
            pipelineTag: "text-generation",
            libraryName: formats.contains("mlx") ? "mlx" : nil,
            license: license,
            baseModel: base,
            parameters: parameters,
            architecture: architecture,
            contextLength: context,
            gated: false,
            private: false,
            formats: formats,
            languages: ["en"],
            tags: formats + ["text-generation", "conversational", "license:\(license)"],
            downloads: downloads,
            likes: likes,
            lastModified: "2026-10-28T09:12:44.000Z",
            createdAt: "2026-07-31T16:02:10.000Z",
            usedStorage: nil
        )
    }

    static func commit(_ short: String) -> String {
        short + String(repeating: "0", count: 40 - short.count)
    }

    private static func presence(_ app: String, files: UInt64, bytes: UInt64) -> AppPresence {
        AppPresence(app: app, files: files, bytes: bytes, sharedBytes: bytes)
    }

    static let models: [LibraryModel] = [
        LibraryModel(
            provider: "hf",
            repo: smolGGUF,
            commit: commit("09816acd5d99"),
            meta: meta(
                smolGGUF, formats: ["gguf"], parameters: 134_515_008, architecture: "llama", context: 8192,
                downloads: 104_300, likes: 61, base: "HuggingFaceTB/SmolLM2-135M-Instruct"
            ),
            revisions: 1,
            fetchedAt: 1_793_980_000,
            storedFiles: 4,
            storedBytes: 105_460_000,
            apps: [presence("bionic", files: 4, bytes: 105_460_000), presence("ollama", files: 4, bytes: 105_460_000)],
            verified: 8,
            mismatch: 0,
            notUpstream: 0,
            unchecked: 0,
            upstream: "ok",
            checkedAt: 1_793_980_000,
            ollamaTags: ["Q4_K_M"],
            archivedAt: nil
        ),
        LibraryModel(
            provider: "hf",
            repo: deepSeek,
            commit: commit("e5d9d9689670"),
            meta: meta(
                deepSeek, formats: ["gguf"], parameters: 284_000_000_000, license: "mit", architecture: "deepseek2",
                context: 163_840, downloads: 48_200, likes: 310, base: "deepseek-ai/DeepSeek-V4-Flash-0731"
            ),
            revisions: 1,
            fetchedAt: 1_792_300_000,
            storedFiles: 4,
            storedBytes: 156_380_000_000,
            apps: [presence("bionic", files: 4, bytes: 156_380_000_000)],
            verified: 4,
            mismatch: 0,
            notUpstream: 0,
            unchecked: 0,
            upstream: "ok",
            checkedAt: 1_793_900_000,
            ollamaTags: [],
            archivedAt: nil
        ),
        LibraryModel(
            provider: "hf",
            repo: qwen,
            commit: commit("6067b15cf581"),
            meta: meta(qwen, formats: ["mlx", "safetensors"], parameters: 27_400_000_000, downloads: 12_900, likes: 88),
            revisions: 2,
            fetchedAt: 1_791_100_000,
            storedFiles: 13,
            storedBytes: 16_080_000_000,
            apps: [presence("bionic", files: 13, bytes: 16_080_000_000)],
            verified: 13,
            mismatch: 0,
            notUpstream: 0,
            unchecked: 0,
            upstream: "ok",
            checkedAt: 1_793_900_000,
            ollamaTags: [],
            archivedAt: nil
        ),
        LibraryModel(
            provider: "hf",
            repo: nemotron,
            commit: commit("2405499bc926"),
            meta: meta(
                nemotron, formats: ["mlx", "safetensors"], parameters: 120_000_000_000, license: "other",
                downloads: 3400, likes: 41
            ),
            revisions: 1,
            fetchedAt: 1_790_400_000,
            storedFiles: 30,
            storedBytes: 98_160_000_000,
            apps: [presence("bionic", files: 30, bytes: 98_160_000_000)],
            verified: 28,
            mismatch: 0,
            notUpstream: 0,
            unchecked: 2,
            upstream: "gone",
            checkedAt: 1_793_900_000,
            ollamaTags: [],
            archivedAt: nil
        ),
        LibraryModel(
            provider: "hf",
            repo: smolMLX,
            commit: commit("642e06afe3fa"),
            meta: meta(smolMLX, formats: ["mlx", "safetensors"], parameters: 134_515_008, downloads: 9100, likes: 12),
            revisions: 1,
            fetchedAt: 1_789_000_000,
            storedFiles: 8,
            storedBytes: 79_220_000,
            apps: [],
            verified: 0,
            mismatch: 0,
            notUpstream: 0,
            unchecked: 0,
            upstream: "ok",
            checkedAt: 1_793_900_000,
            ollamaTags: [],
            archivedAt: nil
        ),
    ]
}

/// One model in full, and the canned daemon's storage and settings.
extension FixtureData {
    static let readme = """
    ---
    license: apache-2.0
    pipeline_tag: text-generation
    ---
    # SmolLM2 135M Instruct

    A small model for **on-device** use. This repository holds the GGUF files.

    ## How to use

    ```sh
    ollama run hf.co/bartowski/SmolLM2-135M-Instruct-GGUF:Q4_K_M
    ```

    The Q4_K_M file is a good first choice; Q8_0 is closest to the original.
    """

    private static let ggufFiles: [(String, UInt64)] = [
        ("SmolLM2-135M-Instruct-Q4_K_M.gguf", 105_450_000),
        ("README.md", 6120),
        ("config.json", 720),
        ("LICENSE", 3160),
    ]

    static func detail(_ repo: String) -> ModelDetail? {
        guard let model = models.first(where: { $0.repo == repo }) else {
            return nil
        }
        let isGGUF = model.meta.formats.contains("gguf")
        let files: [RevisionFile] = isGGUF
            ? ggufFiles.map { name, size in
                RevisionFile(
                    path: repo == smolGGUF ? name : name.replacingOccurrences(of: "SmolLM2-135M-Instruct", with: model.name),
                    size: repo == smolGGUF ? size : (name.hasSuffix(".gguf") ? model.storedBytes : size),
                    sha256: sha(name),
                    gitOid: String(sha(name).prefix(40)),
                    lfs: name.hasSuffix(".gguf"),
                    stored: true,
                    archived: true,
                    apps: model.appNames
                )
            }
            : ["config.json", "model.safetensors", "tokenizer.json", "tokenizer_config.json"].map { name in
                RevisionFile(
                    path: name,
                    size: name == "model.safetensors" ? model.storedBytes : 4200,
                    sha256: sha(name),
                    gitOid: String(sha(name).prefix(40)),
                    lfs: name == "model.safetensors",
                    stored: true,
                    archived: true,
                    apps: model.appNames
                )
            }
        let placements = model.apps.flatMap { app in
            files.map { file in
                Placement(
                    app: app.app,
                    path: "\(folder(of: app.app))/\(repo)/\(file.path)",
                    relPath: file.path,
                    size: file.size,
                    sha256: file.sha256 ?? "",
                    shared: true,
                    verifyState: "verified",
                    origin: app.app == "bionic" ? "adopted" : "placed"
                )
            }
        }
        return ModelDetail(
            model: model,
            revisions: [
                Revision(
                    commit: model.commit ?? "",
                    fetchedAt: model.fetchedAt ?? 0,
                    refs: ["main"],
                    bytes: model.storedBytes,
                    files: files
                ),
            ],
            placements: placements,
            variants: variants(of: model),
            infoJson: #"{"id":"\#(repo)","pipeline_tag":"text-generation","private":false,"gated":false}"#,
            readme: readme
        )
    }

    private static func variants(of model: LibraryModel) -> [Variant] {
        if model.meta.formats.contains("gguf") {
            return [
                Variant(
                    kind: "gguf",
                    name: "Q4_K_M",
                    spec: "\(model.repo):Q4_K_M",
                    files: 4,
                    bytes: model.storedBytes,
                    storedBytes: model.storedBytes,
                    usableBy: ["bionic", "ollama", "hf"],
                    placedIn: model.appNames,
                    decisions: decisions(usableBy: ["bionic", "ollama", "hf"])
                ),
            ]
        }
        return [
            Variant(
                kind: "files",
                name: "all files",
                spec: model.repo,
                files: model.storedFiles,
                bytes: model.storedBytes,
                storedBytes: model.storedBytes,
                usableBy: ["bionic", "hf"],
                placedIn: model.appNames,
                decisions: decisions(usableBy: ["bionic", "hf"], mlx: model.meta.formats.contains("mlx"))
            ),
        ]
    }

    /// What the daemon's app profiles answer for a variant these apps can
    /// be given, in short.
    static func decisions(usableBy: [String], mlx: Bool = false) -> [Decision] {
        ModelApp.allCases.map(\.rawValue).map { app in
            var decision = Decision(
                app: app, state: "ready", method: "clone", cost: "none", reason: "Shares the stored files",
                version: nil, versionState: "unknown"
            )
            if usableBy.contains(app) {
                return decision
            }
            if app == ModelApp.ollama.rawValue, mlx {
                decision.state = "planned"
                decision.method = "clone-with-rewritten-index"
                decision.cost = "megabytes"
                decision.reason = "Ollama can share this model with the store, but dedupd does not do that yet"
                return decision
            }
            decision.state = "no"
            decision.method = nil
            decision.cost = nil
            decision.reason = "\(ModelApp.title(for: app)) cannot run this format"
            return decision
        }
    }

    static func folder(of app: String) -> String {
        switch app {
        case "bionic": "/Users/example/.lmstudio/models"
        case "ollama": "/Users/example/.ollama/models"
        default: "/Users/example/.cache/huggingface/hub"
        }
    }

    /// A made-up digest that is the same for the same name.
    static func sha(_ name: String) -> String {
        let seed = name.utf8.reduce(UInt64(1469598103934665603)) { ($0 ^ UInt64($1)) &* 1099511628211 }
        return String(repeating: String(seed, radix: 16), count: 4).prefix(64).padding(toLength: 64, withPad: "0", startingAt: 0)
    }

    // MARK: - Storage

    static let locations = Locations(
        volumeTotal: 994_660_000_000,
        volumeFree: 412_300_000_000,
        store: Location(
            id: "store", path: "/Users/example/.dedupd/store", present: true, files: 61,
            bytes: 290_040_000_000, sharedBytes: 290_040_000_000, models: 5
        ),
        archive: Location(
            id: "archive", path: archiveFolder, present: true, files: 61,
            bytes: 290_040_000_000, sharedBytes: 0, models: 5
        ),
        apps: [
            Location(
                id: "bionic", path: "/Users/example/.lmstudio/models", present: true, files: 51,
                bytes: 270_725_460_000, sharedBytes: 270_725_460_000, models: 4
            ),
            Location(
                id: "ollama", path: "/Users/example/.ollama/models", present: true, files: 4,
                bytes: 105_460_000, sharedBytes: 105_460_000, models: 1
            ),
            Location(
                id: "hf", path: "/Users/example/.cache/huggingface/hub", present: true, files: 0,
                bytes: 0, sharedBytes: 0, models: 0
            ),
        ]
    )

    static func gcPlan(offload: Bool, freed: UInt64? = nil) -> GcPlan {
        let orphans = [
            StoredFile(sha256: sha("orphan-one"), size: 4_920_000_000, gitOid: nil),
            StoredFile(sha256: sha("orphan-two"), size: 1840, gitOid: String(sha("orphan-two").prefix(40))),
        ]
        let away = offload ? [StoredFile(sha256: sha("model.safetensors"), size: 79_210_000, gitOid: nil)] : []
        return GcPlan(
            orphans: orphans,
            offload: away,
            orphanBytes: orphans.map(\.size).reduce(0, +),
            offloadBytes: away.map(\.size).reduce(0, +),
            freed: freed
        )
    }

    static let storeCheck = StoreCheck(
        running: false, startedAt: 1_793_899_000, finishedAt: 1_793_900_000, checked: 61,
        bytes: 290_040_000_000, damaged: [], missing: [], onlyInArchive: 0, error: nil
    )

    // MARK: - Daemon

    static let settings = DaemonSettings(
        path: "/Users/example/.dedupd/config.toml",
        listen: address,
        hfEndpoint: "https://huggingface.co",
        archive: "/Volumes/Trunk/dedupd",
        bionicModels: "/Users/example/.lmstudio/models",
        ollamaModels: "/Users/example/.ollama/models",
        hfCache: "/Users/example/.cache/huggingface/hub",
        reclaim: true,
        setEnv: false,
        verifyDays: 30,
        scanInterval: 300,
        maxConnections: 8,
        speedLimit: 0,
        restartNeeded: false
    )

    static let log = LogTail(
        path: "/Users/example/.dedupd/daemon.log",
        lines: [
            "2026-11-11T08:00:01Z dedupd 0.1.0 listening on 127.0.0.1:8642 and localhost:8642",
            "2026-11-11T08:00:01Z watching 3 app folders",
            "2026-11-11T08:05:02Z pass: nothing new",
            "2026-11-11T09:14:40Z download 7: bartowski/SmolLM2-135M-Instruct-GGUF:Q8_0 queued",
        ]
    )
}
