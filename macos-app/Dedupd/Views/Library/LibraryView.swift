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
                }
                ToolbarSpacer(.fixed)
                ToolbarItemGroup {
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
                    .inspectorColumnWidth(min: 300, ideal: 350, max: 560)
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
        FilterMenu(
            title: "Format",
            selection: $library.filterFormat,
            options: [FilterOption(value: "", title: "All formats")]
                + library.formats.map { FilterOption(value: $0, title: FormatBadge.title(for: $0)) }
        )
        .help("Show only models of one format")
        .accessibilityIdentifier("library-format")
        FilterMenu(
            title: "App",
            selection: $library.filterApp,
            options: [FilterOption(value: "", title: "All apps")]
                + ModelApp.allCases.map { FilterOption(value: $0.rawValue, title: $0.title) }
        )
        .help("Show only the models one app has")
    }

    private var table: some View {
        @Bindable var library = library
        let comfortable = settings.density == .comfortable
        return Table(library.visible, selection: $library.selection, sortOrder: $library.sortOrder) {
            // The name takes whatever width is left. The other columns have
            // one width each, as wide as what they show, so the whole table
            // fits beside the sidebar and the inspector without scrolling
            // sideways (the window's smallest width is set to match).
            TableColumn("Model", value: \.repo) { model in
                ModelNameCell(
                    name: model.name,
                    organisation: model.organisation,
                    detail: model.meta.license ?? "",
                    comfortable: comfortable
                )
                .accessibilityIdentifier("model-\(model.repo)")
            }
            .width(min: 140, ideal: 360)
            TableColumn("Format") { model in
                PrimaryFormat(meta: model.meta)
                    .frame(maxWidth: .infinity, alignment: .center)
            }
            .width(TableWidth.format)
            .alignment(.center)
            TableColumn("Size", value: \.bytes) { model in
                Text(ByteFormat.string(model.bytes))
                    .monospacedDigit()
                    .frame(maxWidth: .infinity, alignment: .trailing)
            }
            .width(TableWidth.size)
            .alignment(.trailing)
            TableColumn("Apps") { model in
                AppBadges(apps: model.appNames, none: model.isArchived ? "Archived" : "Store only")
            }
            .width(TableWidth.apps)
            TableColumn("State") { model in
                // The symbol alone; the words are in its tooltip and in the
                // inspector, so the column stays narrow beside it.
                HealthLabel(health: model.health, showsText: false)
                    .frame(maxWidth: .infinity, alignment: .center)
            }
            .width(TableWidth.state)
            .alignment(.center)
            TableColumn("Added", value: \.fetched) { model in
                Text(model.fetchedAt.map { TimeFormat.relative(TimeFormat.date(epoch: $0)) } ?? "—")
                    .foregroundStyle(.secondary)
            }
            .width(TableWidth.time)
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

/// The widths of the columns that hold one short thing, the same in every
/// table of models. Each is as wide as the longest thing it shows.
enum TableWidth {
    /// A format tag; "Safetensors" is the longest.
    static let format: CGFloat = 68
    /// "156.38 GB".
    static let size: CGFloat = 70
    /// The short names of up to three apps.
    static let apps: CGFloat = 124
    /// One symbol under the word "State".
    static let state: CGFloat = 40
    /// "45 minutes ago".
    static let time: CGFloat = 100
}

/// A model's name in a table. Compact rows show `organisation/name` on one
/// line; larger rows put the name first and who made it, with the licence,
/// on a second line.
struct ModelNameCell: View {
    let name: String
    let organisation: String
    let detail: String
    let comfortable: Bool

    var body: some View {
        Group {
            if comfortable {
                VStack(alignment: .leading, spacing: 2) {
                    Text(name)
                        .fontWeight(.medium)
                        .truncationMode(.middle)
                    Text([organisation, detail].filter { !$0.isEmpty }.joined(separator: " · "))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .truncationMode(.tail)
                }
                .padding(.vertical, 4)
            } else {
                // `organisation/name` when the column has room for all of
                // it; otherwise the model's own name, which is the part that
                // tells two rows apart. Never half an organisation.
                ViewThatFits(in: .horizontal) {
                    Text("\(Text(organisation + "/").foregroundStyle(.secondary))\(name)")
                        .fixedSize()
                    Text(name)
                        .truncationMode(.middle)
                }
                .help("\(organisation)/\(name)")
            }
        }
        .lineLimit(1)
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
