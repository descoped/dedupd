import DedupdClient
import Foundation
import Observation

/// Something that is loaded from the daemon and may fail.
enum Loaded<Value: Equatable & Sendable>: Equatable, Sendable {
    case idle
    case loading
    case ready(Value)
    case failed(String)

    var value: Value? {
        if case let .ready(value) = self {
            return value
        }
        return nil
    }

    var isLoading: Bool { self == .loading }

    var failure: String? {
        if case let .failed(message) = self {
            return message
        }
        return nil
    }
}

/// The models on this Mac: the list, what the owner filters it by, and the
/// details of the one they selected.
@MainActor
@Observable
final class LibraryStore {
    private(set) var models: [LibraryModel] = []
    private(set) var hasLoaded = false
    private(set) var failure: String?

    /// Words the name must contain.
    var filterText = ""
    /// Only models of this format; empty for all.
    var filterFormat = ""
    /// Only models this app has; empty for all.
    var filterApp = ""
    var sortOrder = [KeyPathComparator(\LibraryModel.repo)]

    var selection: LibraryModel.ID? {
        didSet {
            if selection != oldValue {
                // A model seen before shows its last answer at once, so the
                // panel does not change shape; the fresh answer replaces it.
                detail = selection.flatMap { seen[$0] }.map(Loaded.ready) ?? .idle
                upstream = nil
            }
        }
    }

    private(set) var detail: Loaded<ModelDetail> = .idle
    /// The answer to the last "check for a newer version".
    private(set) var upstream: UpstreamCheck?
    private(set) var isChecking = false
    /// What removing the selected model from an app would delete; set while
    /// the owner is asked to confirm.
    var removal: UnplacePlan?
    /// What archiving the selected model would do; set while the owner is
    /// asked to confirm.
    var archiving: ArchivePlan?

    @ObservationIgnored private let daemon: DaemonStore
    /// Count the reads of the list and of the details, so an answer that
    /// comes back after a later one is dropped.
    @ObservationIgnored private var listGeneration = 0
    @ObservationIgnored private var detailGeneration = 0
    /// The last answer for each model looked at, by its id, until the
    /// library changes.
    @ObservationIgnored private var seen: [String: ModelDetail] = [:]

    init(daemon: DaemonStore) {
        self.daemon = daemon
    }

    // MARK: - The list

    /// The models after filtering, in the chosen order.
    var visible: [LibraryModel] {
        let words = filterText.trimmingCharacters(in: .whitespaces).lowercased()
        return models
            .filter { model in
                (words.isEmpty || model.repo.lowercased().contains(words))
                    && (filterFormat.isEmpty || model.meta.formats.contains(filterFormat))
                    && (filterApp.isEmpty || model.appNames.contains { $0.name == filterApp })
            }
            .sorted(using: sortOrder)
    }

    /// The formats that occur, for the filter.
    var formats: [String] {
        Array(Set(models.flatMap(\.meta.formats))).sorted()
    }

    var selected: LibraryModel? {
        models.first { $0.id == selection }
    }

    /// Select a model by its name, as another screen knows it. The list may
    /// not have been read yet; the id then follows the list's rule.
    func select(repo: String) {
        selection = models.first { $0.repo == repo }?.id ?? "hf/\(repo)"
    }

    /// "5 models · 290.12 GB", or how many of them the filter leaves.
    var countText: String {
        let shown = visible
        let bytes = ByteFormat.string(shown.map(\.bytes).reduce(0, +))
        let count = shown.count == models.count
            ? "\(models.count) model\(models.count == 1 ? "" : "s")"
            : "\(shown.count) of \(models.count) models"
        return "\(count) · \(bytes)"
    }

    /// Read the list again, and the selected model's details with it.
    func reload() async {
        // The library changed: the answers kept for other models may be out
        // of date. The selected one's stays on screen until its new answer.
        seen = [:]
        listGeneration += 1
        let mine = listGeneration
        let answer: Result<[LibraryModel], Error>
        do {
            answer = .success(try await daemon.client.models(at: daemon.address))
        } catch {
            answer = .failure(error)
        }
        // A later reload is out; its answer is the newer one.
        guard mine == listGeneration else {
            return
        }
        switch answer {
        case let .success(list):
            models = list
            failure = nil
        case let .failure(error):
            failure = describe(error)
        }
        hasLoaded = true
        if selection != nil, selected == nil {
            selection = nil
        }
        await loadDetail(quietly: true)
    }

    // MARK: - The selected model

    /// Read the selected model's details. Quietly keeps what is on screen
    /// until the new answer is there.
    func loadDetail(quietly: Bool = false) async {
        guard let model = selected else {
            detail = .idle
            return
        }
        detailGeneration += 1
        let mine = detailGeneration
        if !quietly || detail.value == nil {
            detail = .loading
        }
        let answer: Loaded<ModelDetail>
        do {
            answer = .ready(try await daemon.client.model(model.repo, at: daemon.address))
        } catch {
            answer = .failed(describe(error))
        }
        // The owner may have selected another model, or a later read may
        // have answered, meanwhile.
        if mine == detailGeneration, selected?.repo == model.repo {
            detail = answer
            if let value = answer.value {
                seen[model.id] = value
            }
        }
    }

    /// Ask Hugging Face whether the model changed or is gone.
    func checkUpstream() async {
        guard let model = selected, !isChecking else {
            return
        }
        isChecking = true
        let answer = await daemon.attempt { client, address in try await client.checkUpstream(model.repo, at: address) }
        isChecking = false
        if selected?.repo == model.repo {
            upstream = answer
        }
        await reload()
    }

    /// Place a stored variant into an app. Nothing is downloaded when the
    /// store has every file; the daemon reports it as a download that is
    /// done at once.
    func place(_ variant: Variant, into app: ModelApp) async {
        await daemon.enqueue(PullRequest(model: variant.spec, apps: [app], kind: .activation))
    }

    /// Fetch the files of a variant that are not on this Mac, for the apps
    /// that already have it.
    func download(_ variant: Variant) async {
        await daemon.enqueue(PullRequest(model: variant.spec, apps: variant.placedIn))
    }

    /// Ask what removing the selected model from an app would delete.
    func planRemoval(from app: ModelApp) async {
        guard let model = selected else {
            return
        }
        removal = await daemon.attempt { client, address in
            try await client.unplace(model.repo, app: app, paths: nil, apply: false, at: address)
        }
    }

    /// Remove what the plan lists, and nothing the owner was not shown. The
    /// stored copy stays.
    func applyRemoval() async {
        guard let plan = removal else {
            return
        }
        removal = nil
        let agreed = plan.remove.map(\.path)
        await daemon.attempt { client, address in
            try await client.unplace(plan.repo, app: plan.app, paths: agreed, apply: true, at: address)
        }
        await reload()
    }

    // MARK: - Archive and restore

    /// The archive or restore of the selected model: the one that is
    /// running, or the last one when it failed.
    var move: ModelMove? {
        guard let move = daemon.moving, move.repo == selected?.repo, move.running || move.error != nil else {
            return nil
        }
        return move
    }

    /// The daemon moves one model at a time.
    var isMoving: Bool { daemon.moving?.running == true }

    /// Ask what archiving the selected model would do.
    func planArchive() async {
        guard let model = selected else {
            return
        }
        archiving = await daemon.attempt { client, address in
            try await client.archivePlan(model.repo, at: address)
        }
    }

    /// Archive the model the owner agreed to. The daemon does it in the
    /// background and its events tell how far it is.
    func applyArchive() async {
        guard let plan = archiving else {
            return
        }
        archiving = nil
        await daemon.attempt { client, address in try await client.archive(plan.repo, at: address) }
    }

    /// Copy the selected model back from the archive disk.
    func restore() async {
        guard let model = selected else {
            return
        }
        await daemon.attempt { client, address in try await client.restore(model.repo, at: address) }
    }

    /// Drop the selected model from the records, so its stored files can be
    /// cleaned up. The daemon refuses while an app still has the model.
    func forget() async {
        guard let model = selected else {
            return
        }
        let done = await daemon.attempt { client, address in
            try await client.forget(model.repo, at: address)
            return true
        }
        if done == true {
            selection = nil
            await reload()
        }
    }
}
