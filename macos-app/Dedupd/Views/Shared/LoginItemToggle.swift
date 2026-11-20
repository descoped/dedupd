import SwiftUI

/// The switch for opening the app at login, with the reason under it when
/// macOS did not take a change. Shown where the switch is, so it is seen
/// in whichever window the owner used.
struct LoginItemToggle: View {
    @Environment(LoginItemStore.self) private var loginItem

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Toggle("Open this app at login", isOn: isOn)
            if let failure = loginItem.failure {
                Text(verbatim: failure)
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("login-item-failure")
            }
        }
        // The owner may have changed it in System Settings meanwhile.
        .onAppear { loginItem.refresh() }
    }

    private var isOn: Binding<Bool> {
        Binding {
            loginItem.isEnabled
        } set: { enabled in
            loginItem.set(enabled)
        }
    }
}
