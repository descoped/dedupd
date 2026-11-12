import DedupdClient
import Foundation

/// The canned daemon's view of Hugging Face and its download queue.
extension FixtureData {
    // MARK: - Search

    private static func hit(
        _ repo: String,
        formats: [String],
        parameters: UInt64,
        downloads: UInt64,
        likes: UInt64,
        license: String = "apache-2.0"
    ) -> SearchHit {
        let local = models.first { $0.repo == repo }
        return SearchHit(
            id: repo,
            meta: meta(repo, formats: formats, parameters: parameters, license: license, downloads: downloads, likes: likes),
            archived: local != nil,
            apps: local?.appNames ?? [],
            downloading: false
        )
    }

    static let hits: [SearchHit] = [
        hit(smolGGUF, formats: ["gguf"], parameters: 134_515_008, downloads: 104_300, likes: 61),
        hit("unsloth/Qwen3.8-9B-Instruct-GGUF", formats: ["gguf"], parameters: 9_100_000_000, downloads: 912_000, likes: 540),
        hit(
            "mlx-community/Qwen3.8-9B-Instruct-4bit", formats: ["mlx", "safetensors"], parameters: 9_100_000_000,
            downloads: 71_400, likes: 96
        ),
        hit(deepSeek, formats: ["gguf"], parameters: 284_000_000_000, downloads: 48_200, likes: 310, license: "mit"),
        hit(
            "bartowski/Gemma-4-12B-it-GGUF", formats: ["gguf"], parameters: 12_200_000_000, downloads: 388_000,
            likes: 271, license: "gemma"
        ),
        hit(smolMLX, formats: ["mlx", "safetensors"], parameters: 134_515_008, downloads: 9100, likes: 12),
        hit(
            "HuggingFaceTB/SmolLM2-135M-Instruct", formats: ["safetensors", "onnx"], parameters: 134_515_008,
            downloads: 2_410_000, likes: 302
        ),
    ]

    static let moreHits: [SearchHit] = [
        hit(
            "bartowski/Phi-5-mini-instruct-GGUF", formats: ["gguf"], parameters: 3_800_000_000, downloads: 204_000,
            likes: 133, license: "mit"
        ),
        hit(
            "mlx-community/Phi-5-mini-instruct-8bit", formats: ["mlx", "safetensors"], parameters: 3_800_000_000,
            downloads: 18_300, likes: 22, license: "mit"
        ),
    ]

    // MARK: - One model

    private static let quants: [(String, UInt64)] = [
        ("Q2_K", 88_200_000), ("Q3_K_M", 93_500_000), ("Q4_K_M", 105_450_000), ("Q5_K_M", 112_100_000),
        ("Q6_K", 138_400_000), ("Q8_0", 144_800_000),
    ]

    static func hubModel(_ repo: String, downloading: Bool = false) -> HubModel {
        let hit = (hits + moreHits).first { $0.id == repo }
        let local = models.first { $0.repo == repo }
        let meta = local?.meta ?? hit?.meta ?? self.meta(repo, formats: ["gguf"], parameters: 134_515_008)
        let name = repo.split(separator: "/").last.map(String.init) ?? repo
        let isGGUF = meta.formats.contains("gguf")
        let variants: [Variant]
        let files: [HubFile]
        if isGGUF {
            let stem = name.replacingOccurrences(of: "-GGUF", with: "")
            variants = quants.map { quant, bytes in
                let here = local != nil && quant == "Q4_K_M"
                return Variant(
                    kind: "gguf",
                    name: quant,
                    spec: "\(repo):\(quant)",
                    files: 4,
                    bytes: bytes,
                    storedBytes: here ? bytes : 0,
                    usableBy: ["bionic", "ollama", "hf"],
                    placedIn: here ? local?.appNames ?? [] : [],
                    decisions: decisions(usableBy: ["bionic", "ollama", "hf"])
                )
            }
            files = quants.map { quant, bytes in
                HubFile(
                    path: "\(stem)-\(quant).gguf", size: bytes, sha256: sha(quant), lfs: true,
                    stored: local != nil && quant == "Q4_K_M"
                )
            } + [
                HubFile(path: "README.md", size: 6120, sha256: nil, lfs: false, stored: local != nil),
                HubFile(path: "config.json", size: 720, sha256: nil, lfs: false, stored: local != nil),
            ]
        } else {
            let bytes: UInt64 = local?.storedBytes ?? 5_420_000_000
            let mlx = meta.formats.contains("mlx")
            variants = [
                Variant(
                    kind: "files",
                    name: "all files",
                    spec: repo,
                    files: 8,
                    bytes: bytes,
                    storedBytes: local == nil ? 0 : bytes,
                    usableBy: mlx ? ["bionic", "hf"] : ["hf"],
                    placedIn: local?.appNames ?? [],
                    decisions: decisions(usableBy: mlx ? ["bionic", "hf"] : ["hf"], mlx: mlx)
                ),
            ]
            files = [
                HubFile(
                    path: "model.safetensors", size: bytes - 4_000_000, sha256: sha("model.safetensors"), lfs: true,
                    stored: local != nil
                ),
                HubFile(path: "tokenizer.json", size: 3_990_000, sha256: nil, lfs: false, stored: local != nil),
                HubFile(path: "config.json", size: 1200, sha256: nil, lfs: false, stored: local != nil),
            ]
        }
        var detailed = meta
        detailed.usedStorage = files.map(\.size).reduce(0, +)
        return HubModel(
            id: repo,
            commit: local?.commit ?? "09816acd5d99c8a1c1b2f6d0d0a1b2c3d4e5f607",
            fromArchive: false,
            meta: detailed,
            files: files,
            variants: variants,
            related: hits.filter { $0.id != repo }.prefix(3).map { $0 },
            readme: readme,
            archived: local != nil,
            apps: local?.apps ?? [],
            downloading: downloading,
            infoJson: #"{"id":"\#(repo)","pipeline_tag":"text-generation","private":false,"gated":false,"downloads":104300}"#
        )
    }

    // MARK: - Downloads

    static func job(
        id: Int64,
        request: PullRequest,
        state: JobState,
        total: UInt64 = 144_800_000,
        done: UInt64 = 0,
        speed: UInt64 = 0,
        detail: String? = nil
    ) -> Job {
        let parts = request.model.split(separator: ":", maxSplits: 1).map(String.init)
        let finished = state == .done
        return Job(
            id: id,
            model: request.model,
            repo: parts.first ?? request.model,
            revision: "main",
            quant: parts.count > 1 ? parts[1] : nil,
            include: request.include,
            apps: request.apps,
            offline: request.offline,
            state: state.rawValue,
            detail: detail,
            waitUntil: nil,
            commit: "09816acd5d99c8a1c1b2f6d0d0a1b2c3d4e5f607",
            bytesTotal: total,
            bytesDone: finished ? total : done,
            speed: speed,
            filesTotal: 4,
            filesDone: finished ? 4 : (done > 0 ? 3 : 0),
            createdAt: 1_794_003_000 + id,
            updatedAt: 1_794_003_600 + id,
            finishedAt: finished ? 1_794_003_600 + id : nil
        )
    }

    /// The queue of the daemon that is in the middle of a download.
    static let jobs: [Job] = [
        job(
            id: 7,
            request: PullRequest(model: "\(smolGGUF):Q8_0", apps: ["bionic", "ollama"]),
            state: .downloading,
            total: 106_070_000,
            done: 41_940_000,
            speed: 11_800_000
        ),
        job(
            id: 6,
            request: PullRequest(model: "unsloth/Qwen3.8-9B-Instruct-GGUF:Q4_K_M", apps: ["ollama"]),
            state: .queued,
            total: 5_630_000_000
        ),
        job(
            id: 5,
            request: PullRequest(model: "meta-llama/Llama-5-8B-Instruct", apps: ["hf"]),
            state: .needsAttention,
            total: 0,
            detail: "Hugging Face asks you to accept the model's terms first (gated repository)."
        ),
        job(
            id: 4,
            request: PullRequest(model: "mlx-community/Qwen3.8-9B-Instruct-4bit", apps: ["bionic"]),
            state: .done,
            total: 5_420_000_000
        ),
    ]

    static func jobDetail(_ job: Job) -> JobDetail {
        let weights = job.bytesTotal > 10_000 ? job.bytesTotal - 10_000 : job.bytesTotal
        let stage: String = switch job.phase {
        case .done: "stored"
        case .downloading: "downloading"
        default: "waiting"
        }
        let name = job.repo.split(separator: "/").last.map(String.init) ?? job.repo
        return JobDetail(
            job: job,
            files: [
                JobFile(
                    path: job.quant.map { "\(name)-\($0).gguf" } ?? "model.safetensors",
                    size: weights,
                    done: min(weights, job.bytesDone),
                    stage: stage,
                    sha256: sha(job.model),
                    gitOid: String(sha(job.model).prefix(40))
                ),
                JobFile(
                    path: "README.md", size: 6120, done: job.bytesDone > 0 ? 6120 : 0,
                    stage: job.bytesDone > 0 ? "stored" : "waiting", sha256: nil, gitOid: String(sha("README.md").prefix(40))
                ),
                JobFile(
                    path: "config.json", size: 720, done: job.bytesDone > 0 ? 720 : 0,
                    stage: job.bytesDone > 0 ? "stored" : "waiting", sha256: nil, gitOid: String(sha("config.json").prefix(40))
                ),
            ],
            events: [
                JobEvent(at: job.createdAt, text: "Queued"),
                JobEvent(at: job.createdAt + 2, text: "Found 4 files at revision 09816acd5d99"),
                JobEvent(at: job.updatedAt, text: job.detail ?? job.phase.title),
            ]
        )
    }
}
