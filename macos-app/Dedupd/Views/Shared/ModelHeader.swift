import DedupdClient
import SwiftUI

/// The tabs an inspector divides a model into.
enum InspectorTab: String, CaseIterable, Identifiable {
    case overview = "Overview"
    case files = "Files"
    case readme = "README"
    case json = "JSON"

    var id: String { rawValue }
}

/// The name, maker and formats at the top of an inspector.
struct ModelHeader: View {
    let repo: String
    let meta: ModelMeta

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                FavoriteButton(repo: repo)
                Text(RepoName.name(repo))
                    .font(.title3.weight(.semibold))
                    .textSelection(.enabled)
            }
            HStack(spacing: 6) {
                Text(meta.author ?? RepoName.organisation(repo))
                    .foregroundStyle(.secondary)
                FormatBadges(formats: meta.formats)
                Spacer()
                if let page = HuggingFace.page(repo) {
                    Link(destination: page) {
                        Label("Hugging Face", systemImage: "arrow.up.right.square")
                            .font(.callout)
                    }
                    .help("Open the model's page on huggingface.co")
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
