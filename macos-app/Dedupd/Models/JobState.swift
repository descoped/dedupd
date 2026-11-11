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
            return "Placed into \(appList)"
        default:
            return phase.title
        }
    }

    /// "Bionic and Ollama", or "the store only".
    var appList: String {
        let names = apps.map(ModelApp.shortTitle(for:))
        return names.isEmpty ? "the store only" : ListFormatter.localizedString(byJoining: names)
    }
}
