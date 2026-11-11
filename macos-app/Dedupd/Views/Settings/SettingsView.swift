import SwiftUI

/// The app's own settings: where the daemon is, how the tables look, where
/// each kind of model goes, and what the app tells the owner. The daemon's
/// settings are on the Daemon screen.
struct SettingsView: View {
    @Environment(AppSettings.self) private var settings
    @Environment(DaemonStore.self) private var daemon
    @Environment(ServiceStore.self) private var service

    var body: some View {
        @Bindable var settings = settings
        Form {
            Section("Daemon") {
                TextField("Address", text: $settings.customAddress, prompt: Text(settings.defaultAddress))
                    .autocorrectionDisabled()
                    .onSubmit {
                        daemon.start()
                    }
                    .accessibilityIdentifier("settings-address")
                Text("Leave empty for \(settings.defaultAddress). Change it only if the daemon listens somewhere else.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                LabeledContent("State", value: daemon.summary)
            }
            Section("Window") {
                Picker("Table rows", selection: $settings.density) {
                    ForEach(RowDensity.allCases) { density in
                        Text(density.title).tag(density)
                    }
                }
                .pickerStyle(.radioGroup)
                Text("Compact rows show one line per model. Larger rows add a second line with who made it and what it is.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Section("Where new models go") {
                ForEach(ModelKind.allCases) { kind in
                    LabeledContent(kind.title) {
                        HStack(spacing: 12) {
                            ForEach(apps(for: kind)) { app in
                                Toggle(app.shortTitle, isOn: binding(app, kind))
                                    .toggleStyle(.checkbox)
                            }
                        }
                    }
                }
                Text("Each app gets what it can use. The same choice is offered, and remembered, where you download.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Section("General") {
                Toggle("Tell me when a download is done or needs me", isOn: $settings.notifications)
                Toggle("Open this app at login", isOn: startsAtLogin)
            }
        }
        .formStyle(.grouped)
        .frame(width: 520)
        .fixedSize(horizontal: false, vertical: true)
        .onChange(of: settings.address) {
            daemon.start()
        }
        .task {
            await service.loadAgent()
        }
        .modifier(MainWindowOpener())
    }

    /// The apps that can use a kind of model.
    private func apps(for kind: ModelKind) -> [ModelApp] {
        switch kind {
        case .gguf: [.bionic, .ollama, .huggingFace]
        case .splitGGUF, .mlx: [.bionic, .huggingFace]
        case .other: [.huggingFace]
        }
    }

    private func binding(_ app: ModelApp, _ kind: ModelKind) -> Binding<Bool> {
        Binding {
            settings.apps(for: kind).contains(app)
        } set: { enabled in
            settings.setApp(app, for: kind, to: enabled)
        }
    }

    private var startsAtLogin: Binding<Bool> {
        Binding {
            service.startsAtLogin
        } set: { enabled in
            service.setStartsAtLogin(enabled)
        }
    }
}
