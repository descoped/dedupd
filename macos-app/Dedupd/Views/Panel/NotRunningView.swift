import SwiftUI

/// What the panel shows when no daemon answers, with the command that
/// starts one.
struct NotRunningView: View {
    /// Where the app looked, when nothing answered there.
    let address: String?
    /// What went wrong, when something answered but not with a status.
    let message: String?

    private static let installCommand = "dedupd install"

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(headline)
                .font(.callout.weight(.medium))
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityIdentifier("not-running-message")
            if let message {
                Text(message)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
            } else {
                Text("The apps keep the models they already have. Start the daemon to share new downloads and keep the archive up to date:")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                HStack {
                    Text(Self.installCommand)
                        .font(.system(.callout, design: .monospaced))
                        .textSelection(.enabled)
                    Spacer()
                    Button {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(Self.installCommand, forType: .string)
                    } label: {
                        Label("Copy", systemImage: "doc.on.doc")
                    }
                    .controlSize(.small)
                }
                .padding(8)
                .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 8))
                Text("It installs a login item that runs the daemon and restarts it if it stops.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var headline: String {
        if let address {
            return "No dedupd daemon answers at \(address)."
        }
        return "The daemon answered, but not with its status."
    }
}
