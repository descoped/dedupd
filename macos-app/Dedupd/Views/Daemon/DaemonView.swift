import DedupdClient
import SwiftUI

/// The background service: whether it runs, the launchd agent that keeps
/// it running, its settings and its log.
struct DaemonView: View {
    @Environment(DaemonStore.self) private var daemon
    @Environment(ServiceStore.self) private var service

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                DaemonStateCard()
                AgentCard()
                if daemon.isRunning {
                    DaemonSettingsCard()
                    LogCard()
                }
            }
            .padding(20)
            .frame(maxWidth: 900, alignment: .leading)
            .frame(maxWidth: .infinity)
        }
        .navigationTitle(Pane.daemon.title)
        .navigationSubtitle(daemon.summary)
        .task(id: daemon.connection) {
            await service.reload()
        }
    }
}

/// Whether the daemon runs, and what it says about itself.
private struct DaemonStateCard: View {
    @Environment(DaemonStore.self) private var daemon
    @Environment(ServiceStore.self) private var service

    var body: some View {
        Card(title: "The daemon") {
            HStack(spacing: 8) {
                Image(systemName: daemon.isRunning ? "checkmark.circle.fill" : "xmark.circle.fill")
                    .foregroundStyle(daemon.isRunning ? .green : .red)
                Text(daemon.summary)
                    .font(.title3.weight(.medium))
                    .accessibilityIdentifier("daemon-state")
                Spacer()
                if daemon.isRunning {
                    Button("Restart") {
                        Task { await service.restart() }
                    }
                    .help("Start the daemon again. Downloads continue where they stopped.")
                    .accessibilityIdentifier("restart-daemon")
                }
            }
            if let status = daemon.status, daemon.isRunning {
                let info = status.daemon
                Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 12, verticalSpacing: 5) {
                    FactRow("Version", status.version)
                    FactRow("Process", "\(info.pid)", monospaced: true)
                    FactRow("Running since", TimeFormat.full(TimeFormat.date(epoch: info.startedAt)))
                    FactRow("Listens on", info.listen, monospaced: true)
                    FactRow("Kept running by", info.managedBy.map { "launchd, as \($0)" } ?? "Nothing: it was started by hand")
                    if let executable = info.executable { FactRow("Program", executable, monospaced: true) }
                    FactRow("Home folder", status.home, monospaced: true)
                    FactRow("Fetches from", status.endpoint)
                    FactRow("Hugging Face token", token(info))
                }
            } else if daemon.connection == .notRunning {
                Text(
                    "No daemon answers at \(daemon.address). Your models are untouched and the apps keep using them; "
                        + "nothing new is shared or backed up until the daemon runs."
                )
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private func token(_ info: DaemonInfo) -> String {
        guard info.token else {
            return "None. Gated models cannot be downloaded; run `hf auth login` to add one."
        }
        return info.tokenSource.map { "Found in \($0)" } ?? "Found"
    }
}

/// Installing, updating and removing the launchd agent.
private struct AgentCard: View {
    @Environment(DaemonStore.self) private var daemon
    @Environment(ServiceStore.self) private var service
    @State private var setsEnvironment = false
    @State private var confirmsUninstall = false

    var body: some View {
        Card(
            title: "Background service",
            note: "A launchd agent starts the daemon at login and keeps it running, also when this app is closed. "
                + "macOS lists it as dedupd under System Settings › General › Login Items & Extensions, "
                + "where it can also be switched off."
        ) {
            switch service.agent {
            case let .ready(agent):
                state(agent)
            case let .failed(message):
                FailureLabel(message: message)
            case .idle, .loading:
                ProgressView()
                    .controlSize(.small)
            }
            if let output = service.lastOutput, !output.isEmpty {
                Text(output)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
                    .accessibilityIdentifier("agent-output")
            }
            Divider()
            HStack {
                Toggle("Open this app at login", isOn: startsAtLogin)
                    .help("Shows the menu bar icon after login. The daemon does not need the app to run.")
                    .accessibilityIdentifier("start-at-login")
                Spacer()
                Button("Login Items in System Settings…") {
                    SystemSettings.openLoginItems()
                }
                .controlSize(.small)
                .help("Where macOS lists the daemon and this app, and lets you switch either off")
                .accessibilityIdentifier("open-login-items")
            }
        }
        .confirmationDialog("Uninstall the background service?", isPresented: $confirmsUninstall) {
            Button("Uninstall", role: .destructive) {
                Task { await service.run(.uninstall) }
            }
        } message: {
            Text("The daemon stops and no longer starts at login. Your models, the store and the settings stay where they are.")
        }
    }

    private var startsAtLogin: Binding<Bool> {
        Binding {
            service.startsAtLogin
        } set: { enabled in
            service.setStartsAtLogin(enabled)
        }
    }

    @ViewBuilder
    private func state(_ agent: AgentState) -> some View {
        Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 12, verticalSpacing: 5) {
            FactRow("State", Self.words(agent))
            if agent.installed {
                FactRow("Agent", agent.plist, monospaced: true)
                FactRow("Runs", "\(agent.program)\(agent.installedVersion.map { " (version \($0))" } ?? " (not there)")", monospaced: true)
            }
            FactRow("This app carries", "dedupd \(agent.version)")
        }
        .accessibilityIdentifier("agent-state")
        if agent.needsUpdate {
            Label(
                "The installed daemon is version \(agent.installedVersion ?? "?"); this app carries \(agent.version).",
                systemImage: "arrow.up.circle"
            )
            .foregroundStyle(.orange)
        }
        if !agent.installed {
            Toggle("Also point apps started from the Dock or Finder at the daemon (sets HF_ENDPOINT at login)", isOn: $setsEnvironment)
        }
        HStack(spacing: 8) {
            if !agent.installed {
                Button("Install and Start") {
                    Task { await service.run(.install(setEnvironment: setsEnvironment)) }
                }
                .buttonStyle(.borderedProminent)
                .accessibilityIdentifier("install-agent")
            } else {
                if agent.needsUpdate {
                    Button("Update") {
                        Task { await service.run(.install(setEnvironment: false)) }
                    }
                    .buttonStyle(.borderedProminent)
                    .help("Replace the installed daemon with the one in this app and start it again")
                    .accessibilityIdentifier("update-agent")
                }
                if agent.running {
                    Button("Stop") {
                        Task { await service.run(.stop) }
                    }
                    .help("Stop the daemon until the next login or Start")
                    .accessibilityIdentifier("stop-agent")
                } else {
                    Button("Start") {
                        Task { await service.run(.start) }
                    }
                    .accessibilityIdentifier("start-agent")
                }
                Button("Uninstall…", role: .destructive) {
                    confirmsUninstall = true
                }
                .accessibilityIdentifier("uninstall-agent")
            }
            if service.isWorking {
                ProgressView()
                    .controlSize(.small)
            }
            Spacer()
        }
        .disabled(service.isWorking)
    }

    private static func words(_ agent: AgentState) -> String {
        switch (agent.installed, agent.running) {
        case (true, true): "Installed, and the daemon is running"
        case (true, false): "Installed; the daemon is stopped"
        case (false, true): "The daemon is running, but its agent file is gone"
        case (false, false): "Not installed"
        }
    }
}

/// The end of the daemon's log.
private struct LogCard: View {
    @Environment(ServiceStore.self) private var service

    var body: some View {
        Card(title: "Log") {
            if let log = service.log {
                ScrollView {
                    Text(log.lines.isEmpty ? "The log is empty." : log.lines.joined(separator: "\n"))
                        .font(.caption.monospaced())
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(8)
                }
                .frame(height: 200)
                .defaultScrollAnchor(.bottom)
                .background(.quaternary.opacity(0.4), in: .rect(cornerRadius: 6))
                .accessibilityIdentifier("daemon-log")
                HStack {
                    Text(log.path)
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer()
                    Button("Refresh") {
                        Task { await service.loadLog() }
                    }
                    Button("Show in Finder") {
                        Finder.reveal(log.path)
                    }
                }
                .controlSize(.small)
            } else {
                Text("The daemon writes a log only when launchd runs it.")
                    .foregroundStyle(.secondary)
            }
        }
    }
}
