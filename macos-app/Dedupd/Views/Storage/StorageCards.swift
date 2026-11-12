import DedupdClient
import SwiftUI

/// What can be removed from the store, and the button that removes it
/// after the owner has seen the list.
struct CleanUpCard: View {
    @Environment(StorageStore.self) private var storage

    var body: some View {
        @Bindable var storage = storage
        Card(
            title: "Clean up",
            note: "Nothing is deleted by itself. This lists stored files that no model in the library and no app uses any more."
        ) {
            Toggle(
                "Also list files that can be moved off this Mac: the archive disk has them and no app uses them",
                isOn: $storage.includeOffload
            )
            .onChange(of: storage.includeOffload) {
                Task { await storage.loadPlan() }
            }
            switch storage.plan {
            case let .ready(plan):
                summary(plan)
            case let .failed(message):
                FailureLabel(message: message)
            case .idle, .loading:
                ProgressView()
                    .controlSize(.small)
            }
            if let freed = storage.freed {
                Label("The last clean-up freed \(ByteFormat.string(freed)).", systemImage: "checkmark.circle")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
        }
        .sheet(isPresented: $storage.confirming) {
            if let plan = storage.plan.value {
                CleanUpSheet(plan: plan)
            }
        }
    }

    @ViewBuilder
    private func summary(_ plan: GcPlan) -> some View {
        if plan.orphans.isEmpty, plan.offload.isEmpty {
            Label("Nothing to clean up.", systemImage: "checkmark.circle")
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("nothing-to-clean")
        } else {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    if !plan.orphans.isEmpty {
                        Text("\(ByteFormat.files(UInt64(plan.orphans.count))) nothing uses: \(ByteFormat.string(plan.orphanBytes))")
                    }
                    if !plan.offload.isEmpty {
                        Text(
                            "\(ByteFormat.files(UInt64(plan.offload.count))) that are also on the archive disk: "
                                + ByteFormat.string(plan.offloadBytes)
                        )
                    }
                }
                .accessibilityIdentifier("clean-summary")
                Spacer()
                Button("Review…") {
                    storage.confirming = true
                }
                .disabled(storage.isCleaning)
                .accessibilityIdentifier("review-clean")
            }
        }
    }
}

/// The exact files a clean-up would remove, for the owner to confirm.
private struct CleanUpSheet: View {
    let plan: GcPlan

    @Environment(StorageStore.self) private var storage

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Remove \(ByteFormat.string(plan.orphanBytes + plan.offloadBytes)) from the store?")
                .font(.headline)
            Text("These stored files are deleted from this Mac. Files in the apps' folders are not touched.")
                .foregroundStyle(.secondary)
            ScrollView {
                VStack(alignment: .leading, spacing: 4) {
                    group("Used by nothing", plan.orphans)
                    group("Also on the archive disk", plan.offload)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(8)
            }
            .frame(height: 220)
            .background(.quaternary.opacity(0.4), in: .rect(cornerRadius: 6))
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) {
                    storage.confirming = false
                }
                .keyboardShortcut(.cancelAction)
                Button("Remove", role: .destructive) {
                    storage.confirming = false
                    Task { await storage.clean() }
                }
                .accessibilityIdentifier("confirm-clean")
            }
        }
        .padding(20)
        .frame(width: 560)
    }

    @ViewBuilder
    private func group(_ title: String, _ files: [StoredFile]) -> some View {
        if !files.isEmpty {
            Text(title)
                .font(.caption.weight(.semibold))
                .padding(.top, 4)
            ForEach(files, id: \.sha256) { file in
                HStack {
                    Text(file.sha256)
                        .font(.caption.monospaced())
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer(minLength: 8)
                    Text(ByteFormat.string(file.size))
                        .font(.caption)
                        .monospacedDigit()
                        .foregroundStyle(.secondary)
                }
            }
        }
    }
}

/// Hashing every stored file to see that none is damaged or missing.
struct StoreCheckCard: View {
    @Environment(StorageStore.self) private var storage
    @Environment(DaemonStore.self) private var daemon

    var body: some View {
        Card(
            title: "Check the store",
            note: "Reads every stored file and compares it with its sha256. The daemon does this by itself on a schedule."
        ) {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 4) {
                    if let check = storage.check {
                        result(check)
                    } else {
                        Text("Never checked.")
                            .foregroundStyle(.secondary)
                    }
                }
                Spacer()
                Button(running ? "Checking…" : "Check Now") {
                    Task { await storage.startCheck() }
                }
                .disabled(running)
                .accessibilityIdentifier("check-store")
            }
        }
    }

    private var running: Bool {
        daemon.checking || storage.check?.running == true
    }

    @ViewBuilder
    private func result(_ check: StoreCheck) -> some View {
        if check.running {
            HStack(spacing: 8) {
                ProgressView()
                    .controlSize(.small)
                Text("Checking: \(ByteFormat.files(check.checked)) so far, \(ByteFormat.string(check.bytes))")
            }
        } else if let finished = check.finishedAt {
            let fine = check.damaged.isEmpty && check.missing.isEmpty && check.error == nil
            Label(
                fine
                    ? "All \(ByteFormat.files(check.checked)) intact (\(ByteFormat.string(check.bytes)))"
                    : "\(check.damaged.count) damaged, \(check.missing.count) missing of \(ByteFormat.files(check.checked))",
                systemImage: fine ? "checkmark.seal.fill" : "exclamationmark.triangle.fill"
            )
            .foregroundStyle(fine ? .green : .red)
            .accessibilityIdentifier("check-result")
            Text("Checked \(TimeFormat.relative(TimeFormat.date(epoch: finished)))")
                .font(.callout)
                .foregroundStyle(.secondary)
            if check.onlyInArchive > 0 {
                Text("\(ByteFormat.files(check.onlyInArchive)) are on the archive disk only and are copied back when needed.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
        } else {
            Text("Never checked.")
                .foregroundStyle(.secondary)
        }
        if let error = check.error {
            FailureLabel(message: error)
        }
        ForEach(check.damaged, id: \.self) { digest in
            Text("Damaged: \(digest)")
                .font(.caption.monospaced())
                .textSelection(.enabled)
        }
        ForEach(check.missing, id: \.self) { digest in
            Text("Missing: \(digest)")
                .font(.caption.monospaced())
                .textSelection(.enabled)
        }
    }
}
