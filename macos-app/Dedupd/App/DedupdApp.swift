import SwiftUI

/// The dedupd menu bar app: shows what the daemon holds and lets the user
/// fetch a model or have the app folders looked at.
///
/// The app holds no model data of its own. Everything it shows comes from the
/// daemon's control API, so the daemon keeps working when the app is closed.
@main
struct DedupdApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    private let environment = AppEnvironment.shared

    var body: some Scene {
        MenuBarExtra {
            StatusPanel()
                .environment(environment.store)
                .environment(environment.settings)
        } label: {
            MenuBarLabel(store: environment.store)
        }
        .menuBarExtraStyle(.window)

        Settings {
            SettingsView()
                .environment(environment.store)
                .environment(environment.settings)
        }
    }
}
