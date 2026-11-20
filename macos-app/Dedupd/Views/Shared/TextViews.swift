import AppKit
import SwiftUI

/// A model card as its authors wrote it.
///
/// The text is read into blocks when it changes, not each time the
/// inspector around it is drawn again.
struct MarkdownText: View {
    let source: String
    @State private var lines: [Line] = []

    /// A block with its inline marks already read.
    private enum Line {
        case heading(level: Int, AttributedString)
        case code(String)
        case paragraph(AttributedString)
        case item(AttributedString)

        init(_ block: MarkdownBlock) {
            self = switch block {
            case let .heading(level, text): .heading(level: level, Markdown.inline(text))
            case let .code(text): .code(text)
            case let .paragraph(text): .paragraph(Markdown.inline(text))
            case let .item(text): .item(Markdown.inline(text))
            }
        }
    }

    init(_ source: String) {
        self.source = source
    }

    var body: some View {
        content
            .task(id: source) {
                lines = Markdown.parse(source).map(Line.init)
            }
    }

    private var content: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(lines.enumerated()), id: \.offset) { _, block in
                switch block {
                case let .heading(level, text):
                    Text(text)
                        .font(level <= 1 ? .title3.weight(.semibold) : .headline)
                        .padding(.top, 4)
                        .accessibilityAddTraits(.isHeader)
                case let .code(text):
                    Text(text)
                        .font(.caption.monospaced())
                        .padding(8)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(.quaternary.opacity(0.5), in: .rect(cornerRadius: 6))
                case let .paragraph(text):
                    Text(text)
                case let .item(text):
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        Text("•")
                            .foregroundStyle(.secondary)
                        Text(text)
                    }
                }
            }
        }
        .font(.callout)
        .textSelection(.enabled)
        .frame(maxWidth: .infinity, alignment: .leading)
        .environment(\.openURL, OpenURLAction { url in
            Markdown.mayOpen(url) ? .systemAction : .discarded
        })
    }
}

/// JSON as the provider sent it, indented, with a button to copy it.
struct RawJSONView: View {
    let json: String
    /// The JSON indented; made when it changes, not on every drawing.
    @State private var text = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button("Copy", systemImage: "doc.on.doc") {
                Pasteboard.copy(text)
            }
            .controlSize(.small)
            Text(text)
                .font(.caption.monospaced())
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .task(id: json) {
            text = Markdown.indented(json: json)
        }
    }
}

/// What a screen shows instead of its content while the daemon is away.
struct DaemonAwayView: View {
    @Environment(DaemonStore.self) private var daemon
    @Environment(Navigation.self) private var navigation

    var body: some View {
        if daemon.connection == .connecting {
            ProgressView("Looking for the daemon…")
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if case let .otherBuild(reason) = daemon.connection {
            ContentUnavailableView {
                Label("The daemon is another build", systemImage: "exclamationmark.triangle")
            } description: {
                Text(
                    verbatim: "The daemon at \(daemon.address) is another build of dedupd, and this app cannot read what it says. "
                        + "Update it on the Daemon screen. Your models are untouched."
                )
                Text(verbatim: reason)
                    .font(.caption)
                    .foregroundStyle(.tertiary)
            } actions: {
                Button("Go to the Daemon Screen") {
                    navigation.pane = .daemon
                }
                .accessibilityIdentifier("go-to-daemon")
            }
        } else {
            ContentUnavailableView {
                Label("The daemon is not running", systemImage: "square.stack.3d.up.slash")
            } description: {
                Text("No dedupd daemon answers at \(daemon.address). Your models are untouched; the apps keep using them.")
            } actions: {
                Button("Set Up the Daemon") {
                    navigation.pane = .daemon
                }
                .accessibilityIdentifier("go-to-daemon")
            }
        }
    }
}

/// A line that says something failed, with what failed.
struct FailureLabel: View {
    let message: String

    var body: some View {
        Label(message, systemImage: "exclamationmark.triangle.fill")
            .symbolRenderingMode(.multicolor)
            .font(.callout)
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}
