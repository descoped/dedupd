import DedupdClient
import Foundation

// What the screens say about the daemon's answers, and the small decisions
// they make from them. Plain functions of the answers, so the views only
// lay them out and a test can call each one.

/// The two halves of `org/name`.
enum RepoName {
    /// The part after the organisation: what people call the model.
    static func name(_ repo: String) -> String {
        repo.split(separator: "/").last.map(String.init) ?? repo
    }

    static func organisation(_ repo: String) -> String {
        repo.split(separator: "/").first.map(String.init) ?? ""
    }
}

extension Revision {
    /// "main · 105.45 MB · added 12 November 2026".
    var caption: String {
        var parts = [ByteFormat.string(bytes), "added \(TimeFormat.full(TimeFormat.date(epoch: fetchedAt)))"]
        if !refs.isEmpty {
            parts.insert(refs.joined(separator: ", "), at: 0)
        }
        return parts.joined(separator: " · ")
    }
}

extension RevisionFile {
    /// Where the file is, and which apps have it.
    var note: String {
        let place = stored ? "stored" : (archived ? "on the archive disk only" : "not downloaded")
        let apps = apps.map(\.shortTitle)
        return apps.isEmpty ? place : "\(place) · in \(ListFormatter.localizedString(byJoining: apps))"
    }
}

extension Placement {
    /// How an app's file compares with what Hugging Face lists.
    var stateWords: String {
        switch verifyState {
        case .verified: "verified"
        case .mismatch: "differs"
        case .notUpstream: "not on Hugging Face"
        case .unchecked: "not checked"
        }
    }
}

extension UpstreamCheck {
    /// The answer to "Check Hugging Face", in a sentence.
    var words: String {
        if upstream == .gone {
            return "Hugging Face no longer has this model. Your copy is what is left of it."
        }
        if changed, let latest {
            return "Hugging Face has a newer revision, \(latest.prefix(12)). Download it from Discover."
        }
        return "This is the latest revision."
    }
}

extension AgentState {
    /// Whether the agent is installed and the daemon runs, in words.
    var words: String {
        switch (installed, running) {
        case (true, true): "Installed, and the daemon is running"
        case (true, false): "Installed; the daemon is stopped"
        case (false, true): "The daemon is running, but its agent file is gone"
        case (false, false): "Not installed"
        }
    }
}

extension JobFile {
    /// How far one file of a download is.
    var stageWords: String {
        switch stage {
        case .stored: "in the store"
        case .verifying: "being verified"
        case .downloading: "\(ByteFormat.string(done)) of \(ByteFormat.string(size))"
        case .waiting: done > 0 ? "\(ByteFormat.string(done)) downloaded, waiting" : "waiting"
        }
    }
}

extension AppPresence {
    /// "4 files, 105.45 MB, shared with the store".
    var summary: String {
        let shared = sharedBytes >= bytes
            ? "shared with the store"
            : "\(ByteFormat.string(bytes - sharedBytes)) not shared yet"
        return "\(ByteFormat.files(files)), \(ByteFormat.string(bytes)), \(shared)"
    }
}

extension Variant {
    /// Where the variant's files are, for the Library: "stored", "on the
    /// archive disk", "not on this Mac".
    func storedWords(archived: Bool) -> String {
        if isStored {
            return "stored"
        }
        if weightsStored {
            return "stored; \(ByteFormat.string(missingBytes)) of small files are fetched when it is activated"
        }
        if archived {
            return "on the archive disk"
        }
        return storedBytes == 0 ? "not on this Mac" : "\(ByteFormat.string(storedBytes)) stored"
    }

    /// Files, size and where they are, for the Library:
    /// "4 files, 105.45 MB · stored".
    func libraryDetail(archived: Bool) -> String {
        "\(ByteFormat.files(files)), \(ByteFormat.string(bytes)) · \(storedWords(archived: archived))"
    }

    /// Size, files and where it already is, for Discover:
    /// "105.45 MB · 4 files · in Bionic".
    var hubDetail: String {
        var parts = [ByteFormat.string(bytes)]
        if files > 1 {
            parts.append(ByteFormat.files(files))
        }
        if !placedIn.isEmpty {
            parts.append("in " + ListFormatter.localizedString(byJoining: placedIn.map(\.shortTitle)))
        } else if canActivate {
            parts.append("in the store")
        }
        return parts.joined(separator: " · ")
    }

    /// The apps the button in Discover sends the variant to, of the ones
    /// chosen for its format: a variant whose weights are stored goes only
    /// to those that do not have it; one to download goes to all of them.
    func targets(of chosen: [ModelApp]) -> [ModelApp] {
        canActivate ? chosen.filter { !isPlaced(in: $0) } : chosen
    }

    /// What the button in Discover does with the variant, for the apps it
    /// goes to.
    func fetchHelp(into apps: [ModelApp]) -> String {
        let place = apps.isEmpty
            ? "keeps it in the store"
            : "activates it in \(ListFormatter.localizedString(byJoining: apps.map(\.shortTitle)))"
        if isStored {
            return "Already stored: \(place) as a clone, without downloading"
        }
        if weightsStored {
            let lacking = ByteFormat.string(missingBytes)
            return "Already stored: \(place) as a clone, after fetching the \(lacking) of small files it lacks"
        }
        return "Downloads \(ByteFormat.string(missingBytes)) and \(place)"
    }
}

extension [Variant] {
    /// The variants an app can be given now: it can use them, does not
    /// have them, and their weights are stored.
    func activatable(in app: ModelApp) -> [Variant] {
        filter { $0.usableApps.contains(app) && !$0.isPlaced(in: app) && $0.canActivate }
    }
}

extension [Job] {
    /// The job for a variant that is in the queue and not finished.
    func queued(for variant: Variant) -> Job? {
        first { $0.model == variant.spec && ($0.state.isActive || $0.state == .paused) }
    }

    /// A download or activation of the model for the app is under way.
    func isActivating(_ repo: String, in app: ModelApp) -> Bool {
        contains { $0.state.isActive && $0.repo == repo && $0.apps.contains(app) }
    }
}

extension Job {
    /// One line on a download: "41.94 MB of 106.07 MB · 11.80 MB/s · 5 s left".
    var line: String {
        switch state {
        case .downloading, .done:
            "\(progressText) · \(statusText)"
        case .paused where bytesTotal > 0:
            "Paused · \(progressText)"
        default:
            statusText
        }
    }
}

extension UnplacePlan {
    /// What a deactivation removes, in a sentence.
    var summary: String {
        let count = UInt64(remove.count)
        if count == 0, links.isEmpty {
            return "Nothing can be removed: every file is still needed or has no stored copy."
        }
        return "\(ByteFormat.files(count)), \(ByteFormat.string(bytes)), are deleted from the app's folder. "
            + "The stored copy stays, so the model can be activated again without downloading."
    }
}

extension Location {
    /// One line on a folder: what it is, or what it lacks.
    var note: String {
        if !present {
            return kind == .archive ? "The disk is not attached" : "The folder is not there"
        }
        switch kind {
        case .store:
            return "What the apps share"
        case .archive:
            return "The second copy, on another disk"
        case .app:
            if bytes == 0 {
                return "Empty"
            }
            return sharedBytes >= bytes
                ? "All of it shared with the store"
                : "\(ByteFormat.string(bytes - sharedBytes)) not shared yet"
        }
    }
}

extension Locations {
    /// What the apps' folders hold, as each app sees it.
    var appBytes: UInt64 { apps.map(\.bytes).reduce(0, +) }
    /// What of that is clones of the store.
    var sharedBytes: UInt64 { apps.map(\.sharedBytes).reduce(0, +) }
    /// What is really on the disk for models: the store, plus what the apps
    /// hold that is not a clone of it.
    var modelBytes: UInt64 { store.bytes + (appBytes - min(appBytes, sharedBytes)) }
}

extension DaemonInfo {
    /// Whether the daemon has a Hugging Face token, and from where.
    var tokenWords: String {
        guard token else {
            return "None. Gated models cannot be downloaded; run `hf auth login` to add one."
        }
        return tokenSource.map { "Found in \($0)" } ?? "Found"
    }
}
