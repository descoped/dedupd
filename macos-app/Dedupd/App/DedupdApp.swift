import SwiftUI

/// The dedupd app: an icon in the menu bar that is always there, and a
/// window for everything else.
///
/// The app holds no model data of its own. Everything it shows comes from the
/// daemon's control API, so the daemon keeps working when the app is closed.
/// Closing the window leaves the icon; quitting the app leaves the daemon.
@main
struct DedupdApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    private let environment = AppEnvironment.shared

    var body: some Scene {
        MenuBarExtra {
            MenuPanel()
                .appEnvironment(environment)
        } label: {
            MenuBarLabel(environment: environment)
        }
        .menuBarExtraStyle(.window)

        Window("dedupd", id: MainWindow.id) {
            MainWindow()
                .appEnvironment(environment)
        }
        .defaultSize(width: 1280, height: 800)
        .defaultLaunchBehavior(environment.opensMainWindowAtLaunch ? .presented : .suppressed)
        .restorationBehavior(.disabled)
        .commands {
            MainCommands(environment: environment)
        }

        Settings {
            SettingsView()
                .appEnvironment(environment)
        }
    }
}

/// The menu bar's own commands for the main window.
struct MainCommands: Commands {
    let environment: AppEnvironment

    var body: some Commands {
        CommandGroup(replacing: .newItem) {}
        CommandMenu("Go") {
            ForEach(Array(Pane.allCases.enumerated()), id: \.element) { index, pane in
                Button(pane.title) {
                    environment.showMainWindow(pane)
                }
                .keyboardShortcut(KeyEquivalent(Character("\(index + 1)")), modifiers: .command)
            }
        }
        CommandGroup(after: .toolbar) {
            Button("Look at the App Folders Now") {
                Task { await environment.daemon.scan() }
            }
            .keyboardShortcut("r", modifiers: [.command, .shift])
            .disabled(!environment.daemon.isRunning || environment.daemon.isScanning)
        }
    }
}
