import Foundation
import ServiceManagement

/// What the `dedupd` command says about the launchd agent that keeps the
/// daemon running.
struct AgentState: Equatable, Sendable, Decodable {
    /// The agent's name in launchd.
    var label: String
    /// The agent's definition under `~/Library/LaunchAgents`.
    var plist: String
    var installed: Bool
    var running: Bool
    var home: String
    /// The copy of `dedupd` the agent runs.
    var program: String
    /// The version of that copy; nil when it is not there.
    var installedVersion: String?
    /// The version of the `dedupd` inside this app.
    var version: String
    var log: String

    private enum CodingKeys: String, CodingKey {
        case label, plist, installed, running, home, program, version, log
        case installedVersion = "installed_version"
    }

    /// The app carries a newer or older daemon than the one installed.
    var needsUpdate: Bool {
        installed && installedVersion != nil && installedVersion != version
    }
}

enum AgentError: Error, Equatable {
    /// The app was built without the daemon inside it.
    case notBundled
    /// The command ran and failed; the text is what it wrote.
    case failed(String)

    var message: String {
        switch self {
        case .notBundled:
            "This build of the app has no dedupd inside it. Install the daemon from a terminal with `dedupd install`."
        case let .failed(text):
            text
        }
    }
}

/// Installing, removing, starting and stopping the daemon's launchd agent.
protocol AgentControl: Sendable {
    func state() async throws -> AgentState
    /// Copy the daemon to its home folder, register the agent and start it.
    /// Run again, it replaces the installed daemon with the one in the app.
    func install(setEnvironment: Bool) async throws -> String
    /// Stop the daemon and remove the agent. Models and the store are left
    /// alone.
    func uninstall() async throws -> String
    func start() async throws -> String
    func stop() async throws -> String
}

/// Runs the `dedupd` command the app carries in `Contents/Helpers`.
struct BundledAgent: AgentControl {
    /// Another home folder than `~/.dedupd`, for a trial.
    var home: String?
    /// Another launchd label than the daemon's own, for a trial.
    var label: String?

    /// Where the build puts the daemon inside the app.
    static var executable: URL {
        Bundle.main.bundleURL.appending(path: "Contents/Helpers/dedupd")
    }

    func state() async throws -> AgentState {
        let text = try await run(["agent", "--json"])
        do {
            return try JSONDecoder().decode(AgentState.self, from: Data(text.utf8))
        } catch {
            throw AgentError.failed("dedupd agent --json answered with something else: \(text)")
        }
    }

    func install(setEnvironment: Bool) async throws -> String {
        try await run(["install"] + (setEnvironment ? ["--env"] : []))
    }

    func uninstall() async throws -> String {
        try await run(["uninstall"])
    }

    func start() async throws -> String {
        try await run(["start"])
    }

    func stop() async throws -> String {
        try await run(["stop"])
    }

    /// The whole command line after the program's name.
    func arguments(_ command: [String]) -> [String] {
        var arguments: [String] = []
        if let home {
            arguments += ["--home", home]
        }
        arguments += command
        if let label {
            arguments += ["--label", label]
        }
        return arguments
    }

    private func run(_ command: [String]) async throws -> String {
        let executable = Self.executable
        guard FileManager.default.isExecutableFile(atPath: executable.path) else {
            throw AgentError.notBundled
        }
        let arguments = arguments(command)
        return try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                continuation.resume(with: Result { try Self.launch(executable, arguments) })
            }
        }
    }

    private static func launch(_ executable: URL, _ arguments: [String]) throws -> String {
        let process = Process()
        process.executableURL = executable
        process.arguments = arguments
        let output = Pipe()
        let errors = Pipe()
        process.standardOutput = output
        process.standardError = errors
        process.standardInput = FileHandle.nullDevice
        do {
            try process.run()
        } catch {
            throw AgentError.failed("Could not run \(executable.path): \(error.localizedDescription)")
        }
        // The commands write a few lines; reading to the end before waiting
        // cannot fill a pipe.
        let said = String(bytes: output.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        let complained = String(bytes: errors.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        process.waitUntilExit()
        guard process.terminationStatus == 0 else {
            let text = complained.trimmingCharacters(in: .whitespacesAndNewlines)
            throw AgentError.failed(text.isEmpty ? "dedupd \(arguments.joined(separator: " ")) failed." : text)
        }
        return said.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

/// An agent that answers from fixed data, for UI tests and previews.
struct FixtureAgent: AgentControl {
    var installed = true

    func state() async throws -> AgentState {
        AgentState(
            label: "com.dedupd.daemon",
            plist: "/Users/example/Library/LaunchAgents/com.dedupd.daemon.plist",
            installed: installed,
            running: installed,
            home: "/Users/example/.dedupd",
            program: "/Users/example/.dedupd/bin/dedupd",
            installedVersion: installed ? "0.1.0" : nil,
            version: "0.1.0",
            log: "/Users/example/.dedupd/daemon.log"
        )
    }

    func install(setEnvironment: Bool) async throws -> String { "Installed and started the daemon." }
    func uninstall() async throws -> String { "Stopped the daemon and removed the agent." }
    func start() async throws -> String { "Started the daemon." }
    func stop() async throws -> String { "Stopped the daemon." }
}

/// Whether macOS opens the app at login. The system keeps the answer; the
/// owner can also change it in System Settings › General › Login Items.
@MainActor
enum LoginItem {
    static var isEnabled: Bool {
        SMAppService.mainApp.status == .enabled
    }

    static func set(_ enabled: Bool) throws {
        if enabled {
            try SMAppService.mainApp.register()
        } else {
            try SMAppService.mainApp.unregister()
        }
    }
}
