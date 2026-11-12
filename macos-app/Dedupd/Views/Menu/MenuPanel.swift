import DedupdClient
import SwiftUI

/// What a click on the menu bar icon shows: the daemon's state at a glance,
/// the downloads in flight, and the way into the main window.
struct MenuPanel: View {
    @Environment(DaemonStore.self) private var daemon
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            header
            if daemon.connection == .notRunning {
                notRunning
            } else {
                if !daemon.jobs.isEmpty {
                    MenuDownloads()
                }
                if let status = daemon.status {
                    tiles(status)
                }
                scanOutcome
                if daemon.isRunning {
                    MenuFindField { open(.discover) }
                }
            }
            Divider()
            footer
        }
        .padding(14)
        .frame(width: 340)
        .modifier(MainWindowOpener())
    }

    private var header: some View {
        HStack(spacing: 10) {
            Image(systemName: daemon.menuBarSymbol)
                .font(.title2)
                .foregroundStyle(daemon.isRunning ? Color.accentColor : .secondary)
                .frame(width: 28)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 1) {
                Text("dedupd")
                    .font(.headline)
                Text(daemon.summary)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("daemon-summary")
            }
            Spacer()
        }
    }

    private var notRunning: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("No dedupd daemon answers at \(daemon.address). Your models are untouched; the apps keep using them.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Button("Set Up the Daemon…") {
                open(.daemon)
            }
            .accessibilityIdentifier("setup-daemon")
        }
    }

    private func tiles(_ status: DaemonStatus) -> some View {
        HStack(spacing: 8) {
            MenuTile(
                title: "Store",
                value: ByteFormat.string(status.storeBytes),
                detail: ByteFormat.files(status.storeFiles)
            )
            .accessibilityIdentifier("tile-store")
            MenuTile(
                title: "Shared with apps",
                value: ByteFormat.string(status.sharedBytes),
                detail: "not stored twice"
            )
            .accessibilityIdentifier("tile-shared")
            MenuTile(
                title: "Archive",
                value: status.archivePath == nil ? "None" : (status.archiveAttached ? "Attached" : "Detached"),
                detail: status.archivePath.map { URL(fileURLWithPath: $0).lastPathComponent } ?? "no second copy"
            )
            .accessibilityIdentifier("tile-archive")
        }
    }

    @ViewBuilder private var scanOutcome: some View {
        switch daemon.scanActivity {
        case .idle:
            EmptyView()
        case .running:
            Label("Looking at the app folders…", systemImage: "arrow.triangle.2.circlepath")
                .font(.callout)
                .foregroundStyle(.secondary)
        case let .finished(result):
            Label(result.summary, systemImage: "checkmark.circle")
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityIdentifier("scan-result")
        case let .failed(message):
            FailureLabel(message: message)
        }
    }

    private var footer: some View {
        HStack(spacing: 8) {
            Button("Open dedupd") {
                open(nil)
            }
            .buttonStyle(.borderedProminent)
            .keyboardShortcut("o")
            .accessibilityIdentifier("open-main")
            Button("Scan", systemImage: "arrow.triangle.2.circlepath") {
                Task { await daemon.scan() }
            }
            .disabled(!daemon.isRunning || daemon.isScanning)
            .help("Look at the app folders now: adopt new models and share duplicate copies")
            .accessibilityIdentifier("scan-button")
            Spacer()
            SettingsLink {
                Image(systemName: "gearshape")
            }
            .help("Settings")
            .accessibilityLabel("Settings")
            Button("Quit", systemImage: "power") {
                NSApp.terminate(nil)
            }
            .labelStyle(.iconOnly)
            .keyboardShortcut("q")
            .help("Quit the app. The daemon keeps running.")
        }
    }

    private func open(_ pane: Pane?) {
        AppEnvironment.shared.openMainWindow = { openWindow(id: MainWindow.id) }
        AppEnvironment.shared.showMainWindow(pane)
    }
}

/// A field to go straight to a model: paste its address or its name, or
/// type words to search for, and Return opens the main window on Discover.
/// The text is read by `ModelSpec.typed`; only a checked `org/name` or
/// plain words go on from here, and an address that is not a model's on
/// Hugging Face goes nowhere.
private struct MenuFindField: View {
    @Environment(DiscoverStore.self) private var discover
    /// Bring the main window to the front on Discover.
    let showDiscover: () -> Void

    @State private var text = ""
    @State private var note: String?

    private var isEmpty: Bool {
        text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                TextField("Find a model", text: $text, prompt: Text("Paste a model's address, or search"))
                    .textFieldStyle(.roundedBorder)
                    .autocorrectionDisabled()
                    .onSubmit(submit)
                    .accessibilityIdentifier("panel-find")
                Button("Find", systemImage: "arrow.right.circle", action: submit)
                    .labelStyle(.iconOnly)
                    .disabled(isEmpty)
                    .help("Show this model, or search for these words, in Discover")
                    .accessibilityIdentifier("panel-find-button")
            }
            if let note {
                Text(note)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("panel-find-note")
            }
        }
        .onChange(of: text) {
            note = nil
        }
    }

    private func submit() {
        guard !isEmpty else {
            return
        }
        switch ModelSpec.typed(text) {
        case let .model(repo):
            discover.show(repo)
        case let .words(words):
            discover.text = words
            Task { await discover.search() }
        case let .refused(why):
            note = why
            return
        }
        text = ""
        showDiscover()
    }
}

/// One figure of the panel, with what it measures above and a note below.
private struct MenuTile: View {
    let title: String
    let value: String
    let detail: String

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title)
                .font(.caption2)
                .foregroundStyle(.secondary)
            Text(value)
                .font(.callout.weight(.semibold))
                .monospacedDigit()
            Text(detail)
                .font(.caption2)
                .foregroundStyle(.tertiary)
        }
        .lineLimit(1)
        .minimumScaleFactor(0.8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(8)
        .background(.quaternary.opacity(0.4), in: .rect(cornerRadius: 8))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(title), \(value), \(detail)")
    }
}

/// The downloads in flight, at most three, each with its progress.
private struct MenuDownloads: View {
    @Environment(DaemonStore.self) private var daemon

    private var shown: [Job] {
        Array((daemon.activeJobs + daemon.attentionJobs).prefix(3))
    }

    var body: some View {
        let shown = shown
        let more = daemon.activeJobs.count + daemon.attentionJobs.count - shown.count
        if !shown.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(shown) { job in
                    MenuDownloadRow(job: job)
                }
                if more > 0 {
                    Text("and \(more) more")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
    }
}

private struct MenuDownloadRow: View {
    let job: Job

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                Image(systemName: job.phase.symbol)
                    .foregroundStyle(job.phase.tint)
                Text(job.model)
                    .font(.callout)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            if let fraction = job.fraction, job.phase.isActive {
                ProgressView(value: fraction)
                    .controlSize(.small)
            }
            Text(job.phase == .downloading ? "\(job.progressText) · \(job.statusText)" : job.statusText)
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(2)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(job.model): \(job.phase.title), \(job.progressText)")
        .accessibilityIdentifier("download-progress")
    }
}
