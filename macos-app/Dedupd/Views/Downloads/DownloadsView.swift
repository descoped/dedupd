import DedupdClient
import SwiftUI

/// The download queue: what is being fetched, what waits, what finished,
/// and how hard Hugging Face is being asked.
struct DownloadsView: View {
    @Environment(DaemonStore.self) private var daemon
    @Environment(ServiceStore.self) private var service
    @Environment(Navigation.self) private var navigation
    @State private var selection: Job.ID?
    @State private var showsInspector = true

    /// The speed limits the toolbar offers, in bytes a second; 0 is none.
    private static let limits: [UInt64] = [0, 1_000_000, 5_000_000, 10_000_000, 25_000_000, 50_000_000]

    var body: some View {
        content
            .navigationTitle(Pane.downloads.title)
            .navigationSubtitle(daemon.downloadSummary ?? Pane.downloads.subtitle)
            .toolbar {
                ToolbarItemGroup {
                    Menu("Speed Limit", systemImage: "speedometer") {
                        ForEach(Self.limits, id: \.self) { limit in
                            Toggle(limit == 0 ? "No limit" : ByteFormat.speed(limit), isOn: limitBinding(limit))
                        }
                    }
                    .disabled(!daemon.isRunning)
                    .help("The most the daemon downloads a second. It takes effect at once.")
                    Button("Clear Finished", systemImage: "checklist.checked") {
                        Task { await daemon.clearFinished() }
                    }
                    .disabled(!daemon.jobs.contains { $0.phase.isFinished })
                    .help("Take the downloads that are done or cancelled off the list")
                    .accessibilityIdentifier("clear-finished")
                    InspectorToggle(isShown: $showsInspector)
                }
            }
            .inspector(isPresented: $showsInspector) {
                DownloadInspector(job: daemon.jobs.first { $0.id == selection })
                    .inspectorColumnWidth(min: 320, ideal: 380, max: 520)
            }
    }

    private func limitBinding(_ limit: UInt64) -> Binding<Bool> {
        Binding {
            (daemon.downloads?.governor.speedLimit ?? 0) == limit
        } set: { chosen in
            if chosen {
                Task { await service.update([SettingChange(key: "speed_limit", value: .number(value: limit))]) }
            }
        }
    }

    @ViewBuilder private var content: some View {
        if !daemon.isRunning {
            DaemonAwayView()
        } else {
            VStack(spacing: 0) {
                if let downloads = daemon.downloads {
                    GovernorStrip(downloads: downloads)
                    Divider()
                }
                if daemon.jobs.isEmpty {
                    ContentUnavailableView {
                        Label("Nothing is being downloaded", systemImage: "arrow.down.circle")
                    } description: {
                        Text(
                            "Downloads start from Discover. They continue when the app is closed, "
                                + "and pick up where they stopped after a restart."
                        )
                    } actions: {
                        Button("Search Hugging Face") {
                            navigation.pane = .discover
                        }
                    }
                } else {
                    List(daemon.jobs, selection: $selection) { job in
                        DownloadRow(job: job)
                            .tag(job.id)
                    }
                    .listStyle(.inset)
                    .accessibilityIdentifier("downloads-list")
                }
            }
        }
    }
}

/// One download: what it is, how far it is, and what can be done to it.
struct DownloadRow: View {
    let job: Job

    @Environment(DaemonStore.self) private var daemon

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: job.phase.symbol)
                .font(.title3)
                .foregroundStyle(job.phase.tint)
                .frame(width: 24)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                Text(job.model)
                    .fontWeight(.medium)
                    .lineLimit(1)
                    .truncationMode(.middle)
                if job.phase.isActive || job.phase == .paused, job.phase != .queued || job.bytesDone > 0, let fraction = job.fraction {
                    ProgressView(value: fraction)
                        .controlSize(.small)
                        .tint(job.phase == .paused ? .secondary : .accentColor)
                }
                Text(line)
                    .font(.caption)
                    .foregroundStyle(job.phase == .needsAttention ? .orange : .secondary)
                    .lineLimit(2)
            }
            Spacer(minLength: 12)
            actions
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(job.model): \(line)")
        .accessibilityIdentifier("download-\(job.id)")
    }

    /// "41.94 MB of 106.07 MB · 11.80 MB/s · 5 s left".
    private var line: String {
        switch job.phase {
        case .downloading, .done:
            "\(job.progressText) · \(job.statusText)"
        case .paused where job.bytesTotal > 0:
            "Paused · \(job.progressText)"
        default:
            job.statusText
        }
    }

    @ViewBuilder private var actions: some View {
        HStack(spacing: 2) {
            switch job.phase {
            case .queued:
                button("Start Now", "forward.end.fill", .start, help: "Start this download ahead of the queue")
                button("Pause", "pause.fill", .pause, help: "Keep it in the queue without downloading")
                button("Cancel", "xmark", .cancel, help: "Stop and give up this download")
            case .resolving, .downloading, .verifying, .placing, .waitingServer, .waitingNetwork:
                button("Pause", "pause.fill", .pause, help: "Stop for now. What is downloaded is kept.")
                button("Cancel", "xmark", .cancel, help: "Stop and give up this download")
            case .paused:
                button("Resume", "play.fill", .resume, help: "Continue where it stopped")
                button("Cancel", "xmark", .cancel, help: "Give up this download")
            case .needsAttention:
                button("Try Again", "arrow.clockwise", .retry, help: "Try again from where it stopped")
                button("Cancel", "xmark", .cancel, help: "Give up this download")
            case .done, .cancelled, .unknown:
                button("Remove", "minus.circle", .remove, help: "Take it off the list. Downloaded files stay.")
            }
        }
        .buttonStyle(.borderless)
        .labelStyle(.iconOnly)
    }

    private func button(_ title: String, _ symbol: String, _ action: JobAction, help: String) -> some View {
        Button(title, systemImage: symbol) {
            Task { await daemon.act(action, on: job) }
        }
        .help(help)
        .accessibilityIdentifier("\(action.rawValue)-\(job.id)")
    }
}

/// How hard the daemon is asking Hugging Face right now, and what the
/// server allows.
struct GovernorStrip: View {
    let downloads: Downloads
    @State private var explains = false

    var body: some View {
        let governor = downloads.governor
        HStack(alignment: .top, spacing: 24) {
            figure(
                "Connections",
                "\(governor.connections) of \(governor.window)",
                "at most \(governor.maxConnections)",
                fraction: governor.window == 0 ? 0 : Double(governor.connections) / Double(governor.window)
            )
            figure(
                "Speed",
                ByteFormat.speed(downloads.speed),
                governor.speedLimit == 0 ? "no limit" : "limit \(ByteFormat.speed(governor.speedLimit))",
                fraction: governor.speedLimit == 0 ? nil : Double(downloads.speed) / Double(governor.speedLimit)
            )
            ForEach(governor.budgets, id: \.name) { budget in
                figure(
                    budget.title,
                    "\(budget.remaining) of \(budget.quota) left",
                    "more in \(ByteFormat.duration(seconds: budget.resetsIn))",
                    fraction: budget.fraction
                )
            }
            if governor.holdSeconds > 0 {
                Label("The server asked for a pause: \(ByteFormat.duration(seconds: governor.holdSeconds))", systemImage: "hourglass")
                    .font(.callout)
                    .foregroundStyle(.orange)
            }
            Spacer(minLength: 0)
            Button("How downloads are paced", systemImage: "questionmark.circle") {
                explains.toggle()
            }
            .labelStyle(.iconOnly)
            .buttonStyle(.borderless)
            .popover(isPresented: $explains, arrowEdge: .bottom) {
                Text("""
                The daemon starts with a few connections and adds one for as long as Hugging Face answers normally. \
                When the server says it is being asked too much, the daemon halves its connections and waits for as \
                long as the server asks. The server also publishes how many requests it still allows; the daemon \
                slows down before that runs out.
                """)
                .font(.callout)
                .padding(14)
                .frame(width: 340)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .background(.bar)
        .accessibilityIdentifier("governor")
    }

    private func figure(_ title: String, _ value: String, _ note: String, fraction: Double?) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(title)
                .font(.caption2)
                .foregroundStyle(.secondary)
            Text(value)
                .font(.callout.weight(.semibold))
                .monospacedDigit()
            if let fraction {
                ProgressView(value: min(1, max(0, fraction)))
                    .controlSize(.mini)
                    .frame(width: 90)
            }
            Text(note)
                .font(.caption2)
                .foregroundStyle(.tertiary)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(title): \(value), \(note)")
    }
}
