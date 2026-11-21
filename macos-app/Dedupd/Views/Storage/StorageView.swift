import DedupdClient
import SwiftUI

/// Where the files are on this Mac and on the archive disk, what can be
/// cleaned up, and whether the store is intact.
struct StorageView: View {
    @Environment(StorageStore.self) private var storage
    @Environment(DaemonStore.self) private var daemon

    var body: some View {
        content
            .navigationTitle(Pane.storage.title)
            .navigationSubtitle(Pane.storage.subtitle)
            .toolbar {
                ToolbarItemGroup {
                    Button("Scan", systemImage: "arrow.triangle.2.circlepath") {
                        Task { await daemon.scan() }
                    }
                    .disabled(!daemon.isRunning || daemon.isScanning)
                    .help("Look at the app folders now: adopt new models and share duplicate copies")
                    .accessibilityIdentifier("scan-button")
                }
            }
            .task(id: daemon.revision) {
                if daemon.isRunning {
                    await storage.reload()
                }
            }
            .task(id: daemon.checking) {
                await storage.refreshCheck()
            }
    }

    @ViewBuilder private var content: some View {
        if !daemon.isRunning {
            DaemonAwayView()
        } else {
            ScrollView {
                VStack(alignment: .leading, spacing: 20) {
                    scanOutcome
                    switch storage.locations {
                    case let .ready(locations):
                        VolumeCard(locations: locations)
                        LocationsCard(locations: locations)
                        StoresCard()
                        ArchivesCard()
                    case let .failed(message):
                        FailureLabel(message: message)
                    case .idle, .loading:
                        ProgressView()
                            .frame(maxWidth: .infinity)
                    }
                    CleanUpCard()
                    StoreCheckCard()
                }
                .padding(20)
                .frame(maxWidth: 900, alignment: .leading)
                .frame(maxWidth: .infinity)
            }
        }
    }

    @ViewBuilder private var scanOutcome: some View {
        switch daemon.scanActivity {
        case .idle:
            EmptyView()
        case .running:
            Label("Looking at the app folders…", systemImage: "arrow.triangle.2.circlepath")
                .foregroundStyle(.secondary)
        case let .finished(result):
            HStack {
                Label(result.summary, systemImage: "checkmark.circle")
                    .accessibilityIdentifier("scan-result")
                Spacer()
                Button("Dismiss", systemImage: "xmark") {
                    daemon.dismissScan()
                }
                .labelStyle(.iconOnly)
                .buttonStyle(.borderless)
            }
            ForEach(result.errors, id: \.self) { error in
                FailureLabel(message: error)
            }
        case let .failed(message):
            FailureLabel(message: message)
        }
    }
}

/// The internal disk: how much the models take, and how much of that the
/// apps would take again without sharing.
private struct VolumeCard: View {
    let locations: Locations

    private var apps: UInt64 { locations.appBytes }
    private var shared: UInt64 { locations.sharedBytes }
    private var used: UInt64 { locations.modelBytes }

    var body: some View {
        Card(title: "This Mac's disk") {
            let total = max(locations.volumeTotal, 1)
            GeometryReader { proxy in
                HStack(spacing: 1) {
                    Rectangle()
                        .fill(Color.accentColor)
                        .frame(width: proxy.size.width * Double(used) / Double(total))
                    Rectangle()
                        .fill(.quaternary)
                        .frame(width: proxy.size.width * Double(total - min(total, used + locations.volumeFree)) / Double(total))
                    Rectangle()
                        .fill(.quinary)
                }
            }
            .frame(height: 14)
            .clipShape(.rect(cornerRadius: 4))
            .accessibilityHidden(true)
            HStack(spacing: 24) {
                legend("Models", ByteFormat.string(used), .accentColor)
                legend("Free", ByteFormat.string(locations.volumeFree), .secondary)
                legend("Disk", ByteFormat.string(locations.volumeTotal), .secondary)
                Spacer()
            }
            if shared > 0 {
                Label(
                    "Sharing saves \(ByteFormat.string(shared)): the apps see that much, and it is on the disk once.",
                    systemImage: "square.on.square"
                )
                .font(.callout)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("sharing-saves")
            }
        }
    }

    private func legend(_ title: String, _ value: String, _ tint: Color) -> some View {
        HStack(spacing: 6) {
            Circle()
                .fill(tint)
                .frame(width: 8, height: 8)
            Text(title)
                .foregroundStyle(.secondary)
            Text(value)
                .monospacedDigit()
        }
        .font(.callout)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(title): \(value)")
    }
}

/// The store and each app's folder: where it is and what it holds. The
/// archives have a card of their own.
private struct LocationsCard: View {
    let locations: Locations

    private var all: [Location] {
        [locations.store] + locations.apps
    }

    var body: some View {
        Card(title: "Where the files are") {
            Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 16, verticalSpacing: 10) {
                GridRow {
                    Text("Place")
                    Text("Folder")
                    Text("Models").gridColumnAlignment(.trailing)
                    Text("Files").gridColumnAlignment(.trailing)
                    Text("Size").gridColumnAlignment(.trailing)
                    Text("")
                }
                .font(.caption)
                .foregroundStyle(.secondary)
                Divider()
                ForEach(all) { location in
                    LocationRow(location: location)
                }
            }
        }
    }
}

private struct LocationRow: View {
    let location: Location

    var body: some View {
        GridRow {
            Label(location.title, systemImage: location.symbol)
            VStack(alignment: .leading, spacing: 1) {
                Text(location.path)
                    .font(.callout.monospaced())
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
                Text(location.note)
                    .font(.caption)
                    .foregroundStyle(location.present ? Color.secondary : .orange)
            }
            Text("\(location.models)")
                .monospacedDigit()
            Text("\(location.files)")
                .monospacedDigit()
            Text(ByteFormat.string(location.bytes))
                .monospacedDigit()
            Button("Show in Finder", systemImage: "folder") {
                Finder.reveal(location.path)
            }
            .labelStyle(.iconOnly)
            .buttonStyle(.borderless)
            .disabled(!location.present)
            .help("Show the folder in the Finder")
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("location-\(location.id)")
    }
}
