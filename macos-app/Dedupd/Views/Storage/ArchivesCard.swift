import AppKit
import DedupdClient
import SwiftUI

/// The archives: cold locations on other disks that hold copies of models.
/// Added with a folder picker, named after their disk, and changed at once:
/// there is nothing to apply and no restart.
struct ArchivesCard: View {
    @Environment(StorageStore.self) private var storage
    @State private var adding: PickedFolder?
    @State private var renaming: ArchiveSetting?
    @State private var removing: ArchiveSetting?
    @State private var problem: String?

    var body: some View {
        Card(title: "Archives", note: "Copies of models on other disks. Archive a model to one of them to free "
            + "space here; restore it to bring it back.") {
            if storage.archives.isEmpty {
                Text("No archive yet. Add a folder on another disk, such as an external SSD, to keep a copy of your models there.")
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            ForEach(storage.archives) { archive in
                ArchiveRow(
                    archive: archive,
                    contents: archive.contents(in: storage.locations.value),
                    rename: { renaming = archive },
                    backup: { Task { await change { await storage.useAsBackup(archive.backup ? nil : archive) } } },
                    remove: { removing = archive }
                )
                if archive.id != storage.archives.last?.id {
                    Divider()
                }
            }
            if let problem {
                FailureLabel(message: problem)
            }
            HStack {
                Spacer()
                Button("Add Archive…", systemImage: "plus") {
                    adding = Self.pickFolder().map(PickedFolder.init)
                }
                .accessibilityIdentifier("add-archive")
            }
        }
        .sheet(item: $adding) { picked in
            AddArchiveSheet(folder: picked.url, offersBackup: storage.backupArchive == nil)
        }
        .sheet(item: $renaming) { archive in
            RenameArchiveSheet(archive: archive)
        }
        .confirmationDialog(
            "Remove the archive “\(removing?.name ?? "")”?",
            isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }),
            presenting: removing
        ) { archive in
            Button("Remove", role: .destructive) {
                Task { await change { await storage.remove(archive) } }
            }
        } message: { archive in
            Text(ArchiveWords.removeMessage(archive))
        }
    }

    private func change(_ work: () async -> String?) async {
        problem = await work()
    }

    /// Ask for the folder of a new archive; nil when the owner cancels.
    private static func pickFolder() -> URL? {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.canCreateDirectories = true
        panel.allowsMultipleSelection = false
        panel.prompt = "Use as Archive"
        panel.message = "Choose a disk or a folder on it. dedupd keeps its files in a folder named dedupd there."
        panel.directoryURL = URL(fileURLWithPath: "/Volumes")
        return panel.runModal() == .OK ? panel.url : nil
    }
}

/// A folder the owner picked, for the sheet that names it.
private struct PickedFolder: Identifiable {
    let url: URL
    var id: String { url.path }
}

/// One archive: its name, where it is, what it holds and what can be done.
private struct ArchiveRow: View {
    let archive: ArchiveSetting
    let contents: String?
    let rename: () -> Void
    let backup: () -> Void
    let remove: () -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: archive.attached ? "externaldrive.fill" : "externaldrive.badge.xmark")
                .font(.title2)
                .foregroundStyle(archive.attached ? Color.accentColor : .secondary)
                .frame(width: 28)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(verbatim: archive.name)
                        .font(.body.weight(.semibold))
                    if archive.backup {
                        Text("Backup")
                            .font(.caption.weight(.medium))
                            .padding(.horizontal, 6)
                            .padding(.vertical, 1)
                            .background(.tint.opacity(0.15), in: .capsule)
                    }
                }
                Text(verbatim: archive.path)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
                Text(archive.note)
                    .font(.caption)
                    .foregroundStyle(archive.attached && !archive.sameDiskAsStore ? Color.secondary : .orange)
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
            .help("Rename, back up here, show in the Finder or remove")
            .accessibilityLabel("Actions for \(archive.name)")
        }
        .contentShape(.rect)
        .contextMenu { actions }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("archive-\(archive.name)")
    }

    @ViewBuilder private var actions: some View {
        Button("Rename…", action: rename)
        Button(archive.backup ? "Stop Copying Every Download Here" : "Copy Every Download Here", action: backup)
        Button("Show in Finder") {
            Finder.reveal(archive.path)
        }
        .disabled(!archive.attached)
        Divider()
        Button("Remove…", role: .destructive, action: remove)
    }
}

/// Names a newly picked folder, and asks whether every download goes there.
private struct AddArchiveSheet: View {
    let folder: URL
    let offersBackup: Bool

    @Environment(StorageStore.self) private var storage
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var backup = false
    @State private var problem: String?
    @State private var working = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Add an Archive")
                .font(.headline)
            Text(verbatim: folder.path)
                .font(.callout.monospaced())
                .foregroundStyle(.secondary)
            TextField("Name", text: $name, prompt: Text(ArchiveWords.suggestedName(for: folder)))
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("archive-name")
            if offersBackup {
                Toggle("Copy every download here (a backup of the whole store)", isOn: $backup)
            }
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
                        problem = await storage.addArchive(
                            folder: folder.path,
                            name: chosen.isEmpty ? ArchiveWords.suggestedName(for: folder) : chosen,
                            backup: backup
                        )
                        working = false
                        if problem == nil {
                            dismiss()
                        }
                    }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(working)
                .accessibilityIdentifier("confirm-add-archive")
            }
        }
        .padding(20)
        .frame(width: 460)
        .onAppear { backup = offersBackup }
    }
}

/// Gives an archive another name.
private struct RenameArchiveSheet: View {
    let archive: ArchiveSetting

    @Environment(StorageStore.self) private var storage
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var problem: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(verbatim: "Rename “\(archive.name)”")
                .font(.headline)
            TextField("Name", text: $name)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("rename-archive-name")
            if let problem {
                FailureLabel(message: problem)
            }
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button("Rename") {
                    Task {
                        problem = await storage.rename(archive, to: name)
                        if problem == nil {
                            dismiss()
                        }
                    }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(name.trimmingCharacters(in: .whitespaces).isEmpty)
                .accessibilityIdentifier("confirm-rename-archive")
            }
        }
        .padding(20)
        .frame(width: 380)
        .onAppear { name = archive.name }
    }
}
