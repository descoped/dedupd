import AppKit

enum Pasteboard {
    @MainActor
    static func copy(_ text: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }
}

enum Finder {
    /// Show a file or folder in the Finder; a path that is not there opens
    /// the nearest folder that is.
    @MainActor
    static func reveal(_ path: String) {
        var url = URL(fileURLWithPath: path)
        while !FileManager.default.fileExists(atPath: url.path), url.path != "/" {
            url.deleteLastPathComponent()
        }
        NSWorkspace.shared.activateFileViewerSelecting([url])
    }
}

enum SystemSettings {
    /// Where macOS lists what opens at login and what runs in the background.
    static let loginItems = URL(string: "x-apple.systempreferences:com.apple.LoginItems-Settings.extension")

    /// Open System Settings › General › Login Items & Extensions, where the
    /// owner can see the daemon and this app and switch either off.
    @MainActor
    static func openLoginItems() {
        if let loginItems {
            NSWorkspace.shared.open(loginItems)
        }
    }
}
