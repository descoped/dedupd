import DedupdClient
import SwiftUI

/// The favorite models, with the same details as Discover shows: starred in
/// dedupd, or liked on Hugging Face and brought in by the daemon.
struct FavoritesView: View {
    @Environment(DiscoverStore.self) private var discover
    @Environment(DaemonStore.self) private var daemon
    @Environment(FavoritesStore.self) private var favorites
    @State private var showsInspector = true

    /// What makes the inspector read its model again: another selection, or
    /// a change on this Mac.
    private struct DetailKey: Equatable {
        var selection: String?
        var revision: Int
    }

    var body: some View {
        content
            .navigationTitle(Pane.favorites.title)
            .navigationSubtitle(subtitle)
            .toolbar {
                ToolbarItemGroup {
                    DensityToggle()
                    InspectorToggle(isShown: $showsInspector)
                }
            }
            .inspector(isPresented: $showsInspector) {
                DiscoverInspector()
                    .inspectorColumnWidth(min: 340, ideal: 420, max: 580)
            }
            .task(id: DetailKey(selection: discover.selection, revision: daemon.revision)) {
                await discover.loadDetail(quietly: true)
            }
    }

    @ViewBuilder private var content: some View {
        if daemon.isRunning {
            FavoritesList()
        } else {
            DaemonAwayView()
        }
    }

    private var subtitle: String {
        let count = favorites.favorites.count
        return count == 0 ? Pane.favorites.subtitle : "\(count) favorite\(count == 1 ? "" : "s")"
    }
}
