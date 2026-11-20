import DedupdClient
import Foundation

// MARK: - Downloads

extension FixtureDaemonClient {

    func download(_ id: Int64, at address: String) async throws -> JobDetail {
        try await answering(address)
        guard let job = jobs.first(where: { $0.id == id }) else {
            throw ClientError.Failed(message: "there is no download \(id)")
        }
        return FixtureData.jobDetail(job)
    }

    func enqueue(_ request: PullRequest, at address: String) async throws -> Enqueued {
        try await answering(address)
        // Only the name is checked here; the rest of a spec is the real
        // daemon's rule, which is not written a second time.
        guard let repo = ModelSpec.repository(ofSpec: request.model) else {
            throw ClientError.Failed(
                message: "\(request.model) is not a model name; use org/repo, org/repo:Q4_K_M or org/repo@revision"
            )
        }
        if let existing = jobs.first(where: { $0.model == request.model && !$0.state.isFinished }) {
            return Enqueued(id: existing.id, existing: true)
        }
        let id = nextID
        nextID += 1
        // What is already stored is placed at once; anything else waits its
        // turn, as it would behind a running download. An app the recorded
        // profiles say no for does not take the model, with their reason.
        guard let stored = models.first(where: { $0.repo == repo }) else {
            jobs.insert(FixtureData.job(id: id, request: request, state: .queued), at: 0)
            changed()
            return Enqueued(id: id, existing: false)
        }
        var notTakenBy: [ModelApp: String] = [:]
        let decisions = FixtureData.judged(repo, formats: stored.meta.formats, stored: true).decisions
        for decision in decisions where decision.state != .ready && request.apps.contains(decision.app) {
            notTakenBy[decision.app] = decision.reason
        }
        // Nothing was fetched, so the daemon shows the job as an activation
        // whatever it was asked as.
        var asked = request
        asked.kind = .activation
        let job = FixtureData.job(id: id, request: asked, state: .done, notTakenBy: notTakenBy)
        jobs.insert(job, at: 0)
        for app in request.apps {
            // The history holds the app's own reason, not a sentence about it.
            note(.init(
                action: .activate, repo: repo, app: app, ok: notTakenBy[app] == nil,
                text: notTakenBy[app] ?? "Activated in \(FixtureData.inSentence(app))"
            ))
        }
        // The model is now in the apps that took it; activating an archived
        // model brings its files back first.
        wholeAgain(repo)
        change(repo) { model in
            model.archivedAt = nil
            for app in job.activated where !model.apps.contains(where: { $0.app == app }) {
                model.apps.append(FixtureData.presence(app, files: model.storedFiles, bytes: model.storedBytes))
                if app == .ollama {
                    model.ollamaTags.append(job.quant ?? "latest")
                }
            }
            model.apps.sort { $0.app.name < $1.app.name }
        }
        return Enqueued(id: id, existing: false)
    }

    func downloadAction(_ action: JobAction, id: Int64, at address: String) async throws {
        try await answering(address)
        guard let index = jobs.firstIndex(where: { $0.id == id }) else {
            throw ClientError.Failed(message: "there is no download \(id)")
        }
        guard jobs[index].actions.contains(action) else {
            throw ClientError.Failed(message: FixtureData.notNow(id, jobs[index].state, action))
        }
        switch action {
        case .pause:
            jobs[index].state = .paused
            jobs[index].speed = 0
        case .resume, .retry, .start:
            jobs[index].state = .downloading
            jobs[index].detail = nil
            jobs[index].speed = 11_800_000
        case .cancel:
            jobs[index].state = .cancelled
            jobs[index].speed = 0
        case .remove:
            jobs.remove(at: index)
            changed()
            return
        }
        jobs[index].actions = jobActions(state: jobs[index].state)
        changed()
    }

    func activity(at address: String) async throws -> [ActivityEntry] {
        try await answering(address)
        return history
    }

    func favorites(at address: String) async throws -> Favorites {
        try await answering(address)
        return favoriteList
    }

    func setFavorite(_ repo: String, _ favorite: Bool, at address: String) async throws -> Favorites {
        try await answering(address)
        favoriteList.favorites.removeAll { $0.repo == repo }
        if favorite {
            favoriteList.favorites.insert(Favorite(repo: repo, addedAt: FixtureData.now, source: .dedupd), at: 0)
        }
        changed()
        return favoriteList
    }

    func syncFavorites(at address: String) async throws -> Favorites {
        try await answering(address)
        favoriteList.likes.syncedAt = FixtureData.now
        changed()
        return favoriteList
    }

    /// Add to the canned activity list, newest first.
    func note(_ entry: FixtureData.Happened) {
        let id = (history.first?.id ?? 0) + 1
        history.insert(
            ActivityEntry(
                id: id, at: Int64(Date().timeIntervalSince1970), action: entry.action, repo: entry.repo,
                app: entry.app, ok: entry.ok, text: entry.text
            ),
            at: 0
        )
    }

    func clearDownloads(at address: String) async throws -> UInt64 {
        try await answering(address)
        let before = jobs.count
        jobs.removeAll { $0.state.isFinished }
        changed()
        return UInt64(before - jobs.count)
    }
}
