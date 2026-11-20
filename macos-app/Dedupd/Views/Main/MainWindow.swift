import SwiftUI

/// The main window: the screens on the left, the selected screen's list in
/// the middle, and what is selected there on the right.
struct MainWindow: View {
    static let id = "main"

    @Environment(Navigation.self) private var navigation
    @Environment(DaemonStore.self) private var daemon

    var body: some View {
        NavigationSplitView {
            Sidebar()
                .navigationSplitViewColumnWidth(min: 180, ideal: 200, max: 260)
        } detail: {
            switch navigation.pane {
            case .library: LibraryView()
            case .discover: DiscoverView()
            case .favorites: FavoritesView()
            case .downloads: DownloadsView()
            case .activity: ActivityView()
            case .storage: StorageView()
            case .daemon: DaemonView()
            }
        }
        .frame(minWidth: 1150, minHeight: 560)
        .alert("That did not work", isPresented: showsProblem) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(daemon.problem ?? "")
        }
        .modifier(MainWindowOpener())
    }

    private var showsProblem: Binding<Bool> {
        Binding {
            daemon.problem != nil
        } set: { shown in
            if !shown {
                daemon.problem = nil
            }
        }
    }
}

/// The list of screens, with the daemon's state under it.
struct Sidebar: View {
    @Environment(Navigation.self) private var navigation
    @Environment(DaemonStore.self) private var daemon
    @Environment(ActivityStore.self) private var activity
    @Environment(FavoritesStore.self) private var favorites

    private var selection: Binding<Pane?> {
        Binding {
            navigation.pane
        } set: { pane in
            if let pane {
                navigation.pane = pane
            }
        }
    }

    var body: some View {
        List(selection: selection) {
            Section("Models") {
                row(.library)
                row(.discover)
                row(.favorites)
                row(.downloads, badge: daemon.activeJobs.count + daemon.attentionJobs.count)
                row(.activity, badge: activity.unseenFailures + activity.runningCount)
            }
            Section("This Mac") {
                row(.storage)
                row(.daemon)
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            SidebarStatus()
        }
        // The badge counts failures the owner has not seen, so the list is
        // kept current while another screen is shown.
        .task(id: daemon.revision) {
            await activity.reload()
            await favorites.reload()
        }
    }

    /// A screen's row. The badge goes on before the tag: a row whose tag is
    /// under a badge cannot be selected with a click.
    private func row(_ pane: Pane, badge: Int = 0) -> some View {
        Label(pane.title, systemImage: pane.symbol)
            .accessibilityIdentifier("pane-\(pane.rawValue)")
            .badge(badge)
            .tag(pane)
    }
}

/// The daemon's state in one line, at the foot of the sidebar.
private struct SidebarStatus: View {
    @Environment(DaemonStore.self) private var daemon

    private var tint: Color {
        switch daemon.connection {
        case .running: .green
        case .notRunning: .red
        case .otherBuild: .orange
        case .connecting: .secondary
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let fraction = daemon.downloadFraction {
                ProgressView(value: fraction)
                    .controlSize(.small)
            }
            HStack(spacing: 6) {
                Circle()
                    .fill(tint)
                    .frame(width: 8, height: 8)
                Text(daemon.downloadSummary ?? daemon.summary)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(daemon.downloadSummary.map { "\(daemon.summary). \($0)" } ?? daemon.summary)
        .accessibilityIdentifier("sidebar-status")
    }
}
