import DedupdClient
import Foundation
import Observation

/// The orders Hugging Face can list models in, as the daemon knows them.
extension SearchSort: @retroactive CaseIterable, @retroactive Identifiable {
    /// In the order the menu lists them.
    public static let allCases: [SearchSort] = [.trending, .downloads, .likes, .modified, .created]

    public var id: Self { self }

    var title: String {
        switch self {
        case .trending: "Trending"
        case .downloads: "Most downloaded"
        case .likes: "Most liked"
        case .modified: "Recently updated"
        case .created: "Newest"
        }
    }
}

/// Searching Hugging Face through the daemon, and looking at one model
/// before downloading it.
@MainActor
@Observable
final class DiscoverStore {
    /// The formats and tasks the filter offers. An empty value means any.
    static let formats = ["", "gguf", "mlx", "safetensors", "onnx"]
    static let tasks = [
        "", "text-generation", "image-text-to-text", "feature-extraction", "automatic-speech-recognition",
        "text-to-speech", "text-to-image", "translation",
    ]

    var text = ""
    var format = ""
    var task = ""
    var sort = SearchSort.trending

    private(set) var hits: [SearchHit] = []
    /// Where the next page starts; nil when there is none.
    private(set) var next: String?
    private(set) var isSearching = false
    private(set) var isLoadingMore = false
    private(set) var hasSearched = false
    private(set) var failure: String?
    /// Why the text in the field was not used: an address at a source that
    /// is not built yet, or one that is not a model's address.
    private(set) var notice: String?

    var selection: SearchHit.ID? {
        didSet {
            if selection != oldValue {
                detail = .idle
            }
            // Another choice ends the hold on a model opened by name.
            if selection != opened {
                opened = nil
            }
        }
    }

    private(set) var detail: Loaded<HubModel> = .idle

    @ObservationIgnored private let daemon: DaemonStore
    @ObservationIgnored private var generation = 0
    /// The model last opened by name, which a search does not deselect.
    @ObservationIgnored private var opened: SearchHit.ID?
    /// Counts the reads of a model's details, so an answer that comes back
    /// after a later one is dropped.
    @ObservationIgnored private var detailGeneration = 0
    /// The words of the last search, as they were checked; a next page
    /// asks for the same ones.
    @ObservationIgnored private var words = ""

    init(daemon: DaemonStore) {
        self.daemon = daemon
    }

    private var query: SearchQuery {
        SearchQuery(
            text: words,
            format: format,
            task: task,
            license: "",
            author: "",
            sort: sort,
            cursor: nil
        )
    }

    /// Search with what the fields say. Text that names a repository, or
    /// its address on huggingface.co, opens that model directly. Text that
    /// looks like an address and is not one is not sent anywhere.
    func search() async {
        switch ModelSpec.typed(text) {
        case let .model(repo):
            show(repo)
            return
        case let .refused(why):
            leaveTheList()
            notice = why
            selection = nil
            return
        case let .words(checked):
            words = checked
        }
        notice = nil
        generation += 1
        let mine = generation
        isSearching = true
        defer {
            if mine == generation {
                isSearching = false
            }
        }
        do {
            let page = try await daemon.client.search(query, at: daemon.address)
            guard mine == generation else {
                return
            }
            hits = page.models
            next = page.next
            failure = nil
            // A model opened by name stays through a search that does not
            // list it: "Show in Discover" may come before the first search.
            if let selection, selection != opened, !hits.contains(where: { $0.id == selection }) {
                self.selection = nil
            }
        } catch {
            guard mine == generation else {
                return
            }
            hits = []
            next = nil
            failure = describe(error)
        }
        hasSearched = true
    }

    /// Append the next page of the same search.
    func loadMore() async {
        guard let cursor = next, !isLoadingMore else {
            return
        }
        let mine = generation
        isLoadingMore = true
        defer { isLoadingMore = false }
        var query = query
        query.cursor = cursor
        do {
            let page = try await daemon.client.search(query, at: daemon.address)
            guard mine == generation else {
                return
            }
            let known = Set(hits.map(\.id))
            hits += page.models.filter { !known.contains($0.id) }
            next = page.next
        } catch {
            daemon.problem = describe(error)
        }
    }

    /// Show a model by name, whether the list has it or not. The screen
    /// reads its details when the selection changes.
    func open(_ repo: String) {
        selection = repo
        opened = repo
    }

    /// Show one model in place of the list, as if its name had been typed
    /// into the search field. The name is one `ModelSpec` has checked.
    func show(_ repo: String) {
        leaveTheList()
        text = repo
        open(repo)
    }

    /// Empty the list, and drop the answer of a search that is still out.
    private func leaveTheList() {
        generation += 1
        isSearching = false
        hits = []
        next = nil
        failure = nil
        notice = nil
        hasSearched = true
    }

    /// Read the selected model from Hugging Face. Quietly keeps what is on
    /// screen until the new answer is there.
    func loadDetail(quietly: Bool = false) async {
        guard let repo = selection else {
            detail = .idle
            return
        }
        detailGeneration += 1
        let mine = detailGeneration
        if !quietly || detail.value == nil {
            detail = .loading
        }
        let answer: Loaded<HubModel>
        do {
            answer = .ready(try await daemon.client.hubModel(repo, at: daemon.address))
        } catch {
            answer = .failed(describe(error))
        }
        // The owner may have selected another model, or a later read may
        // have answered, meanwhile.
        if mine == detailGeneration, selection == repo {
            detail = answer
        }
    }

    /// Queue a variant for download into the apps chosen for its kind; one
    /// whose weights are stored is only activated in them.
    func download(_ variant: Variant, into apps: [ModelApp]) async {
        let request = PullRequest(model: variant.spec, apps: apps, kind: variant.canActivate ? .activation : .download)
        let taken = await daemon.enqueue(request)
        if taken {
            await loadDetail(quietly: true)
        }
    }
}
