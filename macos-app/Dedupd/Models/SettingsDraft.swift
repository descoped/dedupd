import DedupdClient
import Foundation

/// The daemon's settings while the owner edits them. Only what differs
/// from what the daemon has is sent, so its config file keeps everything
/// else as it was written, comments included.
struct SettingsDraft: Equatable, Sendable {
    var listen: String
    var endpoint: String
    /// The archive folder; empty for none.
    var archive: String
    var bionic: String
    var ollama: String
    var hfCache: String
    var reclaim: Bool
    var setEnvironment: Bool
    var verifyDays: Int
    var scanInterval: Int
    var maxConnections: Int
    /// Megabytes a second; 0 for no limit.
    var speedLimit: Double

    init(_ settings: DaemonSettings) {
        listen = settings.listen
        endpoint = settings.hfEndpoint
        archive = settings.archive ?? ""
        bionic = settings.bionicModels
        ollama = settings.ollamaModels
        hfCache = settings.hfCache
        reclaim = settings.reclaim
        setEnvironment = settings.setEnv
        verifyDays = Int(settings.verifyDays)
        scanInterval = Int(settings.scanInterval)
        maxConnections = Int(settings.maxConnections)
        speedLimit = Double(settings.speedLimit) / 1_000_000
    }

    /// What to send the daemon to get from `original` to this.
    func changes(from original: SettingsDraft) -> [SettingChange] {
        var changes: [SettingChange] = []
        func text(_ key: Setting, _ value: String, _ was: String, emptyUnsets: Bool = false) {
            let value = value.trimmingCharacters(in: .whitespaces)
            guard value != was else {
                return
            }
            changes.append(SettingChange(key: key, value: value.isEmpty && emptyUnsets ? .unset : .text(value: value)))
        }
        func number(_ key: Setting, _ value: Int, _ was: Int) {
            if value != was {
                changes.append(SettingChange(key: key, value: .number(value: UInt64(max(0, value)))))
            }
        }
        func flag(_ key: Setting, _ value: Bool, _ was: Bool) {
            if value != was {
                changes.append(SettingChange(key: key, value: .flag(value: value)))
            }
        }
        text(.listen, listen, original.listen)
        text(.hfEndpoint, endpoint, original.endpoint, emptyUnsets: true)
        text(.archive, archive, original.archive, emptyUnsets: true)
        text(.bionicModels, bionic, original.bionic, emptyUnsets: true)
        text(.ollamaModels, ollama, original.ollama, emptyUnsets: true)
        text(.hfCache, hfCache, original.hfCache, emptyUnsets: true)
        flag(.reclaim, reclaim, original.reclaim)
        flag(.setEnv, setEnvironment, original.setEnvironment)
        number(.verifyDays, verifyDays, original.verifyDays)
        number(.scanInterval, scanInterval, original.scanInterval)
        number(.maxConnections, maxConnections, original.maxConnections)
        if speedLimit != original.speedLimit {
            // What was typed can be any number: one that is not a number, or
            // too large to count in bytes, is cut to the largest limit.
            let megabytes = speedLimit.isNaN ? 0 : min(max(0, speedLimit), Self.largestSpeedLimit)
            let bytes = UInt64((megabytes * 1_000_000).rounded())
            changes.append(SettingChange(key: .speedLimit, value: .number(value: bytes)))
        }
        return changes
    }

    /// The largest speed limit that is sent, in megabytes a second. Far above
    /// any connection; it only keeps the number countable.
    static let largestSpeedLimit: Double = 1_000_000
}
