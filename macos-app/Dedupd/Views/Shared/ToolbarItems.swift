import SwiftUI

/// The toolbar button that switches the tables between one line and two
/// lines per row.
struct DensityToggle: View {
    @Environment(AppSettings.self) private var settings

    private var isLarger: Binding<Bool> {
        Binding {
            settings.density == .comfortable
        } set: { larger in
            settings.density = larger ? .comfortable : .compact
        }
    }

    var body: some View {
        Toggle(RowDensity.comfortable.title, systemImage: RowDensity.comfortable.symbol, isOn: isLarger)
            .help("Larger rows add a second line with who made the model and what it is")
            .accessibilityIdentifier("row-density")
    }
}

/// One thing a toolbar selector can be set to.
struct FilterOption<Value: Hashable>: Identifiable {
    let value: Value
    let title: String

    var id: Value { value }
}

/// A selector in the toolbar that shows what it is set to and is as wide as
/// that, so two of them beside each other look alike.
struct FilterMenu<Value: Hashable>: View {
    /// What it chooses, for the tooltip and for VoiceOver.
    let title: String
    @Binding var selection: Value
    let options: [FilterOption<Value>]

    var body: some View {
        Menu {
            Picker(title, selection: $selection) {
                ForEach(options) { option in
                    Text(option.title).tag(option.value)
                }
            }
            .pickerStyle(.inline)
            .labelsHidden()
        } label: {
            Text(options.first { $0.value == selection }?.title ?? title)
        }
        .menuIndicator(.visible)
        .accessibilityLabel(title)
    }
}

/// The toolbar button that shows or hides the inspector.
struct InspectorToggle: View {
    @Binding var isShown: Bool

    var body: some View {
        Button("Inspector", systemImage: "sidebar.trailing") {
            isShown.toggle()
        }
        .help(isShown ? "Hide the inspector" : "Show the inspector")
        .keyboardShortcut("i", modifiers: [.command, .option])
    }
}
