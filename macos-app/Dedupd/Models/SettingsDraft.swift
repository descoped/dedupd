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
        func text(_ key: String, _ value: String, _ was: String, emptyUnsets: Bool = false) {
            let value = value.trimmingCharacters(in: .whitespaces)
            guard value != was else {
                return
            }
            changes.append(SettingChange(key: key, value: value.isEmpty && emptyUnsets ? .unset : .text(value: value)))
        }
        func number(_ key: String, _ value: Int, _ was: Int) {
            if value != was {
                changes.append(SettingChange(key: key, value: .number(value: UInt64(max(0, value)))))
            }
        }
        func flag(_ key: String, _ value: Bool, _ was: Bool) {
            if value != was {
                changes.append(SettingChange(key: key, value: .flag(value: value)))
            }
        }
        text("listen", listen, original.listen)
        text("hf_endpoint", endpoint, original.endpoint, emptyUnsets: true)
        text("archive", archive, original.archive, emptyUnsets: true)
        text("bionic_models", bionic, original.bionic, emptyUnsets: true)
        text("ollama_models", ollama, original.ollama, emptyUnsets: true)
        text("hf_cache", hfCache, original.hfCache, emptyUnsets: true)
        flag("reclaim", reclaim, original.reclaim)
        flag("set_env", setEnvironment, original.setEnvironment)
        number("verify_days", verifyDays, original.verifyDays)
        number("scan_interval", scanInterval, original.scanInterval)
        number("max_connections", maxConnections, original.maxConnections)
        if speedLimit != original.speedLimit {
            let bytes = UInt64(max(0, speedLimit) * 1_000_000)
            changes.append(SettingChange(key: "speed_limit", value: .number(value: bytes)))
        }
        return changes
    }
}
