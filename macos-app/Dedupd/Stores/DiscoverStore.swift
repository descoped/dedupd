import DedupdClient
import Foundation
import Observation

/// The orders Hugging Face can list models in.
enum SearchSort: String, CaseIterable, Identifiable, Sendable {
    case trending = "trendingScore"
    case downloads
    case likes
    case modified = "lastModified"
    case created = "createdAt"

    var id: String { rawValue }

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

    var selection: SearchHit.ID? {
        didSet {
            if selection != oldValue {
                detail = .idle
            }
        }
    }

    private(set) var detail: Loaded<HubModel> = .idle

    @ObservationIgnored private let daemon: DaemonStore
    @ObservationIgnored private var generation = 0

    init(daemon: DaemonStore) {
        self.daemon = daemon
    }

    private var query: SearchQuery {
        SearchQuery(
            text: text.trimmingCharacters(in: .whitespaces),
            format: format,
            task: task,
            license: "",
            author: "",
            sort: sort.rawValue,
            cursor: nil
        )
    }

    /// Search with what the fields say. Text that names a repository, or
    /// its address on huggingface.co, opens that model directly.
    func search() async {
        if let repo = ModelSpec.repository(in: text) {
            hits = []
            next = nil
            failure = nil
            hasSearched = true
            open(repo)
            return
        }
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
            if let selection, !hits.contains(where: { $0.id == selection }) {
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
    }

    /// Read the selected model from Hugging Face. Quietly keeps what is on
    /// screen until the new answer is there.
    func loadDetail(quietly: Bool = false) async {
        guard let repo = selection else {
            detail = .idle
            return
        }
        if !quietly || detail.value == nil {
            detail = .loading
        }
        let answer: Loaded<HubModel>
        do {
            answer = .ready(try await daemon.client.hubModel(repo, at: daemon.address))
        } catch {
            answer = .failed(describe(error))
        }
        if selection == repo {
            detail = answer
        }
    }

    /// Queue a variant for download into the apps chosen for its kind.
    func download(_ variant: Variant, into apps: [ModelApp]) async {
        let taken = await daemon.enqueue(PullRequest(model: variant.spec, apps: apps.map(\.rawValue)))
        if taken {
            await loadDetail(quietly: true)
        }
    }
}
