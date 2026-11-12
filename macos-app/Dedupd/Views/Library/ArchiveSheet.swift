import DedupdClient
import SwiftUI

/// Asks the owner to confirm archiving a model, after showing what it does.
struct ArchiveSheet: View {
    let plan: ArchivePlan

    @Environment(LibraryStore.self) private var library

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Archive \(plan.repo)?")
                .font(.headline)
            if plan.isEmpty {
                Text("Nothing would change: no app has the model, and none of its files on the internal disk can go.")
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(Array(plan.steps.enumerated()), id: \.offset) { index, step in
                        HStack(alignment: .firstTextBaseline, spacing: 6) {
                            Text("\(index + 1).")
                                .monospacedDigit()
                                .foregroundStyle(.secondary)
                            Text(step)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(plan.steps.joined(separator: " "))
                .accessibilityIdentifier("archive-steps")
            }
            if hasNotes {
                ScrollView {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Left in place")
                            .font(.caption.weight(.semibold))
                        ForEach(plan.deactivate, id: \.app) { step in
                            ForEach(step.keep, id: \.path) { file in
                                Text("\(file.path): \(file.why)")
                            }
                        }
                        ForEach(plan.keep, id: \.sha256) { file in
                            Text("\(ByteFormat.string(file.size)) on the internal disk (\(file.sha256.prefix(12))): \(file.why)")
                        }
                    }
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(8)
                }
                .frame(maxHeight: 120)
                .background(.quaternary.opacity(0.4), in: .rect(cornerRadius: 6))
            }
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) {
                    library.archiving = nil
                }
                .keyboardShortcut(.cancelAction)
                Button("Archive", role: .destructive) {
                    Task { await library.applyArchive() }
                }
                .disabled(plan.isEmpty)
                .accessibilityIdentifier("confirm-archive")
            }
        }
        .padding(20)
        .frame(width: 520)
    }

    private var hasNotes: Bool {
        !plan.keep.isEmpty || plan.deactivate.contains { !$0.keep.isEmpty }
    }
}

/// How far the archive or restore of the selected model is, or why it failed.
struct MoveStatus: View {
    let move: ModelMove

    var body: some View {
        if move.error != nil {
            FailureLabel(message: move.statusText)
                .accessibilityIdentifier("move-status")
        } else {
            VStack(alignment: .leading, spacing: 4) {
                if let fraction = move.fraction {
                    ProgressView(value: fraction)
                } else {
                    ProgressView()
                        .controlSize(.small)
                }
                Text(move.statusText)
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(move.statusText)
            .accessibilityIdentifier("move-status")
        }
    }
}
