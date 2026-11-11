import Foundation

/// Sizes written the way the `dedupd` command line writes them, so the app
/// and the terminal show the same figure for the same store.
enum ByteFormat {
    private static let units = ["B", "KB", "MB", "GB", "TB"]

    /// `1_500_000` reads "1.50 MB". Decimal units, two decimals, whole bytes
    /// below a kilobyte.
    static func string(_ bytes: UInt64) -> String {
        var value = Double(bytes)
        var unit = 0
        while value >= 1000, unit < units.count - 1 {
            value /= 1000
            unit += 1
        }
        if unit == 0 {
            return "\(bytes) B"
        }
        return String(format: "%.2f %@", locale: Locale(identifier: "en_US_POSIX"), value, units[unit])
    }

    /// "3 files" or "1 file".
    static func files(_ count: UInt64) -> String {
        count == 1 ? "1 file" : "\(count) files"
    }
}
