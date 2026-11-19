import DedupdClient
import Foundation
import Observation

/// The favorite models: starred in dedupd, or liked on Hugging Face and
/// brought in by the daemon. Shared by Discover and the Library.
@MainActor
@Observable
final class FavoritesStore {
    private(set) var list = Favorites(
        favorites: [],
        likes: LikesSync(hasToken: false, user: nil, syncedAt: nil, problem: nil)
    )
    private(set) var isSyncing = false

    @ObservationIgnored private let daemon: DaemonStore

    init(daemon: DaemonStore) {
        self.daemon = daemon
    }

    var favorites: [Favorite] { list.favorites }

    func isFavorite(_ repo: String) -> Bool {
        list.favorites.contains { $0.repo == repo }
    }

    /// Read the list again. Quiet on failure: the daemon being away is
    /// shown elsewhere.
    func reload() async {
        let address = daemon.address
        guard daemon.isRunning, let answer = try? await daemon.client.favorites(at: address),
              address == daemon.address
        else {
            return
        }
        list = answer
    }

    /// Star a model, or take the star off.
    func toggle(_ repo: String) async {
        let favorite = !isFavorite(repo)
        do {
            list = try await daemon.client.setFavorite(repo, favorite, at: daemon.address)
        } catch {
            daemon.problem = describe(error)
        }
    }

    /// Read the likes on Hugging Face now.
    func sync() async {
        isSyncing = true
        defer { isSyncing = false }
        do {
            list = try await daemon.client.syncFavorites(at: daemon.address)
        } catch {
            daemon.problem = describe(error)
        }
    }
}

extension LikesSync {
    /// Where the likes stand, in a sentence.
    var summary: String {
        if !hasToken {
            return "To bring in your likes from Hugging Face, sign in there with `hf auth login`."
        }
        if let problem {
            return problem
        }
        guard let user, let syncedAt else {
            return "Your likes on Hugging Face have not been read yet."
        }
        let when = TimeFormat.relative(TimeFormat.date(epoch: syncedAt))
        return "Likes of \(user) on Hugging Face, read \(when)."
    }
}

extension Favorite {
    var name: String { repo.split(separator: "/").last.map(String.init) ?? repo }
    var organisation: String { repo.split(separator: "/").first.map(String.init) ?? "" }

    /// Where it came from, in words.
    var from: String {
        switch source {
        case .dedupd: "Starred"
        case .huggingFace: "Liked on Hugging Face"
        }
    }
}
