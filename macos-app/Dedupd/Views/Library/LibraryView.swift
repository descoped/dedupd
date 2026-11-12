import DedupdClient
import SwiftUI

/// The models on this Mac: one row each, with where it is and whether it
/// still matches Hugging Face.
struct LibraryView: View {
    @Environment(LibraryStore.self) private var library
    @Environment(DaemonStore.self) private var daemon
    @Environment(AppSettings.self) private var settings
    @Environment(Navigation.self) private var navigation
    @State private var showsInspector = true

    var body: some View {
        @Bindable var library = library
        content
            .navigationTitle(Pane.library.title)
            .navigationSubtitle(library.hasLoaded ? library.countText : Pane.library.subtitle)
            .searchable(text: $library.filterText, placement: .toolbar, prompt: "Filter by name")
            .toolbar {
                ToolbarItemGroup {
                    filters
                    DensityToggle()
                    Button("Scan", systemImage: "arrow.triangle.2.circlepath") {
                        Task { await daemon.scan() }
                    }
                    .disabled(!daemon.isRunning || daemon.isScanning)
                    .help("Look at the app folders now: adopt new models and share duplicate copies")
                    InspectorToggle(isShown: $showsInspector)
                }
            }
            .inspector(isPresented: $showsInspector) {
                LibraryInspector()
                    .inspectorColumnWidth(min: 320, ideal: 400, max: 560)
            }
            .sheet(isPresented: showsRemoval) {
                if let plan = library.removal {
                    RemovalSheet(plan: plan)
                }
            }
            .sheet(isPresented: showsArchive) {
                if let plan = library.archiving {
                    ArchiveSheet(plan: plan)
                }
            }
            .task(id: daemon.revision) {
                await library.reload()
            }
            .task(id: library.selection) {
                await library.loadDetail()
            }
    }

    private var showsRemoval: Binding<Bool> {
        Binding {
            library.removal != nil
        } set: { shown in
            if !shown {
                library.removal = nil
            }
        }
    }

    private var showsArchive: Binding<Bool> {
        Binding {
            library.archiving != nil
        } set: { shown in
            if !shown {
                library.archiving = nil
            }
        }
    }

    @ViewBuilder private var content: some View {
        if !daemon.isRunning, library.models.isEmpty {
            DaemonAwayView()
        } else if let failure = library.failure, library.models.isEmpty {
            ContentUnavailableView(
                "The library could not be read", systemImage: "exclamationmark.triangle", description: Text(failure)
            )
        } else if library.hasLoaded, library.models.isEmpty {
            ContentUnavailableView {
                Label("No models yet", systemImage: "books.vertical")
            } description: {
                Text(
                    "Models that Ollama, Bionic or a Hugging Face tool already have appear here after a scan. "
                        + "New ones come from Discover."
                )
            } actions: {
                Button("Search Hugging Face") {
                    navigation.pane = .discover
                }
                Button("Scan the App Folders") {
                    Task { await daemon.scan() }
                }
            }
        } else {
            table
        }
    }

    @ViewBuilder private var filters: some View {
        @Bindable var library = library
        Picker("Format", selection: $library.filterFormat) {
            Text("All formats").tag("")
            ForEach(library.formats, id: \.self) { format in
                Text(FormatBadge.title(for: format)).tag(format)
            }
        }
        .help("Show only models of one format")
        .accessibilityIdentifier("library-format")
        Picker("App", selection: $library.filterApp) {
            Text("All apps").tag("")
            ForEach(ModelApp.allCases) { app in
                Text(app.title).tag(app.rawValue)
            }
        }
        .help("Show only the models one app has")
    }

    private var table: some View {
        @Bindable var library = library
        let comfortable = settings.density == .comfortable
        return Table(library.visible, selection: $library.selection, sortOrder: $library.sortOrder) {
            TableColumn("Model", value: \.repo) { model in
                ModelNameCell(
                    name: model.name,
                    organisation: model.organisation,
                    detail: model.summary,
                    comfortable: comfortable
                )
                .accessibilityIdentifier("model-\(model.repo)")
            }
            .width(min: 180, ideal: 320)
            TableColumn("Format") { model in
                PrimaryFormat(meta: model.meta)
            }
            .width(min: 84, ideal: 90, max: 110)
            TableColumn("Size", value: \.bytes) { model in
                Text(ByteFormat.string(model.bytes))
                    .monospacedDigit()
            }
            .width(min: 76, ideal: 84, max: 110)
            TableColumn("Apps") { model in
                AppBadges(apps: model.appNames, none: model.isArchived ? "Archived" : "Store only")
            }
            .width(min: 128, ideal: 136, max: 200)
            TableColumn("State") { model in
                // The symbol alone; the words are in its tooltip and in the
                // inspector, so the column stays narrow beside it.
                HealthLabel(health: model.health, showsText: false)
            }
            .width(min: 40, ideal: 44, max: 60)
            TableColumn("Added", value: \.fetched) { model in
                Text(model.fetchedAt.map { TimeFormat.relative(TimeFormat.date(epoch: $0)) } ?? "—")
                    .foregroundStyle(.secondary)
            }
            .width(min: 80, ideal: 100, max: 140)
        }
        .contextMenu(forSelectionType: LibraryModel.ID.self) { ids in
            if let model = library.models.first(where: { ids.contains($0.id) }) {
                Button("Copy Name") {
                    Pasteboard.copy(model.repo)
                }
                Link("Show on Hugging Face", destination: HuggingFace.page(model.repo))
            }
        }
        .accessibilityIdentifier("library-table")
    }
}

/// A model's name in a table. Compact rows show the name alone; larger rows
/// put who made it and what it is on a second line.
struct ModelNameCell: View {
    let name: String
    let organisation: String
    let detail: String
    let comfortable: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            if comfortable {
                Text(name)
                    .fontWeight(.medium)
                Text([organisation, detail].filter { !$0.isEmpty }.joined(separator: " · "))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .truncationMode(.tail)
            } else {
                Text("\(Text(organisation + "/").foregroundStyle(.secondary))\(name)")
            }
        }
        .lineLimit(1)
        .truncationMode(.middle)
        .padding(.vertical, comfortable ? 4 : 0)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(comfortable && !detail.isEmpty ? "\(organisation)/\(name), \(detail)" : "\(organisation)/\(name)")
    }
}

/// Addresses on huggingface.co.
enum HuggingFace {
    static func page(_ repo: String) -> URL {
        URL(string: "https://huggingface.co/\(repo)") ?? URL(fileURLWithPath: "/")
    }
}
