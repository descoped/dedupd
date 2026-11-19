import SwiftUI

/// A star that marks a model as a favorite, or takes the mark off.
struct FavoriteButton: View {
    @Environment(FavoritesStore.self) private var favorites
    let repo: String

    var body: some View {
        let starred = favorites.isFavorite(repo)
        Button {
            Task { await favorites.toggle(repo) }
        } label: {
            Image(systemName: starred ? "star.fill" : "star")
                .foregroundStyle(starred ? Color.yellow : Color.secondary)
        }
        .buttonStyle(.borderless)
        .help(starred ? "Remove from Favorites" : "Add to Favorites")
        .accessibilityLabel(starred ? "Remove from Favorites" : "Add to Favorites")
        .accessibilityIdentifier("favorite")
    }
}

/// A small filled star for a favorite in a list, and nothing otherwise.
struct FavoriteMark: View {
    let isFavorite: Bool

    var body: some View {
        if isFavorite {
            Image(systemName: "star.fill")
                .foregroundStyle(.yellow)
                .imageScale(.small)
                .accessibilityLabel("Favorite")
        }
    }
}
