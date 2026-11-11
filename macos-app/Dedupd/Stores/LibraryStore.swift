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
                detail = .idle
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

    @ObservationIgnored private let daemon: DaemonStore

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
                    && (filterApp.isEmpty || model.appNames.contains(filterApp))
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
        do {
            models = try await daemon.client.models(at: daemon.address)
            failure = nil
        } catch {
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
        if !quietly || detail.value == nil {
            detail = .loading
        }
        let answer: Loaded<ModelDetail>
        do {
            answer = .ready(try await daemon.client.model(model.repo, at: daemon.address))
        } catch {
            answer = .failed(describe(error))
        }
        // The owner may have selected another model meanwhile.
        if selected?.repo == model.repo {
            detail = answer
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
        await daemon.enqueue(PullRequest(model: variant.spec, apps: [app.rawValue]))
    }

    /// Fetch the files of a variant that are not on this Mac, for the apps
    /// that already have it.
    func download(_ variant: Variant) async {
        await daemon.enqueue(PullRequest(model: variant.spec, apps: variant.placedIn))
    }

    /// Ask what removing the selected model from an app would delete.
    func planRemoval(from app: String) async {
        guard let model = selected else {
            return
        }
        removal = await daemon.attempt { client, address in
            try await client.unplace(model.repo, app: app, apply: false, at: address)
        }
    }

    /// Remove what the plan lists. The stored copy stays.
    func applyRemoval() async {
        guard let plan = removal else {
            return
        }
        removal = nil
        await daemon.attempt { client, address in
            try await client.unplace(plan.repo, app: plan.app, apply: true, at: address)
        }
        await reload()
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
