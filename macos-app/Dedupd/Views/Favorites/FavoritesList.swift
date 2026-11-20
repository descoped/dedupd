import DedupdClient
import SwiftUI

/// The favorite models, in place of the search results: starred in dedupd,
/// or liked on Hugging Face and brought in by the daemon.
struct FavoritesList: View {
    @Environment(DiscoverStore.self) private var discover
    @Environment(FavoritesStore.self) private var favorites
    @Environment(AppSettings.self) private var settings

    var body: some View {
        @Bindable var discover = discover
        let comfortable = settings.density == .comfortable
        Group {
            if favorites.favorites.isEmpty {
                ContentUnavailableView(
                    "No favorites yet",
                    systemImage: "star",
                    description: Text("Star a model in its details to keep it here. Models you like on Hugging Face are added too.")
                )
            } else {
                Table(favorites.favorites, selection: $discover.selection) {
                    TableColumn("Model") { favorite in
                        ModelNameCell(
                            name: favorite.name,
                            organisation: favorite.organisation,
                            detail: favorite.from,
                            comfortable: comfortable
                        )
                        .accessibilityIdentifier("favorite-\(favorite.repo)")
                    }
                    .width(min: 180, ideal: 360)
                    TableColumn("From") { favorite in
                        Label(
                            favorite.from,
                            systemImage: favorite.source == .huggingFace ? "heart" : "star"
                        )
                        .foregroundStyle(.secondary)
                    }
                    .width(min: 120, ideal: 170, max: 220)
                    TableColumn("Added") { favorite in
                        Text(TimeFormat.relative(TimeFormat.date(epoch: favorite.addedAt)))
                            .foregroundStyle(.secondary)
                    }
                    .width(min: 80, ideal: 110, max: 140)
                }
                .contextMenu(forSelectionType: String.self) { ids in
                    if let repo = ids.first {
                        Button("Remove from Favorites") {
                            Task { await favorites.set(repo, favorite: false) }
                        }
                        Button("Copy Name") {
                            Pasteboard.copy(repo)
                        }
                        if let page = HuggingFace.page(repo) {
                            Link("Show on Hugging Face", destination: page)
                        }
                    }
                }
                .accessibilityIdentifier("favorites-table")
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            HStack {
                // The daemon's words, never read as Markdown or a format.
                Text(verbatim: favorites.list.likes.summary)
                    .font(.callout)
                    .foregroundStyle(favorites.list.likes.problem == nil ? Color.secondary : Color.orange)
                    .lineLimit(2)
                Spacer()
                if favorites.list.likes.hasToken {
                    Button(favorites.isSyncing ? "Reading…" : "Sync Now") {
                        Task { await favorites.sync() }
                    }
                    .disabled(favorites.isSyncing)
                    .help("Read your likes on Hugging Face now")
                    .accessibilityIdentifier("favorites-sync")
                }
            }
            .padding(8)
            .background(.bar)
        }
    }
}
