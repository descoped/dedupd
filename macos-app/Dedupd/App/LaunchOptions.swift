import Foundation

/// How the app was started.
///
/// Every option is written as a pair, `-name value`, the way AppKit reads
/// its own arguments. A name without a value in front of another option
/// shifts the pairs, the last word is then taken for a file to open, and an
/// app started to open a file shows no window of its own.
struct LaunchOptions: Equatable {
    /// The canned daemon a UI test asked for, with `-uiTestFixture <name>`.
    var fixture: FixtureScenario?
    /// Show the menu bar panel in an ordinary window, asked for with
    /// `-panelWindow YES`. A menu bar panel closes when it loses focus, so it
    /// can be neither driven by a UI test nor photographed.
    var showsPanelWindow = false
    /// Open the main window at launch, asked for with `-mainWindow YES`.
    var showsMainWindow = false
    /// The screen to start on, asked for with `-pane <name>`.
    var pane: Pane?
    /// Another home folder for the daemon the app installs: `-daemonHome <dir>`.
    var agentHome: String?
    /// Another launchd label for the daemon the app installs: `-agentLabel <label>`.
    var agentLabel: String?
    /// Where the daemon is, for this run only: `-daemonAddress host:port`.
    /// It is never saved to the preferences.
    var daemonAddress: String?
    /// The options that were named with no value after them.
    var malformed: [String] = []
    /// The app is hosting its unit tests and must leave the real daemon alone.
    var isHostingUnitTests = false

    init(arguments: [String] = [], environment: [String: String] = [:]) {
        var unfinished: [String] = []
        // The word after the name, unless it is the next option's name or
        // there is none: `-daemonHome -agentLabel x` names no home.
        func value(of flag: String) -> String? {
            guard let index = arguments.firstIndex(of: flag) else {
                return nil
            }
            guard arguments.indices.contains(index + 1), !arguments[index + 1].hasPrefix("-") else {
                unfinished.append(flag)
                return nil
            }
            return arguments[index + 1]
        }
        fixture = value(of: "-uiTestFixture").flatMap(FixtureScenario.init(rawValue:))
        showsPanelWindow = value(of: "-panelWindow") == "YES"
        showsMainWindow = value(of: "-mainWindow") == "YES"
        pane = value(of: "-pane").flatMap(Pane.init(rawValue:))
        agentHome = value(of: "-daemonHome")
        agentLabel = value(of: "-agentLabel")
        daemonAddress = value(of: "-daemonAddress")
        malformed = unfinished
        isHostingUnitTests = environment["XCTestConfigurationFilePath"] != nil
    }

    init(process: ProcessInfo) {
        self.init(arguments: process.arguments, environment: process.environment)
    }
}
