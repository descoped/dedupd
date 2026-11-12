import DedupdClient
import SwiftUI

/// The files of each revision dedupd keeps, and where the apps have them.
struct LibraryFiles: View {
    let detail: ModelDetail

    var body: some View {
        ForEach(detail.revisions, id: \.commit) { revision in
            InspectorSection(title: "Revision \(revision.commit.prefix(12))") {
                Text(Self.caption(revision))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                ForEach(revision.files, id: \.path) { file in
                    FileRow(
                        path: file.path,
                        size: file.size,
                        note: Self.note(file),
                        symbol: file.stored ? "internaldrive" : (file.archived ? "externaldrive" : "icloud"),
                        digest: file.sha256
                    )
                }
            }
        }
        if !detail.placements.isEmpty {
            InspectorSection(title: "In the apps") {
                ForEach(ModelApp.allCases) { app in
                    let files = detail.placements.filter { $0.app == app.rawValue }
                    if !files.isEmpty {
                        PlacementGroup(app: app, files: files)
                    }
                }
            }
        }
    }

    private static func caption(_ revision: Revision) -> String {
        var parts = [ByteFormat.string(revision.bytes), "added \(TimeFormat.full(TimeFormat.date(epoch: revision.fetchedAt)))"]
        if !revision.refs.isEmpty {
            parts.insert(revision.refs.joined(separator: ", "), at: 0)
        }
        return parts.joined(separator: " · ")
    }

    private static func note(_ file: RevisionFile) -> String {
        let place = file.stored ? "stored" : (file.archived ? "on the archive disk only" : "not downloaded")
        let apps = file.apps.map(ModelApp.shortTitle(for:))
        return apps.isEmpty ? place : "\(place) · in \(ListFormatter.localizedString(byJoining: apps))"
    }
}

/// One file: its path, its size and a line on where it is.
struct FileRow: View {
    let path: String
    let size: UInt64
    let note: String
    var symbol = "doc"
    var digest: String?

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: symbol)
                .foregroundStyle(.secondary)
                .frame(width: 16)
            VStack(alignment: .leading, spacing: 1) {
                Text(path)
                    .font(.callout)
                    .lineLimit(2)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
                Text(note)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 8)
            Text(ByteFormat.string(size))
                .font(.callout)
                .monospacedDigit()
                .foregroundStyle(.secondary)
        }
        .help(digest.map { "sha256 \($0)" } ?? path)
        .contextMenu {
            Button("Copy Path") {
                Pasteboard.copy(path)
            }
            if let digest {
                Button("Copy sha256") {
                    Pasteboard.copy(digest)
                }
            }
        }
    }
}

/// The files one app has of the model, with the way to see them in Finder.
private struct PlacementGroup: View {
    let app: ModelApp
    let files: [Placement]

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Label(app.title, systemImage: app.symbol)
                    .font(.callout.weight(.medium))
                Spacer()
                if let first = files.first {
                    Button("Show in Finder") {
                        Finder.reveal(first.path)
                    }
                    .controlSize(.small)
                }
            }
            ForEach(files, id: \.path) { file in
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Image(systemName: file.shared ? "square.on.square" : "doc")
                        .foregroundStyle(file.shared ? Color.green : .secondary)
                        .help(file.shared ? "A clone of the stored copy: takes no extra space" : "A copy of its own")
                    Text(file.path)
                        .font(.caption.monospaced())
                        .lineLimit(2)
                        .truncationMode(.middle)
                        .textSelection(.enabled)
                    Spacer(minLength: 4)
                    Text(Self.state(file))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
    }

    private static func state(_ file: Placement) -> String {
        switch file.verifyState {
        case "verified": "verified"
        case "mismatch": "differs"
        case "not_upstream": "not on Hugging Face"
        default: "not checked"
        }
    }
}

/// Asks the owner to confirm removing a model from an app, showing exactly
/// what would be deleted.
struct RemovalSheet: View {
    let plan: UnplacePlan

    @Environment(LibraryStore.self) private var library

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Deactivate \(plan.repo) in \(ModelApp.title(for: plan.app))?")
                .font(.headline)
            Text(summary)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            ScrollView {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(plan.remove, id: \.path) { file in
                        HStack(alignment: .firstTextBaseline) {
                            Text(file.path)
                                .font(.caption.monospaced())
                                .lineLimit(2)
                                .truncationMode(.middle)
                            Spacer(minLength: 8)
                            Text(ByteFormat.string(file.size))
                                .font(.caption)
                                .monospacedDigit()
                                .foregroundStyle(.secondary)
                        }
                    }
                    ForEach(plan.links, id: \.self) { link in
                        Text(link)
                            .font(.caption.monospaced())
                            .lineLimit(2)
                            .truncationMode(.middle)
                    }
                    if !plan.keep.isEmpty {
                        Text("Left in place")
                            .font(.caption.weight(.semibold))
                            .padding(.top, 6)
                        ForEach(plan.keep, id: \.path) { file in
                            Text("\(file.path): \(file.why)")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(8)
            }
            .frame(height: 180)
            .background(.quaternary.opacity(0.4), in: .rect(cornerRadius: 6))
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) {
                    library.removal = nil
                }
                .keyboardShortcut(.cancelAction)
                Button("Deactivate", role: .destructive) {
                    Task { await library.applyRemoval() }
                }
                .disabled(plan.remove.isEmpty && plan.links.isEmpty)
                .accessibilityIdentifier("confirm-removal")
            }
        }
        .padding(20)
        .frame(width: 520)
    }

    private var summary: String {
        let count = UInt64(plan.remove.count)
        if count == 0, plan.links.isEmpty {
            return "Nothing can be removed: every file is still needed or has no stored copy."
        }
        return "\(ByteFormat.files(count)), \(ByteFormat.string(plan.bytes)), are deleted from the app's folder. "
            + "The stored copy stays, so the model can be activated again without downloading."
    }
}
