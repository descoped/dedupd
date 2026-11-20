import Foundation

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
    /// Whether a link in a README may be opened. A README is written by
    /// whoever published the model, and a link shows only its words: only an
    /// address on the web is opened, never a file, a network share or
    /// another app's own kind of address.
    static func mayOpen(_ url: URL) -> Bool {
        guard let scheme = url.scheme?.lowercased(), let host = url.host(), !host.isEmpty else {
            return false
        }
        return scheme == "https" || scheme == "http"
    }

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

    /// Emphasis, code and links inside a line.
    static func inline(_ text: String) -> AttributedString {
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        return (try? AttributedString(markdown: text, options: options)) ?? AttributedString(text)
    }

    /// JSON indented and with its keys in order; text that is not JSON as
    /// it came.
    static func indented(json: String) -> String {
        guard let object = try? JSONSerialization.jsonObject(with: Data(json.utf8)),
              let data = try? JSONSerialization.data(withJSONObject: object, options: [.prettyPrinted, .sortedKeys]),
              let text = String(bytes: data, encoding: .utf8)
        else {
            return json
        }
        return text
    }
}
