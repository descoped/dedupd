import DedupdClient
import SwiftUI

/// One download in full: where it goes, each of its files and its history.
struct DownloadInspector: View {
    let job: Job?

    @Environment(DaemonStore.self) private var daemon
    @Environment(DiscoverStore.self) private var discover
    @Environment(Navigation.self) private var navigation
    @State private var detail: JobDetail?
    /// Why the files and the history could not be read, for the job named.
    @State private var failure: (job: Int64, message: String)?

    /// What makes the files and the history worth reading again: another
    /// download, another state, another file done, or two per cent more.
    private struct Key: Equatable {
        var id: Int64?
        var state: JobState?
        var files: UInt64
        var step: UInt64

        init(_ job: Job?) {
            id = job?.id
            state = job?.state
            files = job?.filesDone ?? 0
            step = job.map { $0.bytesTotal == 0 ? 0 : $0.bytesDone * 50 / $0.bytesTotal } ?? 0
        }
    }

    var body: some View {
        if let job {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    header(job)
                    InspectorSection(title: "Download") {
                        Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 12, verticalSpacing: 5) {
                            FactRow("State", job.state.title)
                            if let detail = job.detail, !detail.isEmpty { FactRow("Why", detail) }
                            FactRow("Size", job.bytesTotal == 0 ? "Not known yet" : job.progressText)
                            FactRow("Files", "\(job.filesDone) of \(job.filesTotal)")
                            FactRow("Goes into", job.appList)
                            if let commit = job.commit { FactRow("Revision", String(commit.prefix(12)), monospaced: true) }
                            if !job.include.isEmpty { FactRow("Only files", job.include.joined(separator: ", ")) }
                            if job.offline { FactRow("Source", "The archive only; Hugging Face is not asked") }
                            FactRow("Asked for", TimeFormat.full(TimeFormat.date(epoch: job.createdAt)))
                            if let finished = job.finishedAt {
                                FactRow("Finished", TimeFormat.full(TimeFormat.date(epoch: finished)))
                            }
                        }
                    }
                    if let detail, detail.job.id == job.id {
                        files(detail)
                        history(detail)
                    } else if let failure, failure.job == job.id {
                        FailureLabel(message: failure.message)
                    } else {
                        ProgressView()
                            .frame(maxWidth: .infinity)
                    }
                }
                .padding(16)
            }
            .task(id: Key(job)) {
                // An answer that comes after the owner chose another
                // download, or after a newer read began, is dropped.
                do {
                    let answer = try await daemon.client.download(job.id, at: daemon.address)
                    guard !Task.isCancelled else {
                        return
                    }
                    detail = answer
                    failure = nil
                } catch {
                    guard !Task.isCancelled else {
                        return
                    }
                    failure = (job.id, describe(error))
                }
            }
            .accessibilityIdentifier("download-inspector")
        } else {
            ContentUnavailableView(
                "No download selected",
                systemImage: "sidebar.trailing",
                description: Text("Select a download to see its files and what happened to it.")
            )
        }
    }

    private func header(_ job: Job) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(job.model)
                .font(.title3.weight(.semibold))
                .textSelection(.enabled)
            HStack {
                Label(job.state.title, systemImage: job.state.symbol)
                    .foregroundStyle(job.state.tint)
                Spacer()
                Button("Show in Discover") {
                    navigation.pane = .discover
                    discover.open(job.repo)
                }
                .controlSize(.small)
            }
            .font(.callout)
        }
    }

    private func files(_ detail: JobDetail) -> some View {
        InspectorSection(title: "Files") {
            ForEach(detail.files, id: \.path) { file in
                VStack(alignment: .leading, spacing: 3) {
                    FileRow(
                        path: file.path,
                        size: file.size,
                        note: file.stageWords,
                        symbol: file.stage == .stored ? "checkmark.circle" : "doc",
                        digest: file.sha256
                    )
                    if file.stage == .downloading, file.size > 0 {
                        ProgressView(value: min(1, Double(file.done) / Double(file.size)))
                            .controlSize(.mini)
                            .padding(.leading, 24)
                    }
                }
            }
        }
    }

    private func history(_ detail: JobDetail) -> some View {
        InspectorSection(title: "What happened") {
            ForEach(Array(detail.events.enumerated()), id: \.offset) { _, event in
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(TimeFormat.date(epoch: event.at).formatted(date: .omitted, time: .standard))
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.tertiary)
                    Text(event.text)
                        .font(.callout)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
    }
}
