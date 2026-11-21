import AppKit
import DedupdClient
import SwiftUI

/// The stores: the copies apps are given clones of, one on each disk that
/// holds app folders. The one in dedupd's home is always there; another is
/// added with a folder picker, named after its disk, and used at once.
struct StoresCard: View {
    @Environment(StorageStore.self) private var storage
    @State private var adding: PickedStoreFolder?
    @State private var renaming: StoreSetting?
    @State private var removing: StoreSetting?
    @State private var problem: String?

    var body: some View {
        Card(title: "Stores", note: "An app is given a model as clones of the files in the store on its own disk. "
            + "Add a store on another disk when an app keeps its models there.") {
            ForEach(storage.stores) { store in
                StoreRow(
                    store: store,
                    contents: store.contents(in: storage.locations.value),
                    rename: { renaming = store },
                    remove: { removing = store }
                )
                if store.id != storage.stores.last?.id {
                    Divider()
                }
            }
            if let problem {
                FailureLabel(message: problem)
            }
            HStack {
                Spacer()
                Button("Add Store…", systemImage: "plus") {
                    adding = Self.pickFolder().map(PickedStoreFolder.init)
                }
                .accessibilityIdentifier("add-store")
            }
        }
        .sheet(item: $adding) { picked in
            AddStoreSheet(folder: picked.url)
        }
        .sheet(item: $renaming) { store in
            RenameStoreSheet(store: store)
        }
        .confirmationDialog(
            Text(verbatim: "Remove the store “\(removing?.name ?? "")”?"),
            isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }),
            presenting: removing
        ) { store in
            Button("Remove", role: .destructive) {
                Task { problem = await storage.remove(store) }
            }
        } message: { store in
            Text(StoreWords.removeMessage(store))
        }
    }

    /// Ask for the folder of a new store; nil when the owner cancels.
    private static func pickFolder() -> URL? {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.canCreateDirectories = true
        panel.allowsMultipleSelection = false
        panel.prompt = "Use as Store"
        panel.message = "Choose the disk where an app keeps its models (it must be APFS). "
            + "dedupd keeps its files in a folder named dedupd there."
        panel.directoryURL = URL(fileURLWithPath: "/Volumes")
        return panel.runModal() == .OK ? panel.url : nil
    }
}

/// A folder the owner picked, for the sheet that names it.
private struct PickedStoreFolder: Identifiable {
    let url: URL
    var id: String { url.path }
}

/// One store: its name, where it is, which apps it serves and what it holds.
private struct StoreRow: View {
    let store: StoreSetting
    let contents: String?
    let rename: () -> Void
    let remove: () -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: store.attached ? "internaldrive.fill" : "externaldrive.badge.xmark")
                .font(.title2)
                .foregroundStyle(store.attached ? Color.accentColor : .secondary)
                .frame(width: 28)
            VStack(alignment: .leading, spacing: 2) {
                Text(verbatim: store.name)
                    .font(.body.weight(.semibold))
                Text(verbatim: store.path)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
                Text(store.note)
                    .font(.caption)
                    .foregroundStyle(store.attached ? Color.secondary : .orange)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer()
            if let contents {
                Text(contents)
                    .font(.callout)
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
            }
            Menu {
                actions
            } label: {
                Image(systemName: "ellipsis.circle")
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .help(store.primary ? "Show in the Finder" : "Rename, show in the Finder or remove")
            .accessibilityLabel("Actions for \(store.name)")
        }
        .contentShape(.rect)
        .contextMenu { actions }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("store-\(store.name)")
    }

    @ViewBuilder private var actions: some View {
        if !store.primary {
            Button("Rename…", action: rename)
        }
        Button("Show in Finder") {
            Finder.reveal(store.path)
        }
        .disabled(!store.attached)
        if !store.primary {
            Divider()
            Button("Remove…", role: .destructive, action: remove)
        }
    }
}

/// Names a newly picked folder before it becomes a store.
private struct AddStoreSheet: View {
    let folder: URL

    @Environment(StorageStore.self) private var storage
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var problem: String?
    @State private var working = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Add a Store")
                .font(.headline)
            Text(verbatim: folder.path)
                .font(.callout.monospaced())
                .foregroundStyle(.secondary)
            TextField("Name", text: $name, prompt: Text(StoreWords.suggestedName(for: folder)))
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("store-name")
            Text("Apps whose folders are on this disk are given models from this store. "
                + "A model they need that is in another store is copied here when you activate it.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            if let problem {
                FailureLabel(message: problem)
            }
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button("Add") {
                    Task {
                        working = true
                        let chosen = name.trimmingCharacters(in: .whitespaces)
                        problem = await storage.addStore(
                            folder: folder.path,
                            name: chosen.isEmpty ? StoreWords.suggestedName(for: folder) : chosen
                        )
                        working = false
                        if problem == nil {
                            dismiss()
                        }
                    }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(working)
                .accessibilityIdentifier("confirm-add-store")
            }
        }
        .padding(20)
        .frame(width: 460)
    }
}

/// Gives a store another name.
private struct RenameStoreSheet: View {
    let store: StoreSetting

    @Environment(StorageStore.self) private var storage
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var problem: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(verbatim: "Rename “\(store.name)”")
                .font(.headline)
            TextField("Name", text: $name)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("rename-store-name")
            if let problem {
                FailureLabel(message: problem)
            }
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button("Rename") {
                    Task {
                        problem = await storage.rename(store, to: name)
                        if problem == nil {
                            dismiss()
                        }
                    }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(name.trimmingCharacters(in: .whitespaces).isEmpty)
                .accessibilityIdentifier("confirm-rename-store")
            }
        }
        .padding(20)
        .frame(width: 380)
        .onAppear { name = store.name }
    }
}
