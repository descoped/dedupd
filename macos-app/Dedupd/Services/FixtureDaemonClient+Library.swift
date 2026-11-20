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
        wholeAgain(repo)
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
        let left = leaving(model)
        return ArchivePlan(
            repo: repo,
            archive: FixtureData.archiveFolder,
            deactivate: model.apps.map { DeactivateStep(app: $0.app, files: $0.files, bytes: $0.bytes, keep: []) },
            files: left.files,
            bytes: left.bytes,
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
        try free(repo)
        let (files, bytes) = leaving(model)
        note(.init(
            action: .archive, repo: repo, app: nil, ok: true,
            text: "Moved to the archive disk: \(FixtureData.counted(files)), \(ByteFormat.string(bytes)) freed on this Mac"
        ))
        moved(kind: .archive, repo: repo, files: files, bytes: bytes)
        wholeAgain(repo)
        change(repo) { model in
            model.apps = []
            model.ollamaTags = []
            model.archivedAt = FixtureData.now
        }
    }

    /// What archiving a model takes off the internal disk: what the store
    /// holds of it now.
    private func leaving(_ model: LibraryModel) -> (files: UInt64, bytes: UInt64) {
        if model.isArchived {
            return (0, 0)
        }
        let away = FixtureData.weightsAway(model, offloaded: offloaded)
        return (model.storedFiles - (away ? 1 : 0), model.storedBytes - (away ? FixtureData.offloadBytes : 0))
    }

    /// The daemon starts no archive or restore of a model that has a job
    /// in the queue that is not finished.
    private func free(_ repo: String) throws {
        if jobs.contains(where: { $0.repo == repo && !$0.state.isFinished }) {
            throw ClientError.Failed(message: FixtureData.busy(repo))
        }
    }

    /// A model that is archived, restored, activated or forgotten no
    /// longer has weights a clean-up moved away: they are all gone, or all
    /// back.
    func wholeAgain(_ repo: String) {
        if repo == FixtureData.smolMLX {
            offloaded = false
        }
    }

    /// Restoring copies the files back and puts the model into no app.
    func restore(_ repo: String, at address: String) async throws {
        try await answering(address)
        let model = try held(repo)
        guard model.isArchived else {
            throw ClientError.Failed(message: "\(repo) is not archived")
        }
        try free(repo)
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
