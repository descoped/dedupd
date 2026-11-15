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
                let names = model.apps.map(\.app.shortTitle)
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

    private var groups: [(format: ModelFormat, variants: [Variant])] {
        ModelFormat.allCases.compactMap { format in
            let variants = model.variants.filter { $0.format == format }
            return variants.isEmpty ? nil : (format, variants)
        }
    }

    /// The apps at least one of the variants can go into. Each row then
    /// sends only to the chosen apps that can take its own variant: a GGUF
    /// in parts is not sent to Ollama.
    private func usable(_ variants: [Variant]) -> [ModelApp] {
        ModelApp.allCases.filter { app in variants.contains { $0.usableBy.contains(app) } }
    }

    var body: some View {
        if model.variants.isEmpty {
            InspectorSection(title: "Download") {
                Text("The repository lists no files to download.")
                    .foregroundStyle(.secondary)
            }
        }
        ForEach(groups, id: \.format) { group in
            InspectorSection(title: group.variants.count == 1 ? "Download" : "Download · \(group.format.title)") {
                TargetPicker(format: group.format, usable: usable(group.variants))
                ForEach(group.variants) { variant in
                    HubVariantRow(variant: variant, format: group.format)
                }
            }
        }
    }
}

/// The apps a format is placed into. The choice is remembered for the
/// format, so the next GGUF goes where the last one went.
private struct TargetPicker: View {
    let format: ModelFormat
    let usable: [ModelApp]

    @Environment(AppSettings.self) private var settings
    @Environment(DaemonStore.self) private var daemon

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 10) {
                Text("Activate in")
                    .foregroundStyle(.secondary)
                ForEach(usable) { app in
                    Toggle(app.shortTitle, isOn: binding(app))
                        .toggleStyle(.checkbox)
                        .disabled(daemon.apps.isEmpty)
                        .help(app.detail)
                        .accessibilityIdentifier("target-\(format.name)-\(app.name)")
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
            settings.apps(for: format, known: daemon.apps).contains(app)
        } set: { enabled in
            settings.setApp(app, for: format, to: enabled, known: daemon.apps)
        }
    }

    private var note: String {
        let others = ModelApp.allCases.filter { !usable.contains($0) }.map(\.shortTitle)
        let remembered = "Remembered for \(format.title) models."
        if daemon.apps.isEmpty {
            return "Waiting for dedupd to say which apps take \(format.title) models."
        }
        if settings.apps(for: format, usable: usable, known: daemon.apps).isEmpty {
            return "With no app chosen the files are kept in the store only. \(remembered)"
        }
        if others.isEmpty {
            return "Each app gets a clone of the one stored copy. \(remembered)"
        }
        return "\(ListFormatter.localizedString(byJoining: others)) cannot use this model. \(remembered)"
    }
}

/// One variant: its name and size, and the button that fetches it.
private struct HubVariantRow: View {
    let variant: Variant
    let format: ModelFormat

    @Environment(AppSettings.self) private var settings
    @Environment(DaemonStore.self) private var daemon
    @Environment(DiscoverStore.self) private var discover

    private var chosen: [ModelApp] {
        settings.apps(for: format, usable: variant.usableApps, known: daemon.apps)
    }

    private var missing: [ModelApp] {
        chosen.filter { !variant.isPlaced(in: $0) }
    }

    /// The download of this variant that is in the queue, if any.
    private var job: Job? {
        daemon.jobs.first { $0.model == variant.spec && ($0.state.isActive || $0.state == .paused) }
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
            parts.append("in " + ListFormatter.localizedString(byJoining: variant.placedIn.map(\.shortTitle)))
        } else if variant.canActivate {
            parts.append("in the store")
        }
        return parts.joined(separator: " · ")
    }

    @ViewBuilder private var action: some View {
        if let job {
            HStack(spacing: 6) {
                // A bar only once there is something to show on it.
                if job.state != .queued, let fraction = job.fraction {
                    ProgressView(value: fraction)
                        .frame(width: 60)
                }
                Text(job.state == .downloading ? (job.fraction.map { "\(Int($0 * 100)) %" } ?? "…") : job.state.title)
                    .font(.caption)
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
            }
        } else if variant.canActivate, missing.isEmpty {
            Image(systemName: "checkmark.circle.fill")
                .foregroundStyle(.green)
                .help(chosen.isEmpty ? "Stored on this Mac" : "On this Mac, in every app chosen above")
                .accessibilityLabel("On this Mac")
        } else {
            Button(variant.canActivate ? "Activate" : "Download") {
                Task { await discover.download(variant, into: variant.canActivate ? missing : chosen) }
            }
            .controlSize(.small)
            // Until the daemon has said which apps take the model, the apps
            // it would go to are not known; it would go to none.
            .disabled(daemon.apps.isEmpty)
            .help(daemon.apps.isEmpty ? "Waiting for dedupd to say which apps take this model" : help)
            .accessibilityIdentifier("download-\(variant.name)")
        }
    }

    private var help: String {
        let apps = variant.canActivate ? missing : chosen
        let place = apps.isEmpty
            ? "keeps it in the store"
            : "activates it in \(ListFormatter.localizedString(byJoining: apps.map(\.shortTitle)))"
        if variant.isStored {
            return "Already stored: \(place) as a clone, without downloading"
        }
        if variant.weightsStored {
            let lacking = ByteFormat.string(variant.missingBytes)
            return "Already stored: \(place) as a clone, after fetching the \(lacking) of small files it lacks"
        }
        return "Downloads \(ByteFormat.string(variant.missingBytes)) and \(place)"
    }
}
