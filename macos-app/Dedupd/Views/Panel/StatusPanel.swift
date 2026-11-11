import DedupdClient
import SwiftUI

/// The panel that opens from the menu bar icon.
struct StatusPanel: View {
    @Environment(DaemonStore.self) private var store

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            PanelHeader()
            Divider()
            content
            Divider()
            PanelFooter()
        }
        .padding(14)
        .frame(width: 400)
        .onAppear {
            store.panelVisible = true
        }
        .onDisappear {
            store.panelVisible = false
        }
        .task {
            await store.refresh()
        }
    }

    @ViewBuilder private var content: some View {
        switch store.connection {
        case .connecting:
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text("Looking for the daemon…").foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        case let .notRunning(address):
            NotRunningView(address: address, message: nil)
        case let .failed(message):
            NotRunningView(address: nil, message: message)
        case let .running(status):
            RunningView(status: status)
        }
    }
}

/// Everything the panel shows while the daemon answers.
private struct RunningView: View {
    let status: DaemonStatus

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            SummaryTiles(status: status)
            DownloadProgress(status: status)
            AppsSection(apps: status.apps)
            RepositoriesSection(repositories: status.repositories)
            PullSection()
            ActivityMessages()
        }
    }
}

private struct PanelHeader: View {
    @Environment(DaemonStore.self) private var store

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: store.menuBarSymbol)
                .font(.title2)
                .foregroundStyle(store.isRunning ? Color.accentColor : Color.secondary)
                .frame(width: 28)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 1) {
                Text("dedupd").font(.headline)
                Text(store.summary)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("daemon-summary")
            }
            Spacer()
            Button {
                Task { await store.refresh() }
            } label: {
                Image(systemName: "arrow.clockwise")
            }
            .buttonStyle(.borderless)
            .help("Ask the daemon again")
            .accessibilityLabel("Refresh")
        }
    }
}

private struct PanelFooter: View {
    @Environment(DaemonStore.self) private var store
    @Environment(\.openSettings) private var openSettings

    var body: some View {
        HStack {
            Button {
                Task { await store.scan() }
            } label: {
                Label("Scan app folders", systemImage: "arrow.triangle.2.circlepath")
            }
            .disabled(!store.isRunning || store.isScanning)
            .help("Adopt new downloads and share duplicate files now, without waiting for the next pass")
            .accessibilityIdentifier("scan-button")
            Spacer()
            Button("Settings…") {
                openSettings()
                // The app has no Dock icon, so its windows open behind the
                // frontmost app unless it is brought forward.
                NSApp.activate()
            }
            Button("Quit") {
                NSApp.terminate(nil)
            }
        }
        .controlSize(.small)
    }
}
