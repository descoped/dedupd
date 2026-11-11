import DedupdClient
import SwiftUI

/// The form that fetches a model and places it into the chosen apps.
struct PullSection: View {
    @Environment(DaemonStore.self) private var store
    @State private var form = PullForm()

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionTitle(text: "Get a model")
            HStack(spacing: 8) {
                TextField("org/repo or org/repo:Q4_K_M", text: $form.model)
                    .textFieldStyle(.roundedBorder)
                    .autocorrectionDisabled()
                    .onSubmit(send)
                    .accessibilityIdentifier("pull-model")
                Button("Pull", action: send)
                    .buttonStyle(.borderedProminent)
                    .disabled(form.request == nil || store.isPulling)
                    .accessibilityIdentifier("pull-button")
            }
            if let problem = form.problem {
                Text(problem)
                    .font(.caption)
                    .foregroundStyle(.orange)
            }
            HStack(spacing: 14) {
                ForEach(ModelApp.allCases) { app in
                    Toggle(app.title, isOn: binding(for: app))
                        .toggleStyle(.checkbox)
                        .help(app.detail)
                }
            }
            .font(.callout)
            Toggle("Use only what is archived (no download)", isOn: $form.offline)
                .toggleStyle(.checkbox)
                .font(.callout)
        }
    }

    private func binding(for app: ModelApp) -> Binding<Bool> {
        Binding(
            get: { form.apps.contains(app) },
            set: { isOn in
                if isOn {
                    form.apps.insert(app)
                } else {
                    form.apps.remove(app)
                }
            }
        )
    }

    private func send() {
        guard let request = form.request, !store.isPulling else {
            return
        }
        Task { await store.pull(request) }
    }
}

/// What the last pull and the last scan did, until dismissed.
struct ActivityMessages: View {
    @Environment(DaemonStore.self) private var store

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            pull
            scan
        }
    }

    @ViewBuilder private var pull: some View {
        switch store.pullActivity {
        case .idle:
            EmptyView()
        case let .running(model):
            Message(kind: .working, title: "Fetching \(model)…", lines: [], dismiss: nil)
        case let .finished(result):
            Message(
                kind: result.warnings.isEmpty ? .done : .warning,
                title: result.repo,
                lines: [result.summary] + result.placements + result.warnings,
                dismiss: store.dismissPull
            )
            .accessibilityIdentifier("pull-result")
        case let .failed(model, message):
            Message(kind: .failed, title: "Could not fetch \(model)", lines: [message], dismiss: store.dismissPull)
                .accessibilityIdentifier("pull-error")
        }
    }

    @ViewBuilder private var scan: some View {
        switch store.scanActivity {
        case .idle:
            EmptyView()
        case .running:
            Message(kind: .working, title: "Looking at the app folders…", lines: [], dismiss: nil)
        case let .finished(result):
            Message(
                kind: result.errors.isEmpty ? .done : .warning,
                title: "Scan finished",
                lines: [result.summary] + result.errors,
                dismiss: store.dismissScan
            )
            .accessibilityIdentifier("scan-result")
        case let .failed(message):
            Message(kind: .failed, title: "Scan failed", lines: [message], dismiss: store.dismissScan)
        }
    }
}

private struct Message: View {
    enum Kind {
        case working, done, warning, failed
    }

    let kind: Kind
    let title: String
    let lines: [String]
    let dismiss: (() -> Void)?

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            icon.frame(width: 18)
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.callout.weight(.medium))
                    .lineLimit(2)
                ForEach(lines, id: \.self) { line in
                    Text(line)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                }
            }
            Spacer(minLength: 4)
            if let dismiss {
                Button(action: dismiss) {
                    Image(systemName: "xmark")
                }
                .buttonStyle(.borderless)
                .controlSize(.small)
                .accessibilityLabel("Dismiss")
            }
        }
        .padding(8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 8))
    }

    @ViewBuilder private var icon: some View {
        switch kind {
        case .working:
            ProgressView().controlSize(.small)
        case .done:
            Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
        case .warning:
            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
        case .failed:
            Image(systemName: "xmark.octagon.fill").foregroundStyle(.red)
        }
    }
}
