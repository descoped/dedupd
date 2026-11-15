// Written by `just fixtures` from the daemon's app profiles
// (crates/dedupd-core/tests/canned_profiles.rs). Do not edit.

import DedupdClient

/// What the app profiles answer for the kinds of model the canned
/// daemon holds, with Ollama 0.40.16 installed.
enum RecordedProfiles {
    static let apps: [AppInfo] = [
        AppInfo(
            app: .bionic, name: "Bionic", version: nil,
            takes: [
                Taken(format: .gguf, cost: .nothing),
                Taken(format: .mlx, cost: .nothing),
            ]
        ),
        AppInfo(
            app: .ollama, name: "Ollama", version: "0.40.16",
            takes: [
                Taken(format: .gguf, cost: .nothing),
                Taken(format: .mlx, cost: .megabytes),
            ]
        ),
        AppInfo(
            app: .huggingFace, name: "The Hugging Face cache", version: nil,
            takes: [
                Taken(format: .gguf, cost: .nothing),
                Taken(format: .mlx, cost: .nothing),
                Taken(format: .other, cost: .nothing),
            ]
        ),
    ]

    static let gguf: [Decision] = [
        Decision(
            app: .bionic, state: .ready, method: "clone", cost: .nothing,
            reason: "Shares the stored files",
            version: nil, versionState: .unknown
        ),
        Decision(
            app: .ollama, state: .ready, method: "clone", cost: .nothing,
            reason: "Shares the stored file",
            version: "0.40.16", versionState: .tested
        ),
        Decision(
            app: .huggingFace, state: .ready, method: "clone", cost: .nothing,
            reason: "Shares the stored files",
            version: nil, versionState: .unknown
        ),
    ]

    static let splitGGUF: [Decision] = [
        Decision(
            app: .bionic, state: .ready, method: "clone", cost: .nothing,
            reason: "Shares the stored files",
            version: nil, versionState: .unknown
        ),
        Decision(
            app: .ollama, state: .no, method: nil, cost: nil,
            reason: "Ollama cannot use a GGUF that is split into parts",
            version: "0.40.16", versionState: .tested
        ),
        Decision(
            app: .huggingFace, state: .ready, method: "clone", cost: .nothing,
            reason: "Shares the stored files",
            version: nil, versionState: .unknown
        ),
    ]

    static let mlxUnknownBits: [Decision] = [
        Decision(
            app: .bionic, state: .ready, method: "clone", cost: .nothing,
            reason: "Shares the stored files",
            version: nil, versionState: .unknown
        ),
        Decision(
            app: .ollama, state: .ready, method: "clone-with-rewritten-index", cost: .megabytes,
            reason: "Shares the stored files; only the index of each weights file is new",
            version: "0.40.16", versionState: .tested
        ),
        Decision(
            app: .huggingFace, state: .ready, method: "clone", cost: .nothing,
            reason: "Shares the stored files",
            version: nil, versionState: .unknown
        ),
    ]

    static let mlx4: [Decision] = [
        Decision(
            app: .bionic, state: .ready, method: "clone", cost: .nothing,
            reason: "Shares the stored files",
            version: nil, versionState: .unknown
        ),
        Decision(
            app: .ollama, state: .ready, method: "clone-with-rewritten-index", cost: .megabytes,
            reason: "Shares the stored files; only the index of each weights file is new",
            version: "0.40.16", versionState: .tested
        ),
        Decision(
            app: .huggingFace, state: .ready, method: "clone", cost: .nothing,
            reason: "Shares the stored files",
            version: nil, versionState: .unknown
        ),
    ]

    static let mlx6: [Decision] = [
        Decision(
            app: .bionic, state: .ready, method: "clone", cost: .nothing,
            reason: "Shares the stored files",
            version: nil, versionState: .unknown
        ),
        Decision(
            app: .ollama, state: .no, method: nil, cost: nil,
            reason: "Ollama 0.40.16 cannot run 6-bit MLX models; it runs 4-bit and 8-bit ones",
            version: "0.40.16", versionState: .tested
        ),
        Decision(
            app: .huggingFace, state: .ready, method: "clone", cost: .nothing,
            reason: "Shares the stored files",
            version: nil, versionState: .unknown
        ),
    ]

    static let mlx8: [Decision] = [
        Decision(
            app: .bionic, state: .ready, method: "clone", cost: .nothing,
            reason: "Shares the stored files",
            version: nil, versionState: .unknown
        ),
        Decision(
            app: .ollama, state: .ready, method: "clone-with-rewritten-index", cost: .megabytes,
            reason: "Shares the stored files; only the index of each weights file is new",
            version: "0.40.16", versionState: .tested
        ),
        Decision(
            app: .huggingFace, state: .ready, method: "clone", cost: .nothing,
            reason: "Shares the stored files",
            version: nil, versionState: .unknown
        ),
    ]

    static let other: [Decision] = [
        Decision(
            app: .bionic, state: .no, method: nil, cost: nil,
            reason: "Bionic loads GGUF and MLX models, and this is neither",
            version: nil, versionState: .unknown
        ),
        Decision(
            app: .ollama, state: .no, method: nil, cost: nil,
            reason: "Ollama cannot run this format",
            version: "0.40.16", versionState: .tested
        ),
        Decision(
            app: .huggingFace, state: .ready, method: "clone", cost: .nothing,
            reason: "Shares the stored files",
            version: nil, versionState: .unknown
        ),
    ]
}
