import DedupdClient
import SwiftUI

/// Search Hugging Face through the daemon, and look at a model before
/// downloading it.
struct DiscoverView: View {
    @Environment(DiscoverStore.self) private var discover
    @Environment(DaemonStore.self) private var daemon
    @Environment(AppSettings.self) private var settings
    @State private var showsInspector = true

    /// What makes the inspector read its model again: another selection, or
    /// a change on this Mac.
    private struct DetailKey: Equatable {
        var selection: String?
        var revision: Int
    }

    var body: some View {
        @Bindable var discover = discover
        content
            .navigationTitle(Pane.discover.title)
            .navigationSubtitle(subtitle)
            .searchable(text: $discover.text, placement: .toolbar, prompt: "Search, or paste a model's name or address")
            .onSubmit(of: .search) {
                Task { await discover.search() }
            }
            .toolbar {
                ToolbarItemGroup {
                    filters
                }
                ToolbarSpacer(.fixed)
                ToolbarItemGroup {
                    DensityToggle()
                    InspectorToggle(isShown: $showsInspector)
                }
            }
            .inspector(isPresented: $showsInspector) {
                DiscoverInspector()
                    .inspectorColumnWidth(min: 340, ideal: 420, max: 580)
            }
            .task(id: daemon.isRunning) {
                if daemon.isRunning, !discover.hasSearched {
                    await discover.search()
                }
            }
            .task(id: DetailKey(selection: discover.selection, revision: daemon.revision)) {
                await discover.loadDetail(quietly: true)
            }
            .onChange(of: [discover.format, discover.task, discover.sort.rawValue]) {
                Task { await discover.search() }
            }
            .onChange(of: discover.text) { _, text in
                // Clearing the field goes back to the list without a Return.
                if text.isEmpty, discover.hasSearched {
                    Task { await discover.search() }
                }
            }
    }

    private var subtitle: String {
        if discover.isSearching {
            return "Searching…"
        }
        if discover.hasSearched, !discover.hits.isEmpty {
            return "\(discover.hits.count) model\(discover.hits.count == 1 ? "" : "s")\(discover.next == nil ? "" : ", more to load")"
        }
        return Pane.discover.subtitle
    }

    @ViewBuilder private var filters: some View {
        @Bindable var discover = discover
        FilterMenu(
            title: "Format",
            selection: $discover.format,
            options: DiscoverStore.formats.map {
                FilterOption(value: $0, title: $0.isEmpty ? "Any format" : FormatBadge.title(for: $0))
            }
        )
        .help("Show only models that come in this format")
        .accessibilityIdentifier("discover-format")
        FilterMenu(
            title: "Task",
            selection: $discover.task,
            options: DiscoverStore.tasks.map { FilterOption(value: $0, title: $0.isEmpty ? "Any task" : $0) }
        )
        .help("Show only models made for this task")
        FilterMenu(
            title: "Order",
            selection: $discover.sort,
            options: SearchSort.allCases.map { FilterOption(value: $0, title: $0.title) }
        )
        .help("The order Hugging Face lists the models in")
    }

    @ViewBuilder private var content: some View {
        if !daemon.isRunning {
            DaemonAwayView()
        } else if let failure = discover.failure {
            ContentUnavailableView {
                Label("Hugging Face could not be searched", systemImage: "wifi.exclamationmark")
            } description: {
                Text(failure)
            } actions: {
                Button("Try Again") {
                    Task { await discover.search() }
                }
            }
        } else if discover.hits.isEmpty, discover.selection != nil {
            ContentUnavailableView(
                "One model, by name",
                systemImage: "arrow.right.circle",
                description: Text("The inspector shows \(discover.selection ?? ""). Clear the search field to list models again.")
            )
        } else if discover.hits.isEmpty, discover.hasSearched, !discover.isSearching {
            ContentUnavailableView.search(text: discover.text)
        } else if discover.hits.isEmpty {
            ProgressView("Asking Hugging Face…")
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            table
        }
    }

    private var table: some View {
        @Bindable var discover = discover
        let comfortable = settings.density == .comfortable
        return Table(discover.hits, selection: $discover.selection) {
            TableColumn("Model") { hit in
                ModelNameCell(
                    name: hit.name,
                    organisation: hit.organisation,
                    detail: [hit.meta.pipelineTag, hit.meta.license].compactMap { $0 }.joined(separator: " · "),
                    comfortable: comfortable
                )
                .accessibilityIdentifier("hit-\(hit.id)")
            }
            .width(min: 180, ideal: 320)
            TableColumn("On this Mac") { hit in
                HStack(spacing: 4) {
                    if hit.downloading {
                        Label("Downloading", systemImage: "arrow.down.circle")
                            .foregroundStyle(Color.accentColor)
                    } else if !hit.apps.isEmpty {
                        AppBadges(apps: hit.apps)
                    } else if hit.archived {
                        Tag(text: "Store")
                    }
                }
                .help(hit.localState ?? "Not on this Mac")
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(hit.localState ?? "Not on this Mac")
            }
            .width(min: 112, ideal: 130, max: 220)
            TableColumn("Format") { hit in
                PrimaryFormat(meta: hit.meta)
                    .frame(maxWidth: .infinity, alignment: .center)
            }
            .width(TableWidth.format)
            .alignment(.center)
            TableColumn("Params") { hit in
                Text(hit.meta.parameters.map(ByteFormat.parameters) ?? "—")
                    .monospacedDigit()
                    .frame(maxWidth: .infinity, alignment: .trailing)
            }
            .width(min: 50, ideal: 64, max: 100)
            .alignment(.trailing)
            TableColumn("Downloads") { hit in
                Text(hit.meta.downloads.map(ByteFormat.count) ?? "—")
                    .monospacedDigit()
                    .frame(maxWidth: .infinity, alignment: .trailing)
            }
            .width(min: 80, ideal: 88, max: 110)
            .alignment(.trailing)
            TableColumn("Likes") { hit in
                Text(hit.meta.likes.map(ByteFormat.count) ?? "—")
                    .monospacedDigit()
                    .frame(maxWidth: .infinity, alignment: .trailing)
            }
            .width(min: 40, ideal: 50, max: 80)
            .alignment(.trailing)
            TableColumn("Updated") { hit in
                Text(hit.meta.modified.map { TimeFormat.relative($0) } ?? "—")
                    .foregroundStyle(.secondary)
            }
            .width(min: 80, ideal: 100, max: 140)
        }
        .contextMenu(forSelectionType: SearchHit.ID.self) { ids in
            if let repo = ids.first {
                Button("Copy Name") {
                    Pasteboard.copy(repo)
                }
                Link("Show on Hugging Face", destination: HuggingFace.page(repo))
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if discover.next != nil {
                HStack {
                    Spacer()
                    Button(discover.isLoadingMore ? "Loading…" : "Load More") {
                        Task { await discover.loadMore() }
                    }
                    .disabled(discover.isLoadingMore)
                    .accessibilityIdentifier("load-more")
                    Spacer()
                }
                .padding(8)
                .background(.bar)
            }
        }
        .accessibilityIdentifier("discover-table")
    }
}
