import Foundation

/// The app's own version, and how two versions compare.
///
/// One number names a release everywhere: `Cargo.toml` holds it, the app's
/// version and build number are synced from it (`Version.xcconfig`), and the
/// daemon inside the app reports it. The words the screens use for it are
/// made here, so they read the same wherever they appear.
enum AppVersion {
    /// This app's version, for example "0.3.0".
    static var current: String {
        Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "unknown"
    }

    /// The app by name and version, for example "dedupd 0.3.0".
    static var title: String { titled(current) }

    /// A version by name: "dedupd 0.3.0".
    static func titled(_ version: String) -> String {
        "dedupd \(version)"
    }

    /// Whether `version` is older than `other`, comparing the numbers between
    /// the dots ("0.9.0" is older than "0.10.0"). A part that is not a number
    /// counts as 0, and a missing part as 0 ("1.2" equals "1.2.0").
    static func isOlder(_ version: String, than other: String) -> Bool {
        let mine = numbers(version)
        let theirs = numbers(other)
        for index in 0 ..< max(mine.count, theirs.count) {
            let left = index < mine.count ? mine[index] : 0
            let right = index < theirs.count ? theirs[index] : 0
            if left != right {
                return left < right
            }
        }
        return false
    }

    private static func numbers(_ version: String) -> [Int] {
        version.split(separator: ".").map { Int($0) ?? 0 }
    }
}
