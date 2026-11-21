import DedupdClient
import Foundation
import Observation

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
    /// The models Hugging Face is being asked about.
    private var checking: Set<String> = []
    /// The selected model is being asked about.
    var isChecking: Bool { selected.map { checking.contains($0.repo) } ?? false }
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
    /// A model another screen asked for by name before the list was read;
    /// it is selected when the list has it.
    @ObservationIgnored private var wanted: String?

    init(daemon: DaemonStore) {
        self.daemon = daemon
        daemon.whenAddressChanges { [weak self] in self?.forgetDaemon() }
    }

    /// The app turned to another daemon: nothing of the old one stays, and
    /// what it still answers is dropped.
    private func forgetDaemon() {
        listGeneration += 1
        detailGeneration += 1
        models = []
        hasLoaded = false
        failure = nil
        seen = [:]
        wanted = nil
        selection = nil
        removal = nil
        archiving = nil
        checking = []
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

    /// Select a model by its name, as another screen knows it. When the
    /// list has not been read yet, the model is selected once it has.
    func select(repo: String) {
        if let model = models.first(where: { $0.repo == repo }) {
            wanted = nil
            selection = model.id
        } else {
            wanted = repo
        }
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
        if let wanted {
            self.wanted = nil
            if let model = models.first(where: { $0.repo == wanted }) {
                selection = model.id
            }
        }
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
        guard let model = selected, !checking.contains(model.repo) else {
            return
        }
        checking.insert(model.repo)
        let answer = await daemon.attempt { client, address in try await client.checkUpstream(model.repo, at: address) }
        checking.remove(model.repo)
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
        let plan = await daemon.attempt { client, address in
            try await client.unplace(model.repo, app: app, paths: nil, apply: false, at: address)
        }
        // The owner may have gone on to another model meanwhile; a sheet
        // for this one is not put over that one.
        if selected?.repo == model.repo {
            removal = plan
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

    /// The archives a model can go to now: the attached ones, the backup
    /// archive first.
    var archiveChoices: [ArchiveSetting] { (daemon.status?.archives ?? []).filter(\.attached) }

    /// Ask what archiving the selected model would do, to the archive named
    /// `to` (nil: the backup archive, or else the first attached one).
    func planArchive(to name: String? = nil) async {
        guard let model = selected else {
            return
        }
        let target = name ?? archiveChoices.first(where: \.backup)?.name ?? archiveChoices.first?.name
        let plan = await daemon.attempt { client, address in
            try await client.archivePlan(model.repo, to: target, at: address)
        }
        if selected?.repo == model.repo, let plan {
            archiving = plan
        }
    }

    /// Archive the model the owner agreed to. The daemon does it in the
    /// background and its events tell how far it is.
    func applyArchive() async {
        guard let plan = archiving else {
            return
        }
        archiving = nil
        await daemon.attempt { client, address in
            try await client.archive(plan.repo, to: plan.archiveName, at: address)
        }
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
            // Only if the owner still looks at the model that was forgotten.
            if selection == model.id {
                selection = nil
            }
            await reload()
        }
    }
}
