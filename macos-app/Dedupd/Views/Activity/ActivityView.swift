import DedupdClient
import SwiftUI

/// What happened to the models in the apps: activations that are running,
/// then each activation, deactivation, archive and restore with its result.
/// Double-click a line to open the model in the Library.
struct ActivityView: View {
    @Environment(DaemonStore.self) private var daemon
    @Environment(ActivityStore.self) private var activity
    @Environment(LibraryStore.self) private var library
    @Environment(Navigation.self) private var navigation
    @State private var selection: ActivityLine?

    var body: some View {
        // The sidebar keeps the list current; this screen only shows it.
        content
            .navigationTitle(Pane.activity.title)
            .navigationSubtitle(Pane.activity.subtitle)
            // Seen once it is on the screen, also what arrives while it is.
            .onChange(of: activity.entries.first?.id, initial: true) {
                activity.markSeen()
            }
    }

    @ViewBuilder private var content: some View {
        if !daemon.isRunning {
            DaemonAwayView()
        } else if activity.entries.isEmpty, activity.running.isEmpty {
            ContentUnavailableView {
                Label("Nothing has happened yet", systemImage: Pane.activity.symbol)
            } description: {
                Text("When a model is activated in an app, taken out of one, archived or restored, it is listed here.")
            } actions: {
                Button("Open the Library") {
                    navigation.pane = .library
                }
            }
        } else {
            List(selection: $selection) {
                if !activity.running.isEmpty {
                    Section("Now") {
                        ForEach(activity.running) { job in
                            RunningActivationRow(job: job)
                                .tag(ActivityLine.job(job.id))
                        }
                    }
                }
                Section(activity.running.isEmpty ? "" : "Earlier") {
                    ForEach(activity.entries, id: \.id) { entry in
                        ActivityRow(entry: entry)
                            .tag(ActivityLine.entry(entry.id))
                    }
                }
            }
            .listStyle(.inset)
            .contextMenu(forSelectionType: ActivityLine.self) { lines in
                if let repo = lines.first.flatMap(activity.repo(of:)) {
                    Button("Show in Library") { show(repo) }
                }
            } primaryAction: { lines in
                if let repo = lines.first.flatMap(activity.repo(of:)) {
                    show(repo)
                }
            }
            .accessibilityIdentifier("activity-list")
        }
    }

    private func show(_ repo: String) {
        library.selection = repo
        navigation.pane = .library
    }
}

/// One thing that happened: what, to which model, and when.
struct ActivityRow: View {
    let entry: ActivityEntry

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: entry.ok ? entry.symbol : "exclamationmark.circle.fill")
                .font(.title3)
                .foregroundStyle(entry.ok ? Color.secondary : Color.orange)
                .frame(width: 24)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                Text(entry.repo)
                    .fontWeight(.medium)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text(entry.ok ? entry.text : "\(entry.failureTitle). \(entry.text)")
                    .font(.caption)
                    .foregroundStyle(entry.ok ? .secondary : Color.orange)
                    .lineLimit(3)
            }
            Spacer(minLength: 12)
            Text(TimeFormat.relative(TimeFormat.date(epoch: entry.at)))
                .font(.caption)
                .foregroundStyle(.secondary)
                .help(TimeFormat.full(TimeFormat.date(epoch: entry.at)))
        }
        .padding(.vertical, 3)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(entry.repo): \(entry.ok ? entry.text : "\(entry.failureTitle). \(entry.text)")")
        .accessibilityIdentifier("activity-\(entry.id)")
    }
}

/// An activation the daemon is working on, or one that waits for the owner.
private struct RunningActivationRow: View {
    let job: Job

    var body: some View {
        HStack(spacing: 10) {
            Group {
                if job.state == .needsAttention {
                    Image(systemName: job.state.symbol)
                        .foregroundStyle(job.state.tint)
                } else {
                    ProgressView()
                        .controlSize(.small)
                }
            }
            .font(.title3)
            .frame(width: 24)
            VStack(alignment: .leading, spacing: 3) {
                Text(job.repo)
                    .fontWeight(.medium)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text(job.state == .needsAttention ? job.statusText : "Activating in \(job.appList)…")
                    .font(.caption)
                    .foregroundStyle(job.state == .needsAttention ? Color.orange : .secondary)
                    .lineLimit(3)
            }
            Spacer(minLength: 12)
            if job.state == .needsAttention {
                JobActionButtons(job: job)
            }
        }
        .padding(.vertical, 3)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("activating-\(job.id)")
    }
}

extension ActivityEntry {
    /// The symbol for what was done.
    var symbol: String {
        switch action {
        case .activate: "plus.circle"
        case .deactivate: "minus.circle"
        case .archive: "archivebox"
        case .restore: "arrow.uturn.backward.circle"
        }
    }

    /// What did not happen, in front of the reason: "Not activated in Ollama".
    var failureTitle: String {
        let app = app.map { " in \($0.title)" } ?? ""
        return switch action {
        case .activate: "Not activated\(app)"
        case .deactivate: "Not deactivated\(app)"
        case .archive: "Not archived"
        case .restore: "Not restored"
        }
    }
}
