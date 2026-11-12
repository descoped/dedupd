import DedupdClient
import SwiftUI

/// Everything Hugging Face says about the model selected in Discover, and
/// the ways to download it.
struct DiscoverInspector: View {
    @Environment(DiscoverStore.self) private var discover
    @State private var tab = InspectorTab.overview

    var body: some View {
        switch discover.detail {
        case .idle where discover.selection == nil:
            ContentUnavailableView(
                "No model selected",
                systemImage: "sidebar.trailing",
                description: Text("Select a model to see its variants, files and description, and to download it.")
            )
        case .idle, .loading:
            ProgressView("Asking Hugging Face…")
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        case let .failed(message):
            ContentUnavailableView {
                Label("The model could not be read", systemImage: "exclamationmark.triangle")
            } description: {
                Text(message)
            } actions: {
                Button("Try Again") {
                    Task { await discover.loadDetail() }
                }
            }
        case let .ready(model):
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    ModelHeader(repo: model.id, meta: model.meta)
                    HubLocalState(model: model)
                    Picker("Section", selection: $tab) {
                        ForEach(InspectorTab.allCases) { tab in
                            Text(tab.rawValue).tag(tab)
                        }
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .accessibilityIdentifier("inspector-tabs")
                    tabContent(model)
                }
                .padding(16)
            }
            .accessibilityIdentifier("discover-inspector")
        }
    }

    @ViewBuilder
    private func tabContent(_ model: HubModel) -> some View {
        switch tab {
        case .overview:
            HubDownloads(model: model)
            InspectorSection(title: "About") {
                MetaFacts(meta: model.meta, commit: model.commit)
                if !model.meta.plainTags.isEmpty {
                    TagCloud(tags: model.meta.plainTags)
                }
            }
            if !model.related.isEmpty {
                InspectorSection(title: "From the same base model") {
                    ForEach(model.related) { hit in
                        Button {
                            discover.open(hit.id)
                        } label: {
                            HStack {
                                Text(hit.id)
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                                Spacer()
                                FormatBadges(formats: hit.meta.formats)
                            }
                            .contentShape(.rect)
                        }
                        .buttonStyle(.plain)
                    }
                }
            }
        case .files:
            InspectorSection(title: "\(ByteFormat.files(UInt64(model.files.count))) in the repository") {
                ForEach(model.files, id: \.path) { file in
                    FileRow(
                        path: file.path,
                        size: file.size,
                        note: file.stored ? "stored on this Mac" : (file.lfs ? "large file" : "small file"),
                        symbol: file.stored ? "internaldrive" : "icloud",
                        digest: file.sha256
                    )
                }
            }
        case .readme:
            if let readme = model.readme, !readme.isEmpty {
                MarkdownText(readme)
            } else {
                Text("This model has no README.")
                    .foregroundStyle(.secondary)
            }
        case .json:
            RawJSONView(json: model.infoJson)
        }
    }
}

/// What this Mac already has of the model.
private struct HubLocalState: View {
    let model: HubModel

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if model.fromArchive {
                Label("Hugging Face did not answer. This is the description dedupd kept of the model.", systemImage: "archivebox")
                    .foregroundStyle(.orange)
            }
            if model.downloading {
                Label("Downloading", systemImage: "arrow.down.circle")
                    .foregroundStyle(Color.accentColor)
            } else if !model.apps.isEmpty {
                let names = model.apps.map { ModelApp.shortTitle(for: $0.app) }
                Label("On this Mac, in \(ListFormatter.localizedString(byJoining: names))", systemImage: "checkmark.circle.fill")
                    .foregroundStyle(.green)
            } else if model.archived {
                Label("In the store; no app has it", systemImage: "archivebox")
                    .foregroundStyle(.secondary)
            }
        }
        .font(.callout)
        .accessibilityIdentifier("hub-local-state")
    }
}

/// The ways to download the model: its variants, grouped by kind, each
/// kind with the apps it goes into.
private struct HubDownloads: View {
    let model: HubModel

    private var groups: [(kind: ModelKind, variants: [Variant])] {
        ModelKind.allCases.compactMap { kind in
            let variants = model.variants.filter { $0.kind(formats: model.meta.formats) == kind }
            return variants.isEmpty ? nil : (kind, variants)
        }
    }

    var body: some View {
        if model.variants.isEmpty {
            InspectorSection(title: "Download") {
                Text("The repository lists no files to download.")
                    .foregroundStyle(.secondary)
            }
        }
        ForEach(groups, id: \.kind) { group in
            InspectorSection(title: group.variants.count == 1 ? "Download" : "Download · \(group.kind.title)") {
                TargetPicker(kind: group.kind, usable: group.variants.first?.usableApps ?? [])
                ForEach(group.variants) { variant in
                    HubVariantRow(variant: variant, kind: group.kind)
                }
            }
        }
    }
}

/// The apps a kind of model is placed into. The choice is remembered for
/// the kind, so the next GGUF goes where the last one went.
private struct TargetPicker: View {
    let kind: ModelKind
    let usable: [ModelApp]

    @Environment(AppSettings.self) private var settings

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 10) {
                Text("Activate in")
                    .foregroundStyle(.secondary)
                ForEach(usable) { app in
                    Toggle(app.shortTitle, isOn: binding(app))
                        .toggleStyle(.checkbox)
                        .help(app.detail)
                        .accessibilityIdentifier("target-\(kind.rawValue)-\(app.rawValue)")
                }
            }
            .font(.callout)
            Text(note)
                .font(.caption)
                .foregroundStyle(.tertiary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private func binding(_ app: ModelApp) -> Binding<Bool> {
        Binding {
            settings.apps(for: kind).contains(app)
        } set: { enabled in
            settings.setApp(app, for: kind, to: enabled)
        }
    }

    private var note: String {
        let others = ModelApp.allCases.filter { !usable.contains($0) }.map(\.shortTitle)
        let remembered = "Remembered for \(kind.title) models."
        if settings.apps(for: kind, usable: usable).isEmpty {
            return "With no app chosen the files are kept in the store only. \(remembered)"
        }
        if others.isEmpty {
            return "Each app gets a clone of the one stored copy. \(remembered)"
        }
        return "\(ListFormatter.localizedString(byJoining: others)) cannot use this kind. \(remembered)"
    }
}

/// One variant: its name and size, and the button that fetches it.
private struct HubVariantRow: View {
    let variant: Variant
    let kind: ModelKind

    @Environment(AppSettings.self) private var settings
    @Environment(DaemonStore.self) private var daemon
    @Environment(DiscoverStore.self) private var discover

    private var chosen: [ModelApp] {
        settings.apps(for: kind, usable: variant.usableApps)
    }

    private var missing: [ModelApp] {
        chosen.filter { !variant.isPlaced(in: $0) }
    }

    /// The download of this variant that is in the queue, if any.
    private var job: Job? {
        daemon.jobs.first { $0.model == variant.spec && ($0.phase.isActive || $0.phase == .paused) }
    }

    var body: some View {
        HStack(spacing: 8) {
            VStack(alignment: .leading, spacing: 1) {
                Text(variant.name)
                    .font(.callout.weight(.medium))
                Text(detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 8)
            action
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("variant-\(variant.name)")
    }

    private var detail: String {
        var parts = [ByteFormat.string(variant.bytes)]
        if variant.files > 1 {
            parts.append(ByteFormat.files(variant.files))
        }
        if !variant.placedIn.isEmpty {
            parts.append("in " + ListFormatter.localizedString(byJoining: variant.placedIn.map(ModelApp.shortTitle(for:))))
        } else if variant.isStored {
            parts.append("in the store")
        }
        return parts.joined(separator: " · ")
    }

    @ViewBuilder private var action: some View {
        if let job {
            HStack(spacing: 6) {
                // A bar only once there is something to show on it.
                if job.phase != .queued, let fraction = job.fraction {
                    ProgressView(value: fraction)
                        .frame(width: 60)
                }
                Text(job.phase == .downloading ? (job.fraction.map { "\(Int($0 * 100)) %" } ?? "…") : job.phase.title)
                    .font(.caption)
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
            }
        } else if variant.isStored, missing.isEmpty {
            Image(systemName: "checkmark.circle.fill")
                .foregroundStyle(.green)
                .help(chosen.isEmpty ? "Stored on this Mac" : "On this Mac, in every app chosen above")
                .accessibilityLabel("On this Mac")
        } else {
            Button(variant.isStored ? "Activate" : "Download") {
                Task { await discover.download(variant, into: variant.isStored ? missing : chosen) }
            }
            .controlSize(.small)
            .help(help)
            .accessibilityIdentifier("download-\(variant.name)")
        }
    }

    private var help: String {
        let apps = variant.isStored ? missing : chosen
        let place = apps.isEmpty
            ? "keeps it in the store"
            : "activates it in \(ListFormatter.localizedString(byJoining: apps.map(\.shortTitle)))"
        return variant.isStored
            ? "Already stored: \(place) as a clone, without downloading"
            : "Downloads \(ByteFormat.string(variant.bytes - min(variant.bytes, variant.storedBytes))) and \(place)"
    }
}
