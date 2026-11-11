import SwiftUI

/// The icon in the menu bar. Its shape says whether the daemon is running
/// and whether something is being downloaded.
struct MenuBarLabel: View {
    let store: DaemonStore

    var body: some View {
        Image(systemName: store.menuBarSymbol)
            .accessibilityLabel("dedupd: \(store.summary)")
    }
}
