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

/// A titled box on the Storage and Daemon screens.
struct Card<Content: View>: View {
    let title: String
    var note: String?
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.headline)
                    .accessibilityAddTraits(.isHeader)
                if let note {
                    Text(note)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            content
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.background.secondary, in: .rect(cornerRadius: 10))
        .overlay {
            RoundedRectangle(cornerRadius: 10)
                .strokeBorder(.separator)
        }
    }
}

/// The internal disk: how much the models take, and how much of that the
/// apps would take again without sharing.
private struct VolumeCard: View {
    let locations: Locations

    private var apps: UInt64 { locations.apps.map(\.bytes).reduce(0, +) }
    private var shared: UInt64 { locations.apps.map(\.sharedBytes).reduce(0, +) }
    /// What is really on the disk for models: the store, plus what the apps
    /// hold that is not a clone of it.
    private var used: UInt64 { locations.store.bytes + (apps - min(apps, shared)) }

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

/// The store, the archive and each app's folder: where it is and what it
/// holds.
private struct LocationsCard: View {
    let locations: Locations

    private var all: [Location] {
        [locations.store] + (locations.archive.map { [$0] } ?? []) + locations.apps
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
            if locations.archive == nil {
                Text("No archive disk is set. With one, every model has a second copy on another disk; set it under Daemon.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
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
                Text(note)
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

    private var note: String {
        if !location.present {
            return location.id == "archive" ? "The disk is not attached" : "The folder is not there"
        }
        switch location.id {
        case "store":
            return "What the apps share"
        case "archive":
            return "The second copy, on another disk"
        default:
            if location.bytes == 0 {
                return "Empty"
            }
            return location.sharedBytes >= location.bytes
                ? "All of it shared with the store"
                : "\(ByteFormat.string(location.bytes - location.sharedBytes)) not shared yet"
        }
    }
}
