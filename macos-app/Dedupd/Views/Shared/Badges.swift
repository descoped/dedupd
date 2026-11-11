import DedupdClient
import SwiftUI

/// A small rounded label: a format, a tag, a state.
struct Tag: View {
    let text: String
    var tint: Color = .secondary

    var body: some View {
        Text(text)
            .font(.caption2.weight(.medium))
            .lineLimit(1)
            .fixedSize()
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .foregroundStyle(tint)
            .background(tint.opacity(0.14), in: .rect(cornerRadius: 4))
    }
}

/// The formats a model comes in, as tags.
struct FormatBadges: View {
    let formats: [String]

    var body: some View {
        HStack(spacing: 4) {
            ForEach(formats, id: \.self) { format in
                Tag(text: FormatBadge.title(for: format), tint: FormatBadge.tint(for: format))
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(formats.map(FormatBadge.title(for:)).joined(separator: ", "))
    }
}

/// The one format that says how a model is used, for a table row.
struct PrimaryFormat: View {
    let meta: ModelMeta

    var body: some View {
        if let format = meta.primaryFormat {
            Tag(text: FormatBadge.title(for: format), tint: FormatBadge.tint(for: format))
                .help(meta.formats.map(FormatBadge.title(for:)).joined(separator: ", "))
                .accessibilityLabel(FormatBadge.title(for: format))
        }
    }
}

/// The apps that have a model, as their short names.
struct AppBadges: View {
    let apps: [String]
    /// What to show when no app has it.
    var none = "—"

    var body: some View {
        if apps.isEmpty {
            Text(none)
                .foregroundStyle(.tertiary)
        } else {
            HStack(spacing: 4) {
                ForEach(apps, id: \.self) { app in
                    Tag(text: ModelApp.badge(for: app), tint: .primary)
                }
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(apps.map(ModelApp.shortTitle(for:)).joined(separator: ", "))
        }
    }
}

/// A model's state against Hugging Face: a symbol and, when there is room,
/// the words.
struct HealthLabel: View {
    let health: ModelHealth
    var showsText = true

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: health.symbol)
                .foregroundStyle(health.tint)
            if showsText {
                Text(health.label)
            }
        }
        .help(health.label)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(health.label)
    }
}

/// A titled block of an inspector.
struct InspectorSection<Content: View>: View {
    let title: String
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title)
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
                .textCase(.uppercase)
                .accessibilityAddTraits(.isHeader)
            content
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// One fact about a model: its name on the left, its value on the right.
/// Used inside a `Grid`.
struct FactRow: View {
    let label: String
    let value: String
    var monospaced = false

    init(_ label: String, _ value: String, monospaced: Bool = false) {
        self.label = label
        self.value = value
        self.monospaced = monospaced
    }

    var body: some View {
        GridRow(alignment: .firstTextBaseline) {
            Text(label)
                .foregroundStyle(.secondary)
                .gridColumnAlignment(.trailing)
            Text(value)
                .font(monospaced ? .callout.monospaced() : .callout)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .font(.callout)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(label): \(value)")
    }
}

/// The facts Hugging Face gives about a model, the ones that are known.
struct MetaFacts: View {
    let meta: ModelMeta
    var commit: String?

    var body: some View {
        Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 12, verticalSpacing: 5) {
            if let task = meta.pipelineTag { FactRow("Task", task) }
            if let library = meta.libraryName { FactRow("Library", library) }
            if let parameters = meta.parameters { FactRow("Parameters", ByteFormat.parameters(parameters)) }
            if let architecture = meta.architecture { FactRow("Architecture", architecture) }
            if let context = meta.contextLength { FactRow("Context", "\(context.formatted()) tokens") }
            if let license = meta.license { FactRow("Licence", license) }
            if let base = meta.baseModel { FactRow("Based on", base) }
            if !meta.languages.isEmpty { FactRow("Languages", meta.languages.joined(separator: ", ")) }
            if let downloads = meta.downloads { FactRow("Downloads", ByteFormat.count(downloads)) }
            if let likes = meta.likes { FactRow("Likes", ByteFormat.count(likes)) }
            if let size = meta.usedStorage { FactRow("Repository", ByteFormat.string(size)) }
            if let modified = meta.modified { FactRow("Updated", TimeFormat.full(modified)) }
            if let created = meta.createdAt.flatMap(TimeFormat.date(iso:)) { FactRow("Created", TimeFormat.full(created)) }
            if meta.gated { FactRow("Access", "Gated: the terms must be accepted on Hugging Face") }
            if let commit { FactRow("Revision", String(commit.prefix(12)), monospaced: true) }
        }
    }
}

/// Tags that wrap onto as many lines as they need.
struct TagCloud: View {
    let tags: [String]

    var body: some View {
        FlowLayout(spacing: 4) {
            ForEach(tags, id: \.self) { tag in
                Tag(text: tag)
            }
        }
    }
}

/// Lays its children out in rows, starting a new row when one is full.
struct FlowLayout: Layout {
    var spacing: CGFloat = 4

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let rows = arrange(subviews, width: proposal.width ?? .infinity)
        return CGSize(width: proposal.width ?? rows.width, height: rows.height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let rows = arrange(subviews, width: bounds.width)
        for (subview, point) in zip(subviews, rows.points) {
            subview.place(at: CGPoint(x: bounds.minX + point.x, y: bounds.minY + point.y), proposal: .unspecified)
        }
    }

    private struct Arrangement {
        var points: [CGPoint]
        var width: CGFloat
        var height: CGFloat
    }

    private func arrange(_ subviews: Subviews, width: CGFloat) -> Arrangement {
        var points: [CGPoint] = []
        var origin = CGPoint.zero
        var rowHeight: CGFloat = 0
        var widest: CGFloat = 0
        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if origin.x > 0, origin.x + size.width > width {
                origin = CGPoint(x: 0, y: origin.y + rowHeight + spacing)
                rowHeight = 0
            }
            points.append(origin)
            origin.x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
            widest = max(widest, origin.x - spacing)
        }
        return Arrangement(points: points, width: widest, height: origin.y + rowHeight)
    }
}
