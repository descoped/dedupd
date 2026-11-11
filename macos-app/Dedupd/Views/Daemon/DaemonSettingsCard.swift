import AppKit
import DedupdClient
import SwiftUI

/// The daemon's own settings, as its config file has them. Changes are
/// written to the file by the daemon; most take effect when it restarts.
struct DaemonSettingsCard: View {
    @Environment(ServiceStore.self) private var service
    @State private var draft: SettingsDraft?
    @State private var original: SettingsDraft?

    var body: some View {
        Card(title: "Settings", note: service.settings.value.map { "Kept in \($0.path)" }) {
            switch service.settings {
            case let .ready(settings):
                if let draft = Binding($draft) {
                    form(draft, settings)
                }
            case let .failed(message):
                FailureLabel(message: message)
            case .idle, .loading:
                ProgressView()
                    .controlSize(.small)
            }
        }
        .onChange(of: service.settings.value, initial: true) { _, settings in
            // What the daemon answers replaces the draft: it is the state
            // of the file after the change.
            let fresh = settings.map(SettingsDraft.init)
            draft = fresh
            original = fresh
        }
    }

    @ViewBuilder
    private func form(_ draft: Binding<SettingsDraft>, _ settings: DaemonSettings) -> some View {
        if settings.restartNeeded {
            HStack {
                Label("Some settings take effect when the daemon restarts.", systemImage: "arrow.clockwise.circle.fill")
                    .foregroundStyle(.orange)
                Spacer()
                Button("Restart Now") {
                    Task { await service.restart() }
                }
            }
            .accessibilityIdentifier("restart-needed")
        }
        Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 12, verticalSpacing: 10) {
            downloads(draft)
            folders(draft)
            behaviour(draft)
            network(draft)
        }
        .textFieldStyle(.roundedBorder)
        HStack {
            Spacer()
            Button("Revert") {
                self.draft = original
            }
            .disabled(draft.wrappedValue == original)
            Button("Apply") {
                guard let original else {
                    return
                }
                let changes = draft.wrappedValue.changes(from: original)
                Task { await service.update(changes) }
            }
            .buttonStyle(.borderedProminent)
            .disabled(draft.wrappedValue == original)
            .accessibilityIdentifier("apply-settings")
        }
    }

    @ViewBuilder
    private func downloads(_ draft: Binding<SettingsDraft>) -> some View {
        heading("Downloads", "These two take effect at once.")
        row("Connections at most") {
            Stepper(value: draft.maxConnections, in: 1 ... 32) {
                Text("\(draft.wrappedValue.maxConnections)")
                    .monospacedDigit()
            }
            .accessibilityIdentifier("max-connections")
        }
        row("Speed limit") {
            HStack {
                TextField("MB/s", value: draft.speedLimit, format: .number.precision(.fractionLength(0 ... 1)))
                    .frame(width: 70)
                    .multilineTextAlignment(.trailing)
                Text("MB a second; 0 for no limit")
                    .foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder
    private func folders(_ draft: Binding<SettingsDraft>) -> some View {
        heading("Folders", nil)
        row("Archive disk") {
            FolderField(path: draft.archive, prompt: "None: no second copy")
        }
        row("Bionic and LM Studio") {
            FolderField(path: draft.bionic, prompt: "~/.lmstudio/models")
        }
        row("Ollama") {
            FolderField(path: draft.ollama, prompt: "~/.ollama/models")
        }
        row("Hugging Face cache") {
            FolderField(path: draft.hfCache, prompt: "~/.cache/huggingface/hub")
        }
    }

    @ViewBuilder
    private func behaviour(_ draft: Binding<SettingsDraft>) -> some View {
        heading("Behaviour", nil)
        row("Duplicate copies") {
            Toggle("Replace an app's own copy with a clone of the stored one", isOn: draft.reclaim)
        }
        row("Apps from the Dock") {
            Toggle("Point them at the daemon by setting HF_ENDPOINT at login", isOn: draft.setEnvironment)
        }
        row("Check the store every") {
            Stepper(value: draft.verifyDays, in: 1 ... 365) {
                Text("\(draft.wrappedValue.verifyDays) day\(draft.wrappedValue.verifyDays == 1 ? "" : "s")")
                    .monospacedDigit()
            }
        }
        row("Look at the app folders every") {
            Stepper(value: draft.scanInterval, in: 30 ... 86400, step: 30) {
                Text(ByteFormat.duration(seconds: UInt64(draft.wrappedValue.scanInterval)))
                    .monospacedDigit()
            }
        }
    }

    @ViewBuilder
    private func network(_ draft: Binding<SettingsDraft>) -> some View {
        heading("Network", nil)
        row("Listens on") {
            TextField("127.0.0.1:8642", text: draft.listen)
                .frame(maxWidth: 220)
        }
        row("Fetches from") {
            TextField("https://huggingface.co", text: draft.endpoint)
                .frame(maxWidth: 320)
        }
    }

    private func heading(_ title: String, _ note: String?) -> some View {
        GridRow {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(title)
                    .font(.subheadline.weight(.semibold))
                if let note {
                    Text(note)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .padding(.top, 6)
            .gridCellColumns(2)
        }
    }

    private func row(_ label: String, @ViewBuilder content: () -> some View) -> some View {
        GridRow {
            Text(label)
                .foregroundStyle(.secondary)
                .gridColumnAlignment(.trailing)
            content()
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// A folder's path, typed or chosen.
private struct FolderField: View {
    @Binding var path: String
    let prompt: String

    var body: some View {
        HStack {
            TextField(prompt, text: $path)
                .font(.callout.monospaced())
            Button("Choose…") {
                let panel = NSOpenPanel()
                panel.canChooseDirectories = true
                panel.canChooseFiles = false
                panel.canCreateDirectories = true
                panel.allowsMultipleSelection = false
                if !path.isEmpty {
                    panel.directoryURL = URL(fileURLWithPath: path)
                }
                if panel.runModal() == .OK, let url = panel.url {
                    path = url.path
                }
            }
            .controlSize(.small)
        }
    }
}
