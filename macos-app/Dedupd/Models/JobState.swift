import DedupdClient
import SwiftUI

/// Where a download is, as the daemon names it.
enum JobState: String, Sendable {
    case queued
    case resolving
    case downloading
    case verifying
    case placing
    case done
    case paused
    case waitingServer = "waiting_server"
    case waitingNetwork = "waiting_network"
    case needsAttention = "needs_attention"
    case cancelled
    /// A state this version of the app does not know.
    case unknown

    init(_ name: String) {
        self = JobState(rawValue: name) ?? .unknown
    }

    var title: String {
        switch self {
        case .queued: "Queued"
        case .resolving: "Looking up"
        case .downloading: "Downloading"
        case .verifying: "Verifying"
        case .placing: "Placing into apps"
        case .done: "Done"
        case .paused: "Paused"
        case .waitingServer: "Waiting for the server"
        case .waitingNetwork: "Waiting for the network"
        case .needsAttention: "Needs attention"
        case .cancelled: "Cancelled"
        case .unknown: "Unknown"
        }
    }

    var symbol: String {
        switch self {
        case .queued: "clock"
        case .resolving: "magnifyingglass"
        case .downloading: "arrow.down.circle.fill"
        case .verifying: "checkmark.shield"
        case .placing: "square.on.square"
        case .done: "checkmark.circle.fill"
        case .paused: "pause.circle.fill"
        case .waitingServer: "hourglass"
        case .waitingNetwork: "wifi.exclamationmark"
        case .needsAttention: "exclamationmark.triangle.fill"
        case .cancelled: "xmark.circle"
        case .unknown: "questionmark.circle"
        }
    }

    var tint: Color {
        switch self {
        case .done: .green
        case .needsAttention: .orange
        case .waitingServer, .waitingNetwork: .yellow
        case .downloading, .verifying, .placing, .resolving: .accentColor
        case .queued, .paused, .cancelled, .unknown: .secondary
        }
    }

    /// The daemon is working on it, or will by itself.
    var isActive: Bool {
        switch self {
        case .queued, .resolving, .downloading, .verifying, .placing, .waitingServer, .waitingNetwork: true
        case .done, .paused, .needsAttention, .cancelled, .unknown: false
        }
    }

    /// Nothing more will happen to it.
    var isFinished: Bool {
        self == .done || self == .cancelled
    }

    var canPause: Bool { isActive }

    var canResume: Bool { self == .paused || self == .needsAttention }
}

extension Job {
    var phase: JobState { JobState(state) }

    /// How far the download has come, from 0 to 1; nil before its size is
    /// known.
    var fraction: Double? {
        guard bytesTotal > 0 else {
            return phase == .done ? 1 : nil
        }
        return min(1, Double(bytesDone) / Double(bytesTotal))
    }

    /// Seconds left at the current speed; nil when that cannot be told.
    var secondsLeft: UInt64? {
        guard phase == .downloading, speed > 0, bytesTotal > bytesDone else {
            return nil
        }
        return (bytesTotal - bytesDone) / speed
    }

    /// "41.94 MB of 106.07 MB", or what there is to say before the size is
    /// known.
    var progressText: String {
        guard bytesTotal > 0 else {
            return phase.title
        }
        if phase == .done {
            return ByteFormat.string(bytesTotal)
        }
        return "\(ByteFormat.string(bytesDone)) of \(ByteFormat.string(bytesTotal))"
    }

    /// The second line of a download: speed and time left while it runs, the
    /// reason while it waits.
    var statusText: String {
        if let detail, !detail.isEmpty, phase != .downloading {
            return detail
        }
        switch phase {
        case .downloading:
            guard speed > 0 else {
                return "Starting…"
            }
            let left = secondsLeft.map { " · \(ByteFormat.duration(seconds: $0)) left" } ?? ""
            return "\(ByteFormat.speed(speed))\(left)"
        case .done:
            return outcomeText
        default:
            return phase.title
        }
    }

    /// The job puts a stored model into apps, rather than fetching one.
    var isActivation: Bool { kind == "activation" }

    /// The daemon says what a finished job did. One older than the app sends
    /// no kind and no answer per app; its jobs are not judged.
    private var saysOutcome: Bool { !kind.isEmpty }

    /// The apps asked for that did not take the model, each with why, once
    /// the job is done.
    var missed: [NotActivated] {
        phase == .done && saysOutcome ? notActivated : []
    }

    /// What a finished job did: "Activated in Bionic", "Not activated in
    /// Ollama: <why>", or both.
    var outcomeText: String {
        if apps.isEmpty {
            return "In the store only"
        }
        guard saysOutcome else {
            return phase.title
        }
        var parts: [String] = []
        if !activated.isEmpty {
            parts.append("Activated in \(Self.list(activated))")
        }
        let missed = missed
        if !missed.isEmpty {
            let not = activated.isEmpty ? "Not activated in" : "not in"
            // Each app with its own reason, so two reasons are not mixed up.
            let why = missed.map { missed.count == 1 ? $0.reason : "\(ModelApp.shortTitle(for: $0.app)): \($0.reason)" }
            parts.append("\(not) \(Self.list(missed.map(\.app))): \(why.joined(separator: "; "))")
        }
        return parts.joined(separator: " · ")
    }

    /// The row's symbol: a finished job that an app did not take is marked,
    /// so it does not pass for a success.
    var symbol: String {
        missed.isEmpty ? phase.symbol : "exclamationmark.circle.fill"
    }

    var tint: Color {
        missed.isEmpty ? phase.tint : .orange
    }

    /// "Bionic and Ollama", or "the store only".
    var appList: String {
        apps.isEmpty ? "the store only" : Self.list(apps)
    }

    private static func list(_ apps: [String]) -> String {
        ListFormatter.localizedString(byJoining: apps.map(ModelApp.shortTitle(for:)))
    }
}
