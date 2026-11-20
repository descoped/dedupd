import DedupdClient
import SwiftUI

/// How a job's state is shown. The daemon names the states and decides
/// what can be done in each (`Job.actions`).
extension JobState {
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
        }
    }

    var tint: Color {
        switch self {
        case .done: .green
        case .needsAttention: .orange
        case .waitingServer, .waitingNetwork: .yellow
        case .downloading, .verifying, .placing, .resolving: .accentColor
        case .queued, .paused, .cancelled: .secondary
        }
    }

    /// The daemon is working on it, or will by itself. The rule is the
    /// daemon's own.
    var isActive: Bool { jobIsActive(state: self) }

    /// Nothing more will happen to it. The rule is the daemon's own.
    var isFinished: Bool { jobIsFinished(state: self) }
}

/// How an action on a job is offered.
extension JobAction {
    /// The word the daemon's control API uses, also for naming controls.
    var name: String {
        switch self {
        case .pause: "pause"
        case .resume: "resume"
        case .retry: "retry"
        case .start: "start"
        case .cancel: "cancel"
        case .remove: "remove"
        }
    }

    var title: String {
        switch self {
        case .pause: "Pause"
        case .resume: "Resume"
        case .retry: "Try Again"
        case .start: "Start Now"
        case .cancel: "Cancel"
        case .remove: "Remove"
        }
    }

    var symbol: String {
        switch self {
        case .pause: "pause.fill"
        case .resume: "play.fill"
        case .retry: "arrow.clockwise"
        case .start: "forward.end.fill"
        case .cancel: "xmark"
        case .remove: "minus.circle"
        }
    }

    /// What the action does, for a job of this kind.
    func help(activation: Bool) -> String {
        let job = activation ? "activation" : "download"
        return switch self {
        case .pause: "Stop for now. What is downloaded is kept."
        case .resume: "Continue where it stopped"
        case .retry: "Try the \(job) again"
        case .start: "Start this \(job) ahead of the queue"
        case .cancel: activation ? "Give up this activation. The stored copy stays." : "Stop and give up this download"
        case .remove: "Take it off the list. Downloaded files stay."
        }
    }
}

extension Job {
    /// How far the download has come, from 0 to 1; nil before its size is
    /// known.
    var fraction: Double? {
        guard bytesTotal > 0 else {
            return state == .done ? 1 : nil
        }
        return min(1, Double(bytesDone) / Double(bytesTotal))
    }

    /// Seconds left at the current speed; nil when that cannot be told.
    var secondsLeft: UInt64? {
        guard state == .downloading, speed > 0, bytesTotal > bytesDone else {
            return nil
        }
        return (bytesTotal - bytesDone) / speed
    }

    /// "41.94 MB of 106.07 MB", or what there is to say before the size is
    /// known.
    var progressText: String {
        guard bytesTotal > 0 else {
            return state.title
        }
        if state == .done {
            return ByteFormat.string(bytesTotal)
        }
        return "\(ByteFormat.string(bytesDone)) of \(ByteFormat.string(bytesTotal))"
    }

    /// The second line of a download: speed and time left while it runs, the
    /// reason while it waits.
    var statusText: String {
        if let detail, !detail.isEmpty, state != .downloading {
            return detail
        }
        switch state {
        case .downloading:
            guard speed > 0 else {
                return "Starting…"
            }
            let left = secondsLeft.map { " · \(ByteFormat.duration(seconds: $0)) left" } ?? ""
            return "\(ByteFormat.speed(speed))\(left)"
        case .done:
            return outcomeText
        default:
            return state.title
        }
    }

    /// The job puts a stored model into apps, rather than fetching one.
    var isActivation: Bool { kind == .activation }

    /// The apps asked for that did not take the model, each with why, once
    /// the job is done.
    var missed: [NotActivated] {
        state == .done ? notActivated : []
    }

    /// What a finished job did: "Activated in Bionic", "Not activated in
    /// Ollama: <why>", or both.
    var outcomeText: String {
        if apps.isEmpty {
            return "In the store only"
        }
        var parts: [String] = []
        if !activated.isEmpty {
            parts.append("Activated in \(Self.list(activated))")
        }
        let missed = missed
        if !missed.isEmpty {
            let not = activated.isEmpty ? "Not activated in" : "not in"
            // Each app with its own reason, so two reasons are not mixed up.
            let why = missed.map { missed.count == 1 ? $0.reason : "\($0.app.shortTitle): \($0.reason)" }
            parts.append("\(not) \(Self.list(missed.map(\.app))): \(why.joined(separator: "; "))")
        }
        return parts.isEmpty ? state.title : parts.joined(separator: " · ")
    }

    /// The row's symbol: a finished job that an app did not take is marked,
    /// so it does not pass for a success.
    var symbol: String {
        missed.isEmpty ? state.symbol : "exclamationmark.circle.fill"
    }

    var tint: Color {
        missed.isEmpty ? state.tint : .orange
    }

    /// "Bionic and Ollama", or "the store only".
    var appList: String {
        apps.isEmpty ? "the store only" : Self.list(apps)
    }

    private static func list(_ apps: [ModelApp]) -> String {
        ListFormatter.localizedString(byJoining: apps.map(\.shortTitle))
    }
}
