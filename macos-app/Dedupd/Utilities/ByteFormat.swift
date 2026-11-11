import Foundation

/// Sizes written the way the `dedupd` command line writes them, so the app
/// and the terminal show the same figure for the same store.
enum ByteFormat {
    private static let units = ["B", "KB", "MB", "GB", "TB"]
    private static let posix = Locale(identifier: "en_US_POSIX")

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
        return String(format: "%.2f %@", locale: posix, value, units[unit])
    }

    /// "3 files" or "1 file".
    static func files(_ count: UInt64) -> String {
        count == 1 ? "1 file" : "\(count) files"
    }

    /// "12.30 MB/s".
    static func speed(_ bytesPerSecond: UInt64) -> String {
        "\(string(bytesPerSecond))/s"
    }

    /// A count as people say it: 950, 1.2k, 104k, 1.3M.
    static func count(_ number: UInt64) -> String {
        scaled(number, units: ["", "k", "M", "B"])
    }

    /// A number of parameters: 135M, 1.7B.
    static func parameters(_ number: UInt64) -> String {
        scaled(number, units: ["", "K", "M", "B", "T"])
    }

    private static func scaled(_ number: UInt64, units: [String]) -> String {
        var value = Double(number)
        var unit = 0
        while value >= 1000, unit < units.count - 1 {
            value /= 1000
            unit += 1
        }
        if unit == 0 {
            return "\(number)"
        }
        let digits = value < 10 ? "%.1f%@" : "%.0f%@"
        return String(format: digits, locale: posix, value, units[unit])
            .replacingOccurrences(of: ".0", with: "")
    }

    /// How long something takes, to the precision a person cares about:
    /// "45 s", "12 min", "2 h 5 min".
    static func duration(seconds: UInt64) -> String {
        if seconds < 60 {
            return "\(seconds) s"
        }
        let minutes = (seconds + 30) / 60
        if minutes < 60 {
            return "\(minutes) min"
        }
        let rest = minutes % 60
        return rest == 0 ? "\(minutes / 60) h" : "\(minutes / 60) h \(rest) min"
    }
}

/// Points in time as the daemon and the provider send them.
enum TimeFormat {
    /// The provider's timestamps: ISO 8601 with fractions of a second.
    static func date(iso text: String) -> Date? {
        let withFraction = ISO8601DateFormatter()
        withFraction.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return withFraction.date(from: text) ?? ISO8601DateFormatter().date(from: text)
    }

    static func date(epoch seconds: Int64) -> Date {
        Date(timeIntervalSince1970: TimeInterval(seconds))
    }

    /// "3 days ago", or "just now" for the last minute.
    static func relative(_ date: Date, to now: Date = Date()) -> String {
        if abs(now.timeIntervalSince(date)) < 60 {
            return "just now"
        }
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .full
        return formatter.localizedString(for: date, relativeTo: now)
    }

    /// "12 Nov 2026 at 14:05".
    static func full(_ date: Date) -> String {
        date.formatted(date: .abbreviated, time: .shortened)
    }
}
