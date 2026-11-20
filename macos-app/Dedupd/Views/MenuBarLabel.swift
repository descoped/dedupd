import SwiftUI

/// The icon in the menu bar. Its shape says whether the daemon is running,
/// whether something is being downloaded, and whether a download needs the
/// owner.
struct MenuBarLabel: View {
    let environment: AppEnvironment

    var body: some View {
        Image(systemName: environment.daemon.menuBarSymbol)
            .accessibilityLabel("dedupd: \(environment.daemon.summary)")
            .modifier(MainWindowOpener())
            .environment(environment.windows)
    }
}

/// Lets the rest of the app open the main window. Only a view can reach
/// SwiftUI's window actions, so every scene's root view hands the action on.
struct MainWindowOpener: ViewModifier {
    @Environment(\.openWindow) private var openWindow
    @Environment(WindowControl.self) private var windows

    func body(content: Content) -> some View {
        content.onAppear {
            windows.open = { openWindow(id: MainWindow.id) }
        }
    }
}
