import AppKit
import SwiftUI

/// A piece of a README.
enum MarkdownBlock: Equatable, Sendable {
    case heading(level: Int, text: String)
    case code(String)
    case paragraph(String)
    case item(String)
}

/// Enough of Markdown to read a model card: headings, paragraphs, lists
/// and code. Anything else is kept as written.
enum Markdown {
    /// Split a README into blocks. The metadata block Hugging Face keeps at
    /// the top of the file is left out; the app shows it as fields.
    static func parse(_ source: String) -> [MarkdownBlock] {
        var lines = source.components(separatedBy: .newlines)[...]
        if lines.first?.trimmingCharacters(in: .whitespaces) == "---",
           let end = lines.dropFirst().firstIndex(where: { $0.trimmingCharacters(in: .whitespaces) == "---" }) {
            lines = lines[(end + 1)...]
        }
        var blocks: [MarkdownBlock] = []
        var paragraph: [String] = []
        var code: [String]?
        func flush() {
            if !paragraph.isEmpty {
                blocks.append(.paragraph(paragraph.joined(separator: " ")))
                paragraph = []
            }
        }
        for line in lines {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.hasPrefix("```") {
                if let open = code {
                    blocks.append(.code(open.joined(separator: "\n")))
                    code = nil
                } else {
                    flush()
                    code = []
                }
            } else if code != nil {
                code?.append(line)
            } else if trimmed.isEmpty {
                flush()
            } else if trimmed.hasPrefix("#") {
                flush()
                let level = trimmed.prefix { $0 == "#" }.count
                blocks.append(.heading(level: level, text: trimmed.drop { $0 == "#" }.trimmingCharacters(in: .whitespaces)))
            } else if trimmed.hasPrefix("- ") || trimmed.hasPrefix("* ") {
                flush()
                blocks.append(.item(String(trimmed.dropFirst(2))))
            } else {
                paragraph.append(trimmed)
            }
        }
        if let open = code {
            blocks.append(.code(open.joined(separator: "\n")))
        }
        flush()
        return blocks
    }
}

/// A model card as its authors wrote it.
struct MarkdownText: View {
    let blocks: [MarkdownBlock]

    init(_ source: String) {
        blocks = Markdown.parse(source)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                switch block {
                case let .heading(level, text):
                    Text(Self.inline(text))
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
                    Text(Self.inline(text))
                case let .item(text):
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        Text("•")
                            .foregroundStyle(.secondary)
                        Text(Self.inline(text))
                    }
                }
            }
        }
        .font(.callout)
        .textSelection(.enabled)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// Emphasis, code and links inside a line.
    private static func inline(_ text: String) -> AttributedString {
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        return (try? AttributedString(markdown: text, options: options)) ?? AttributedString(text)
    }
}

/// JSON as the provider sent it, indented, with a button to copy it.
struct RawJSONView: View {
    let json: String

    private var pretty: String {
        guard let object = try? JSONSerialization.jsonObject(with: Data(json.utf8)),
              let data = try? JSONSerialization.data(withJSONObject: object, options: [.prettyPrinted, .sortedKeys]),
              let text = String(bytes: data, encoding: .utf8)
        else {
            return json
        }
        return text
    }

    var body: some View {
        let text = pretty
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
    }
}

enum Pasteboard {
    @MainActor
    static func copy(_ text: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }
}

enum Finder {
    /// Show a file or folder in the Finder; a path that is not there opens
    /// the nearest folder that is.
    @MainActor
    static func reveal(_ path: String) {
        var url = URL(fileURLWithPath: path)
        while !FileManager.default.fileExists(atPath: url.path), url.path != "/" {
            url.deleteLastPathComponent()
        }
        NSWorkspace.shared.activateFileViewerSelecting([url])
    }
}

enum SystemSettings {
    /// Where macOS lists what opens at login and what runs in the background.
    static let loginItems = URL(string: "x-apple.systempreferences:com.apple.LoginItems-Settings.extension")

    /// Open System Settings › General › Login Items & Extensions, where the
    /// owner can see the daemon and this app and switch either off.
    @MainActor
    static func openLoginItems() {
        if let loginItems {
            NSWorkspace.shared.open(loginItems)
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
