import AppKit
import SwiftUI

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private var panelWindow: NSWindow?

    func applicationDidFinishLaunching(_ notification: Notification) {
        let environment = AppEnvironment.shared
        if environment.options.isHostingUnitTests {
            return
        }
        let center = NotificationCenter.default
        center.addObserver(self, selector: #selector(windowAppeared), name: NSWindow.didBecomeKeyNotification, object: nil)
        center.addObserver(self, selector: #selector(windowClosing), name: NSWindow.willCloseNotification, object: nil)
        environment.daemon.start()
        if environment.followsDaemon {
            Task { await environment.service.followAppVersion() }
        }
        if environment.options.showsPanelWindow {
            showPanelInWindow(environment)
        }
    }

    /// Closing the last window leaves the app in the menu bar.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }

    /// A click on the Dock icon with no window open opens the main window.
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows: Bool) -> Bool {
        if !hasVisibleWindows {
            AppEnvironment.shared.showMainWindow()
        }
        return true
    }

    // MARK: - Dock icon

    /// The app has a Dock icon exactly while it has an ordinary window open.
    @objc
    private func windowAppeared(_ notification: Notification) {
        updateDockIcon(closing: nil)
    }

    @objc
    private func windowClosing(_ notification: Notification) {
        updateDockIcon(closing: notification.object as? NSWindow)
    }

    private func updateDockIcon(closing: NSWindow?) {
        let hasWindow = NSApp.windows.contains { window in
            window !== closing && DockIcon.isOrdinary(
                visible: window.isVisible, titled: window.styleMask.contains(.titled),
                normalLevel: window.level == .normal, panel: window is NSPanel
            )
        }
        let wanted: NSApplication.ActivationPolicy = hasWindow ? .regular : .accessory
        if NSApp.activationPolicy() != wanted {
            NSApp.setActivationPolicy(wanted)
        }
    }

    // MARK: - The panel in a window

    private func showPanelInWindow(_ environment: AppEnvironment) {
        let panel = MenuPanel().appEnvironment(environment)
        let window = NSWindow(contentViewController: NSHostingController(rootView: panel))
        window.title = "dedupd menu"
        window.styleMask = [.titled, .closable]
        window.identifier = NSUserInterfaceItemIdentifier("panel-window")
        window.center()
        NSApp.setActivationPolicy(.regular)
        window.makeKeyAndOrderFront(nil)
        NSApp.activate()
        panelWindow = window
    }
}

/// The rule for the Dock icon: the app has one exactly while it has an
/// ordinary window open.
enum DockIcon {
    /// A window the owner opened, as opposed to the menu bar panel and the
    /// windows the system keeps for the menu bar.
    static func isOrdinary(visible: Bool, titled: Bool, normalLevel: Bool, panel: Bool) -> Bool {
        visible && titled && normalLevel && !panel
    }
}
