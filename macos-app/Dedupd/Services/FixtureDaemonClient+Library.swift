import DedupdClient
import Foundation

/// What the owner does to a model: each changes the library the canned
/// daemon holds, as the real daemon's does.
extension FixtureDaemonClient {
    // MARK: - Models

    func checkUpstream(_ repo: String, at address: String) async throws -> UpstreamCheck {
        try await answering(address)
        let model = try held(repo)
        return UpstreamCheck(upstream: model.upstream, commit: model.commit, latest: model.commit, changed: false)
    }

    func forget(_ repo: String, at address: String) async throws {
        try await answering(address)
        let model = try held(repo)
        let files = model.apps.map(\.files).reduce(0, +)
        if files > 0, let first = FixtureData.detail(of: model).placements.first {
            throw ClientError.Failed(
                message: "\(repo) still has \(files) files in app folders (for example \(first.path)). "
                    + "Remove the model from the apps first."
            )
        }
        // Its records go; its files stay in the store until a clean-up.
        models.removeAll { $0.repo == repo }
        library += 1
        changed()
    }

    func unplace(_ repo: String, app: ModelApp, paths: [String]?, apply: Bool, at address: String) async throws
        -> UnplacePlan {
        try await answering(address)
        let model = try held(repo)
        let files = FixtureData.detail(of: model).placements.filter { $0.app == app }
            .filter { paths?.contains($0.path) ?? true }
        let plan = UnplacePlan(
            repo: repo,
            app: app,
            remove: files.map { PlannedFile(path: $0.path, size: $0.size) },
            links: app == .ollama ? model.ollamaTags.map { "\(FixtureData.folder(of: app))/manifests/hf.co/\(repo)/\($0)" } : [],
            keep: [],
            bytes: files.map(\.size).reduce(0, +),
            removed: apply ? UInt64(files.count) : nil
        )
        if apply, model.apps.contains(where: { $0.app == app }) {
            note(.init(action: .deactivate, repo: repo, app: app, ok: true, text: "Deactivated in \(FixtureData.inSentence(app))"))
            change(repo) { model in
                model.apps.removeAll { $0.app == app }
                if app == .ollama {
                    model.ollamaTags = []
                }
            }
        }
        return plan
    }

    func archivePlan(_ repo: String, at address: String) async throws -> ArchivePlan {
        try await answering(address)
        let model = try held(repo)
        return ArchivePlan(
            repo: repo,
            archive: FixtureData.archiveFolder,
            deactivate: model.apps.map { DeactivateStep(app: $0.app, files: $0.files, bytes: $0.bytes, keep: []) },
            files: model.isArchived ? 0 : model.storedFiles,
            bytes: model.isArchived ? 0 : model.storedBytes,
            copyFiles: 0,
            copyBytes: 0,
            keep: []
        )
    }

    /// The canned daemon moves a model at once: out of every app and off
    /// the internal disk, with the daemon's own line in the history.
    func archive(_ repo: String, at address: String) async throws {
        try await answering(address)
        let model = try held(repo)
        let (files, bytes) = model.isArchived ? (0, 0) : (model.storedFiles, model.storedBytes)
        note(.init(
            action: .archive, repo: repo, app: nil, ok: true,
            text: "Moved to the archive disk: \(FixtureData.counted(files)), \(ByteFormat.string(bytes)) freed on this Mac"
        ))
        moved(kind: .archive, repo: repo, files: files, bytes: bytes)
        change(repo) { model in
            model.apps = []
            model.ollamaTags = []
            model.archivedAt = FixtureData.now
        }
    }

    /// Restoring copies the files back and puts the model into no app.
    func restore(_ repo: String, at address: String) async throws {
        try await answering(address)
        let model = try held(repo)
        guard model.isArchived else {
            throw ClientError.Failed(message: "\(repo) is not archived")
        }
        note(.init(
            action: .restore, repo: repo, app: nil, ok: true,
            text: "Brought back from the archive disk: \(FixtureData.counted(model.storedFiles)), "
                + ByteFormat.string(model.storedBytes)
        ))
        moved(kind: .restore, repo: repo, files: model.storedFiles, bytes: model.storedBytes)
        change(repo) { $0.archivedAt = nil }
    }

    func moved(kind: MoveKind, repo: String, files: UInt64, bytes: UInt64) {
        moving = ModelMove(
            kind: kind, repo: repo, running: false, startedAt: FixtureData.now, finishedAt: FixtureData.now,
            bytesTotal: bytes, bytesDone: bytes, files: files, kept: 0, error: nil
        )
    }
}
