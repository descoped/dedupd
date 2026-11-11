import SwiftUI

/// The Settings window: where the daemon listens, and what it reports about
/// itself.
struct SettingsView: View {
    @Environment(AppSettings.self) private var settings
    @Environment(DaemonStore.self) private var store

    var body: some View {
        @Bindable var settings = settings
        Form {
            Section {
                TextField("Daemon address", text: $settings.customAddress, prompt: Text(settings.defaultAddress))
                    .autocorrectionDisabled()
                    .onSubmit {
                        Task { await store.refresh() }
                    }
                    .accessibilityIdentifier("settings-address")
                Text("Leave empty for \(settings.defaultAddress). Change it only if `listen` is set in the daemon's config.toml.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Section("Daemon") {
                LabeledContent("State", value: store.summary)
                if let status = store.status {
                    LabeledContent("Home folder", value: status.home)
                    LabeledContent("Fetches from", value: status.endpoint)
                    LabeledContent("Archive", value: status.archivePath ?? "None")
                }
            }
        }
        .formStyle(.grouped)
        .frame(width: 460)
        .fixedSize(horizontal: false, vertical: true)
        .onChange(of: settings.address) {
            Task { await store.refresh() }
        }
    }
}
