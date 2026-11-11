import Foundation

/// The screens of the main window, as the sidebar lists them.
enum Pane: String, CaseIterable, Identifiable, Sendable {
    case library
    case discover
    case downloads
    case storage
    case daemon

    var id: String { rawValue }

    var title: String {
        switch self {
        case .library: "Library"
        case .discover: "Discover"
        case .downloads: "Downloads"
        case .storage: "Storage"
        case .daemon: "Daemon"
        }
    }

    var symbol: String {
        switch self {
        case .library: "books.vertical"
        case .discover: "magnifyingglass"
        case .downloads: "arrow.down.circle"
        case .storage: "internaldrive"
        case .daemon: "gearshape.2"
        }
    }

    /// What the screen is for, shown under its title.
    var subtitle: String {
        switch self {
        case .library: "The models on this Mac and where each app has them"
        case .discover: "Search Hugging Face and look at a model before downloading it"
        case .downloads: "What is being downloaded, and how hard the server is being asked"
        case .storage: "Where the files are and what can be cleaned up"
        case .daemon: "The background service that does the work"
        }
    }
}

/// How much room a table row takes.
enum RowDensity: String, CaseIterable, Identifiable, Sendable {
    /// One line per row.
    case compact
    /// Two lines per row: the name, and what else there is to say under it.
    case comfortable

    var id: String { rawValue }

    var title: String {
        switch self {
        case .compact: "Compact rows"
        case .comfortable: "Larger rows"
        }
    }

    var symbol: String {
        switch self {
        case .compact: "list.bullet"
        case .comfortable: "list.bullet.rectangle"
        }
    }
}
