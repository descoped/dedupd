import DedupdClient
import SwiftUI

/// The app's own settings: where the daemon is, how the tables look, where
/// each kind of model goes, and what the app tells the owner. The daemon's
/// settings are on the Daemon screen.
struct SettingsView: View {
    @Environment(AppSettings.self) private var settings
    @Environment(DaemonStore.self) private var daemon
    @Environment(ServiceStore.self) private var service
    /// The address as it is being typed. It becomes the setting on Return
    /// and when the field is left, so the app does not try to reach every
    /// half-typed address.
    @State private var addressDraft = ""
    @FocusState private var addressFocused: Bool

    var body: some View {
        @Bindable var settings = settings
        Form {
            Section("Daemon") {
                TextField("Address", text: $addressDraft, prompt: Text(settings.defaultAddress))
                    .autocorrectionDisabled()
                    .focused($addressFocused)
                    .onSubmit(applyAddress)
                    .onChange(of: addressFocused) { _, focused in
                        if !focused {
                            applyAddress()
                        }
                    }
                    .onAppear { addressDraft = settings.customAddress }
                    .onChange(of: settings.customAddress) { _, address in
                        // Changed elsewhere while this window is open.
                        if !addressFocused {
                            addressDraft = address
                        }
                    }
                    .onDisappear(perform: applyAddress)
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
                if daemon.apps.isEmpty {
                    Text("Shown once dedupd is running: it knows which apps take which formats.")
                        .foregroundStyle(.secondary)
                }
                ForEach(ModelFormat.allCases) { format in
                    let apps = daemon.apps.apps(taking: format)
                    if !apps.isEmpty {
                        LabeledContent(format.title) {
                            HStack(spacing: 12) {
                                ForEach(apps) { app in
                                    Toggle(app.shortTitle, isOn: binding(app, format))
                                        .toggleStyle(.checkbox)
                                }
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

    /// Take the typed address into use. Following starts again through the
    /// change of `settings.address`.
    private func applyAddress() {
        if settings.customAddress != addressDraft {
            settings.customAddress = addressDraft
        }
    }

    private func binding(_ app: ModelApp, _ format: ModelFormat) -> Binding<Bool> {
        Binding {
            settings.apps(for: format, known: daemon.apps).contains(app)
        } set: { enabled in
            settings.setApp(app, for: format, to: enabled, known: daemon.apps)
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
