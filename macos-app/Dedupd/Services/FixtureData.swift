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
    static let archiveFolder = "/Volumes/Archive/dedupd"

    // MARK: - Status

    /// The files nothing refers to, until the owner cleans up.
    static let orphanFiles: UInt64 = 2
    static let orphanBytes: UInt64 = 4_920_001_840

    /// What the internal disk's store holds of a library: every model that
    /// is not archived, and the orphans while they are there.
    static func stored(of library: [LibraryModel], cleaned: Bool, offloaded: Bool = false) -> (files: UInt64, bytes: UInt64) {
        let here = library.filter { !$0.isArchived }
        // A weights file whose local copy went in a clean-up is not counted:
        // the daemon counts what the store holds.
        let away = here.contains { weightsAway($0, offloaded: offloaded) }
        return (
            here.map(\.storedFiles).reduce(0, +) + (cleaned ? 0 : orphanFiles) - (away ? 1 : 0),
            here.map(\.storedBytes).reduce(0, +) + (cleaned ? 0 : orphanBytes) - (away ? offloadBytes : 0)
        )
    }

    /// The one file a clean-up can move off this Mac: the weights of the
    /// model no app has, which the archive disk holds too.
    static let offloadBytes: UInt64 = 79_210_000

    /// Whether a clean-up may offer that file: its model is in the store
    /// and in no app.
    static func canOffload(_ model: LibraryModel) -> Bool {
        model.repo == smolMLX && !model.isArchived && model.apps.isEmpty
    }

    /// Whether a model's weights left the internal disk in a clean-up.
    static func weightsAway(_ model: LibraryModel, offloaded: Bool) -> Bool {
        offloaded && model.repo == smolMLX && !model.isArchived
    }

    /// What one app holds of a library.
    struct Held {
        var files: UInt64
        var bytes: UInt64
        var models: UInt64
    }

    /// What one app holds of a library, as the daemon adds its files up.
    static func usage(of app: ModelApp, in library: [LibraryModel]) -> Held {
        let held = library.compactMap { model in model.apps.first { $0.app == app } }
        return Held(
            files: held.map(\.files).reduce(0, +), bytes: held.map(\.bytes).reduce(0, +), models: UInt64(held.count)
        )
    }

    /// The status of a daemon that holds this library. Every figure is
    /// added up from the models, as the real daemon adds up its records.
    static func status(
        jobs: [Job] = [], library: [LibraryModel] = models, cleaned: Bool = false, offloaded: Bool = false
    ) -> DaemonStatus {
        let active = jobs.filter { $0.state.isActive }
        let store = stored(of: library, cleaned: cleaned, offloaded: offloaded)
        // By the app's name, as the daemon lists them; an app with nothing
        // is not listed.
        let apps: [AppUsage] = [ModelApp.bionic, .huggingFace, .ollama].compactMap { app in
            let held = usage(of: app, in: library)
            return held.files == 0 ? nil : AppUsage(app: app, files: held.files, bytes: held.bytes)
        }
        return DaemonStatus(
            version: "0.1.0",
            home: "/Users/example/.dedupd",
            endpoint: "https://huggingface.co",
            archivePath: archiveFolder,
            archiveAttached: true,
            storeFiles: store.files,
            storeBytes: store.bytes,
            // Every canned app file is a clone of the stored one.
            sharedBytes: apps.map(\.bytes).reduce(0, +),
            downloadingTotal: active.map(\.bytesTotal).reduce(0, +),
            downloadingDone: active.map(\.bytesDone).reduce(0, +),
            apps: apps,
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

    static func presence(_ app: ModelApp, files: UInt64, bytes: UInt64) -> AppPresence {
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
            apps: [presence(.bionic, files: 4, bytes: 105_460_000), presence(.ollama, files: 4, bytes: 105_460_000)],
            verified: 8,
            mismatch: 0,
            notUpstream: 0,
            unchecked: 0,
            upstream: .ok,
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
            apps: [presence(.bionic, files: 4, bytes: 156_380_000_000)],
            verified: 4,
            mismatch: 0,
            notUpstream: 0,
            unchecked: 0,
            upstream: .ok,
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
            apps: [presence(.bionic, files: 13, bytes: 16_080_000_000)],
            verified: 13,
            mismatch: 0,
            notUpstream: 0,
            unchecked: 0,
            upstream: .ok,
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
            apps: [presence(.bionic, files: 30, bytes: 98_160_000_000)],
            verified: 28,
            mismatch: 0,
            notUpstream: 0,
            unchecked: 2,
            upstream: .gone,
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
            upstream: .ok,
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

    /// One of the fixed models in full, as it is before anything is done
    /// to it.
    static func detail(_ repo: String) -> ModelDetail? {
        models.first { $0.repo == repo }.map { detail(of: $0) }
    }

    /// A model in full, as the daemon holds it now: its files are where the
    /// model's own figures say (in the store unless it is archived, in the
    /// apps it lists).
    static func detail(of model: LibraryModel, offloaded: Bool = false) -> ModelDetail {
        let repo = model.repo
        let here = !model.isArchived
        let away = weightsAway(model, offloaded: offloaded)
        let isGGUF = model.meta.formats.contains("gguf")
        let files: [RevisionFile] = isGGUF
            ? ggufFiles.map { name, size in
                RevisionFile(
                    path: repo == smolGGUF ? name : name.replacingOccurrences(of: "SmolLM2-135M-Instruct", with: model.name),
                    size: repo == smolGGUF ? size : (name.hasSuffix(".gguf") ? model.storedBytes : size),
                    sha256: sha(name),
                    gitOid: String(sha(name).prefix(40)),
                    lfs: name.hasSuffix(".gguf"),
                    stored: here,
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
                    stored: here && !(away && name == "model.safetensors"),
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
                    verifyState: .verified,
                    origin: app.app == .bionic ? .adopted : .placed
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
            variants: variants(of: model, offloaded: offloaded),
            infoJson: #"{"id":"\#(repo)","pipeline_tag":"text-generation","private":false,"gated":false}"#,
            readme: readme
        )
    }

    private static func variants(of model: LibraryModel, offloaded: Bool) -> [Variant] {
        let judged = judged(model.repo, formats: model.meta.formats, stored: true)
        // An archived model has nothing in the store: the daemon then says
        // no bytes are stored and the weights are not there.
        // One whose weights went in a clean-up keeps its small files only.
        let away = weightsAway(model, offloaded: offloaded)
        let here = !model.isArchived && !away
        let stored = model.isArchived ? 0 : model.storedBytes - (away ? offloadBytes : 0)
        if model.meta.formats.contains("gguf") {
            return [
                Variant(
                    kind: .gguf, format: judged.format, name: "Q4_K_M", spec: "\(model.repo):Q4_K_M", files: 4,
                    bytes: model.storedBytes, storedBytes: stored, weightsStored: here, usableBy: judged.usableBy,
                    placedIn: model.appNames, decisions: judged.decisions
                ),
            ]
        }
        return [
            Variant(
                kind: .files, format: judged.format, name: "all files", spec: model.repo, files: model.storedFiles,
                bytes: model.storedBytes, storedBytes: stored, weightsStored: here, usableBy: judged.usableBy,
                placedIn: model.appNames, decisions: judged.decisions
            ),
        ]
    }

    /// What the app profiles answer for a variant: its format, every
    /// app's decision, and the apps that can take it (those answered
    /// `ready`), as the daemon puts them on a variant.
    struct Judged {
        var format: ModelFormat
        var decisions: [Decision]
        var usableBy: [ModelApp] { decisions.filter { $0.state == .ready }.map(\.app) }
    }

    /// What the app profiles answer for a repository the canned daemon
    /// knows, recorded from the real profiles (`RecordedProfiles`). The bit
    /// width of an MLX model is known once its `config.json` is stored; the
    /// canned daemon reads it from the name (`-4bit`).
    static func judged(_ repo: String, formats: [String], stored: Bool) -> Judged {
        if formats.contains("gguf") {
            return Judged(format: .gguf, decisions: RecordedProfiles.gguf)
        }
        guard formats.contains("mlx") else {
            return Judged(format: .other, decisions: RecordedProfiles.other)
        }
        let bits = stored ? repo.lowercased().firstMatch(of: /(\d+)-?bit/).flatMap { Int($0.1) } : nil
        let decisions = switch bits {
        case 4: RecordedProfiles.mlx4
        case 6: RecordedProfiles.mlx6
        case 8: RecordedProfiles.mlx8
        default: RecordedProfiles.mlxUnknownBits
        }
        return Judged(format: .mlx, decisions: decisions)
    }

    static func folder(of app: ModelApp) -> String {
        switch app {
        case .bionic: "/Users/example/.lmstudio/models"
        case .ollama: "/Users/example/.ollama/models"
        case .huggingFace: "/Users/example/.cache/huggingface/hub"
        }
    }

    /// A made-up digest that is the same for the same name.
    static func sha(_ name: String) -> String {
        let seed = name.utf8.reduce(UInt64(1469598103934665603)) { ($0 ^ UInt64($1)) &* 1099511628211 }
        return String(repeating: String(seed, radix: 16), count: 4).prefix(64).padding(toLength: 64, withPad: "0", startingAt: 0)
    }

    // MARK: - Daemon

    static let settings = DaemonSettings(
        path: "/Users/example/.dedupd/config.toml",
        listen: address,
        hfEndpoint: "https://huggingface.co",
        archive: "/Volumes/Archive/dedupd",
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
