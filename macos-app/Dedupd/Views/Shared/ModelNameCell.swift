import SwiftUI

/// The widths of the columns that hold one short thing, the same in every
/// table of models. Each is as wide as the longest thing it shows.
enum TableWidth {
    /// A format tag; "Safetensors" is the longest.
    static let format: CGFloat = 68
    /// "156.38 GB".
    static let size: CGFloat = 70
    /// The short names of up to three apps.
    static let apps: CGFloat = 124
    /// One symbol under the word "State".
    static let state: CGFloat = 40
    /// "45 minutes ago".
    static let time: CGFloat = 100
}

/// A model's name in a table. Compact rows show `organisation/name` on one
/// line; larger rows put the name first and who made it, with the licence,
/// on a second line.
struct ModelNameCell: View {
    let name: String
    let organisation: String
    let detail: String
    let comfortable: Bool

    var body: some View {
        Group {
            if comfortable {
                VStack(alignment: .leading, spacing: 2) {
                    Text(name)
                        .fontWeight(.medium)
                        .truncationMode(.middle)
                    Text([organisation, detail].filter { !$0.isEmpty }.joined(separator: " · "))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .truncationMode(.tail)
                }
                .padding(.vertical, 4)
            } else {
                // `organisation/name` when the column has room for all of
                // it; otherwise the model's own name, which is the part that
                // tells two rows apart. Never half an organisation.
                ViewThatFits(in: .horizontal) {
                    Text("\(Text(organisation + "/").foregroundStyle(.secondary))\(name)")
                        .fixedSize()
                    Text(name)
                        .truncationMode(.middle)
                }
                .help("\(organisation)/\(name)")
            }
        }
        .lineLimit(1)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(comfortable && !detail.isEmpty ? "\(organisation)/\(name), \(detail)" : "\(organisation)/\(name)")
    }
}
