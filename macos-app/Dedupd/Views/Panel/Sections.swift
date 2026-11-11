import DedupdClient
import SwiftUI

/// A small heading above a group of rows.
struct SectionTitle: View {
    let text: String

    var body: some View {
        Text(text)
            .font(.caption.weight(.semibold))
            .foregroundStyle(.secondary)
            .textCase(.uppercase)
            .accessibilityAddTraits(.isHeader)
    }
}

/// How many files of each app the store covers.
struct AppsSection: View {
    let apps: [AppUsage]

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            SectionTitle(text: "App folders")
            if apps.isEmpty {
                Text("No app holds a model the store knows about yet.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            } else {
                ForEach(apps, id: \.app) { usage in
                    HStack(spacing: 8) {
                        Image(systemName: ModelApp.symbol(for: usage.app))
                            .frame(width: 18)
                            .foregroundStyle(.secondary)
                            .accessibilityHidden(true)
                        Text(ModelApp.title(for: usage.app))
                        Spacer()
                        Text(ByteFormat.files(usage.files))
                            .foregroundStyle(.secondary)
                        Text(ByteFormat.string(usage.bytes))
                            .frame(minWidth: 76, alignment: .trailing)
                    }
                    .font(.callout)
                    .monospacedDigit()
                    .accessibilityElement(children: .combine)
                }
            }
        }
    }
}

/// The models the daemon has archived.
struct RepositoriesSection: View {
    let repositories: [Repository]

    /// How many models fit before the list starts to scroll.
    private static let visibleRows = 5

    private var rows: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(repositories, id: \.self) { repository in
                RepositoryRow(repository: repository)
            }
        }
        .accessibilityIdentifier("repository-list")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            SectionTitle(text: "Models (\(repositories.count))")
            if repositories.isEmpty {
                Text("Nothing is archived yet. Fetch a model below, or let the daemon adopt what the apps already hold.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                // A scroll view has no height of its own, and the panel takes
                // its height from its content, so a short list is laid out
                // plainly and only a long one scrolls, at a fixed height.
                if repositories.count <= Self.visibleRows {
                    rows
                } else {
                    ScrollView {
                        rows
                    }
                    .frame(height: 190)
                }
            }
        }
    }
}

private struct RepositoryRow: View {
    let repository: Repository

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: repository.health.symbol)
                .frame(width: 18)
                .foregroundStyle(tint)
                .help(repository.health.label)
                .accessibilityLabel(repository.health.label)
            VStack(alignment: .leading, spacing: 1) {
                Text(repository.name)
                    .font(.callout)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .help(repository.name)
                Text(repository.detail)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .help(repository.detail)
            }
            Spacer(minLength: 4)
            Text(ByteFormat.string(repository.storedBytes))
                .font(.callout)
                .monospacedDigit()
        }
        // Spelled out rather than combined from the children: with tooltips
        // on them the combined element reads as nothing.
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(
            "\(repository.name), \(ByteFormat.string(repository.storedBytes)), \(repository.detail). \(repository.health.label)"
        )
    }

    private var tint: Color {
        switch repository.health {
        case .verified: .green
        case .different: .orange
        case .unconfirmed, .storedOnly: .secondary
        }
    }
}
