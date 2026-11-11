import DedupdClient
import SwiftUI

/// The three figures at the top of the panel: what the store holds, how much
/// of the app folders shares its disk blocks, and the state of the archive.
struct SummaryTiles: View {
    let status: DaemonStatus

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Tile(
                title: "Store",
                value: ByteFormat.string(status.storeBytes),
                detail: ByteFormat.files(status.storeFiles),
                identifier: "tile-store"
            )
            Tile(
                title: "Shared with apps",
                value: ByteFormat.string(status.sharedBytes),
                detail: "not stored twice",
                identifier: "tile-shared"
            )
            Tile(
                title: "Archive",
                value: archiveValue,
                detail: archiveDetail,
                identifier: "tile-archive",
                warning: status.archivePath != nil && !status.archiveAttached
            )
        }
    }

    private var archiveValue: String {
        guard status.archivePath != nil else {
            return "None"
        }
        return status.archiveAttached ? "Attached" : "Detached"
    }

    private var archiveDetail: String {
        guard let path = status.archivePath else {
            return "no second copy"
        }
        return path
    }
}

private struct Tile: View {
    let title: String
    let value: String
    let detail: String
    let identifier: String
    var warning = false

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title)
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(value)
                .font(.title3.weight(.semibold))
                .monospacedDigit()
                .foregroundStyle(warning ? Color.orange : Color.primary)
                .lineLimit(1)
                .minimumScaleFactor(0.7)
            Text(detail)
                .font(.caption2)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.middle)
                .help(detail)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(8)
        .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 8))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(title), \(value), \(detail)")
        .accessibilityIdentifier(identifier)
    }
}

/// The bar that shows while the daemon is downloading.
struct DownloadProgress: View {
    let status: DaemonStatus

    var body: some View {
        if status.downloadingTotal > 0 {
            VStack(alignment: .leading, spacing: 4) {
                ProgressView(
                    value: Double(min(status.downloadingDone, status.downloadingTotal)),
                    total: Double(status.downloadingTotal)
                )
                Text(
                    "Downloading: \(ByteFormat.string(status.downloadingDone)) of \(ByteFormat.string(status.downloadingTotal))"
                )
                .font(.caption)
                .foregroundStyle(.secondary)
                .monospacedDigit()
            }
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier("download-progress")
        }
    }
}
