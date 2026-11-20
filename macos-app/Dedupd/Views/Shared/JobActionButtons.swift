import DedupdClient
import SwiftUI

/// The buttons for what can be done to a job, as the daemon lists them.
struct JobActionButtons: View {
    let job: Job

    @Environment(DaemonStore.self) private var daemon

    var body: some View {
        HStack(spacing: 2) {
            ForEach(job.actions, id: \.self) { action in
                Button(action.title, systemImage: action.symbol) {
                    Task { await daemon.act(action, on: job) }
                }
                .help(action.help(activation: job.isActivation))
                .accessibilityIdentifier("\(action.name)-\(job.id)")
            }
        }
        .buttonStyle(.borderless)
        .labelStyle(.iconOnly)
    }
}
