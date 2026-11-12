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
            Text(repo.split(separator: "/").last.map(String.init) ?? repo)
                .font(.title3.weight(.semibold))
                .textSelection(.enabled)
            HStack(spacing: 6) {
                Text(meta.author ?? repo.split(separator: "/").first.map(String.init) ?? "")
                    .foregroundStyle(.secondary)
                FormatBadges(formats: meta.formats)
                Spacer()
                Link(destination: HuggingFace.page(repo)) {
                    Label("Hugging Face", systemImage: "arrow.up.right.square")
                        .font(.callout)
                }
                .help("Open the model's page on huggingface.co")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// Everything about the model selected in the library.
struct LibraryInspector: View {
    @Environment(LibraryStore.self) private var library
    @State private var tab = InspectorTab.overview

    var body: some View {
        if let model = library.selected {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    ModelHeader(repo: model.repo, meta: model.meta)
                    HealthLabel(health: model.health)
                        .font(.callout)
                    Picker("Section", selection: $tab) {
                        ForEach(InspectorTab.allCases) { tab in
                            Text(tab.rawValue).tag(tab)
                        }
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .accessibilityIdentifier("inspector-tabs")
                    tabContent(model)
                }
                .padding(16)
            }
            .accessibilityIdentifier("library-inspector")
        } else {
            ContentUnavailableView(
                "No model selected",
                systemImage: "sidebar.trailing",
                description: Text("Select a model to see where it is, which apps have it and what it is.")
            )
        }
    }

    @ViewBuilder
    private func tabContent(_ model: LibraryModel) -> some View {
        switch tab {
        case .overview:
            LibraryOverview(model: model, detail: library.detail.value)
        case .files:
            if let detail = library.detail.value {
                LibraryFiles(detail: detail)
            } else {
                loading
            }
        case .readme:
            if let readme = library.detail.value?.readme {
                MarkdownText(readme)
            } else if library.detail.value != nil {
                Text("No README was kept with this model.")
                    .foregroundStyle(.secondary)
            } else {
                loading
            }
        case .json:
            if let json = library.detail.value?.infoJson {
                RawJSONView(json: json)
            } else if library.detail.value != nil {
                Text("Hugging Face's description of this model was not kept. It is kept the next time the model is checked.")
                    .foregroundStyle(.secondary)
            } else {
                loading
            }
        }
    }

    @ViewBuilder private var loading: some View {
        if let failure = library.detail.failure {
            FailureLabel(message: failure)
        } else {
            ProgressView()
                .frame(maxWidth: .infinity)
        }
    }
}

/// Where the model is, which apps have it, and what Hugging Face says
/// about it.
private struct LibraryOverview: View {
    let model: LibraryModel
    let detail: ModelDetail?

    @Environment(LibraryStore.self) private var library
    @Environment(DaemonStore.self) private var daemon
    @Environment(DiscoverStore.self) private var discover
    @Environment(Navigation.self) private var navigation
    @State private var confirmsForget = false

    var body: some View {
        InspectorSection(title: "On this Mac") {
            Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 12, verticalSpacing: 5) {
                FactRow("Size", ByteFormat.string(model.bytes))
                FactRow("Stored", model.storedText)
                if let commit = model.shortCommit { FactRow("Revision", commit, monospaced: true) }
                if model.revisions > 1 { FactRow("Revisions kept", "\(model.revisions)") }
                if let fetched = model.fetchedAt { FactRow("Added", TimeFormat.full(TimeFormat.date(epoch: fetched))) }
                if let archived = model.archivedAt { FactRow("Archived", TimeFormat.full(TimeFormat.date(epoch: archived))) }
                if let checked = model.checkedAt { FactRow("Last checked", TimeFormat.relative(TimeFormat.date(epoch: checked))) }
                if !model.ollamaTags.isEmpty { FactRow("Ollama tags", model.ollamaTags.joined(separator: ", ")) }
            }
            if let move = library.move {
                MoveStatus(move: move)
            }
            upstream
        }
        InspectorSection(title: "Apps") {
            ForEach(ModelApp.allCases) { app in
                AppRow(app: app, presence: model.apps.first { $0.app == app.rawValue }, variants: detail?.variants ?? [])
            }
        }
        if let variants = detail?.variants, !variants.isEmpty {
            InspectorSection(title: "Variants") {
                ForEach(variants) { variant in
                    LocalVariantRow(variant: variant, archived: model.isArchived)
                }
            }
        }
        InspectorSection(title: "About") {
            MetaFacts(meta: model.meta)
            if !model.meta.plainTags.isEmpty {
                TagCloud(tags: model.meta.plainTags)
            }
        }
        Divider()
        HStack {
            Button("Show in Discover") {
                navigation.pane = .discover
                discover.open(model.repo)
            }
            .help("Look at the model on Hugging Face: its other variants and files")
            Spacer()
            if model.isArchived {
                Button("Restore") {
                    Task { await library.restore() }
                }
                .disabled(library.isMoving || daemon.status?.archiveAttached != true)
                .help(ArchiveWords.restoreHelp(daemon.status))
                .accessibilityIdentifier("restore-model")
            } else {
                Button("Archive…") {
                    Task { await library.planArchive() }
                }
                .disabled(library.isMoving || daemon.status?.archiveAttached != true)
                .help(ArchiveWords.archiveHelp(daemon.status))
                .accessibilityIdentifier("archive-model")
            }
            Button("Forget…", role: .destructive) {
                confirmsForget = true
            }
            .disabled(!model.apps.isEmpty)
            .help(model.apps.isEmpty
                ? "Drop the model from the records, so its stored files can be cleaned up"
                : "Deactivate the model in every app first")
        }
        .confirmationDialog("Forget \(model.repo)?", isPresented: $confirmsForget) {
            Button("Forget", role: .destructive) {
                Task { await library.forget() }
            }
        } message: {
            Text(
                "dedupd drops its records of the model. No file is deleted now; "
                    + "the stored files show up under Storage › Clean Up, where you decide."
            )
        }
    }

    @ViewBuilder private var upstream: some View {
        HStack(spacing: 8) {
            Button("Check Hugging Face") {
                Task { await library.checkUpstream() }
            }
            .disabled(library.isChecking)
            .help("Ask Hugging Face whether the model changed or was removed")
            if library.isChecking {
                ProgressView()
                    .controlSize(.small)
            }
        }
        if let check = library.upstream {
            Label(Self.words(for: check), systemImage: check.changed ? "arrow.down.circle" : "checkmark.circle")
                .font(.callout)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("upstream-check")
        }
    }

    private static func words(for check: UpstreamCheck) -> String {
        if check.upstream == "gone" {
            return "Hugging Face no longer has this model. Your copy is what is left of it."
        }
        if check.changed, let latest = check.latest {
            return "Hugging Face has a newer revision, \(latest.prefix(12)). Download it from Discover."
        }
        return "This is the latest revision."
    }
}

/// One app and whether it has the model, with the way to change that.
private struct AppRow: View {
    let app: ModelApp
    let presence: AppPresence?
    let variants: [Variant]

    @Environment(LibraryStore.self) private var library

    /// The variants this app can use and does not have yet.
    private var placeable: [Variant] {
        variants.filter { $0.usableApps.contains(app) && !$0.isPlaced(in: app) && $0.isStored }
    }

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: app.symbol)
                .frame(width: 20)
                .foregroundStyle(presence == nil ? .tertiary : .primary)
            VStack(alignment: .leading, spacing: 1) {
                Text(app.title)
                Text(detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            if presence != nil {
                Button("Deactivate…") {
                    Task { await library.planRemoval(from: app.rawValue) }
                }
                .controlSize(.small)
                .help("Take the model out of \(app.title). The stored copy stays.")
                .accessibilityIdentifier("remove-\(app.rawValue)")
            } else if placeable.count == 1, let variant = placeable.first {
                Button("Activate") {
                    Task { await library.place(variant, into: app) }
                }
                .controlSize(.small)
                .help(variant.placeHelp(for: app))
                .accessibilityIdentifier("place-\(app.rawValue)")
            } else if !placeable.isEmpty {
                Menu("Activate") {
                    ForEach(placeable) { variant in
                        Button(variant.name) {
                            Task { await library.place(variant, into: app) }
                        }
                    }
                }
                .controlSize(.small)
                .fixedSize()
            }
        }
        .accessibilityElement(children: .contain)
    }

    private var detail: String {
        guard let presence else {
            return variants.note(for: app)
        }
        let shared = presence.sharedBytes >= presence.bytes
            ? "shared with the store"
            : "\(ByteFormat.string(presence.bytes - presence.sharedBytes)) not shared yet"
        return "\(ByteFormat.files(presence.files)), \(ByteFormat.string(presence.bytes)), \(shared)"
    }
}

/// One way the model can be used: a quantization, or the whole repository.
private struct LocalVariantRow: View {
    let variant: Variant
    /// The model is on the archive disk only; Restore brings it back.
    let archived: Bool

    @Environment(LibraryStore.self) private var library

    var body: some View {
        HStack(spacing: 8) {
            VStack(alignment: .leading, spacing: 1) {
                Text(variant.name)
                    .font(.callout.weight(.medium))
                Text("\(ByteFormat.files(variant.files)), \(ByteFormat.string(variant.bytes)) · \(state)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            AppBadges(apps: variant.placedIn, none: "")
            if !variant.isStored, !archived {
                Button("Download") {
                    Task { await library.download(variant) }
                }
                .controlSize(.small)
                .help("Fetch the files of this variant that are not on this Mac")
            }
        }
    }

    private var state: String {
        if variant.isStored {
            return "stored"
        }
        if archived {
            return "on the archive disk"
        }
        return variant.storedBytes == 0 ? "not on this Mac" : "\(ByteFormat.string(variant.storedBytes)) stored"
    }
}
