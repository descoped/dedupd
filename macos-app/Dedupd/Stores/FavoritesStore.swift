import DedupdClient
import Foundation
import Observation

/// The favorite models: starred in dedupd, or liked on Hugging Face and
/// brought in by the daemon. Shared by Discover and the Library.
@MainActor
@Observable
final class FavoritesStore {
    private(set) var list = nothing
    private(set) var isSyncing = false

    @ObservationIgnored private let daemon: DaemonStore

    init(daemon: DaemonStore) {
        self.daemon = daemon
        daemon.whenAddressChanges { [weak self] in self?.list = Self.nothing }
    }

    private static let nothing = Favorites(
        favorites: [],
        likes: LikesSync(hasToken: false, user: nil, syncedAt: nil, problem: nil)
    )

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
        await set(repo, favorite: !isFavorite(repo))
    }

    /// Star a model (`true`) or take the star off, whatever the list on
    /// screen says: a menu item that says "Remove" only ever removes.
    func set(_ repo: String, favorite: Bool) async {
        let address = daemon.address
        do {
            let answer = try await daemon.client.setFavorite(repo, favorite, at: address)
            if address == daemon.address {
                list = answer
            }
        } catch {
            daemon.problem = describe(error)
        }
    }

    /// Read the likes on Hugging Face now.
    func sync() async {
        isSyncing = true
        defer { isSyncing = false }
        let address = daemon.address
        do {
            let answer = try await daemon.client.syncFavorites(at: address)
            if address == daemon.address {
                list = answer
            }
        } catch {
            daemon.problem = describe(error)
        }
    }
}
