import DedupdClient
import Foundation

/// The canned daemon's storage: where the files are, what a clean-up
/// would remove and the last check of the store.
extension FixtureData {
    // MARK: - Storage

    /// The places of the fixed library, before anything is done to it.
    static var locations: Locations { locations(of: models, cleaned: false) }

    /// Where a library's files are, counted as the daemon counts them: the
    /// store by what it holds, each app by its own files, and the archive
    /// disk by what it holds: the backup has every file the daemon knows,
    /// also of an archived model, and so every model whole.
    static func locations(
        of library: [LibraryModel], cleaned: Bool, offloaded: Bool = false, archives: [ArchiveSetting] = archives,
        archivedTo: [String: String] = [:]
    ) -> Locations {
        let store = stored(of: library, cleaned: cleaned, offloaded: offloaded)
        let backup = (
            files: library.map(\.storedFiles).reduce(0, +) + (cleaned ? 0 : orphanFiles),
            bytes: library.map(\.storedBytes).reduce(0, +) + (cleaned ? 0 : orphanBytes)
        )
        return Locations(
            volumeTotal: 994_660_000_000,
            volumeFree: 412_300_000_000,
            store: Location(
                kind: .store, app: nil, name: nil, backup: false, path: "/Users/example/.dedupd/store", present: true,
                files: store.files, bytes: store.bytes, sharedBytes: 0, models: UInt64(library.count)
            ),
            archives: archives.map { archive in
                // The backup has every file; another archive, the models
                // archived to it. A disk that is away counts as nothing.
                let held = archive.backup
                    ? library
                    : library.filter { archivedTo[$0.repo] == archive.name }
                let counted = !archive.attached
                    ? (files: UInt64(0), bytes: UInt64(0), models: UInt64(0))
                    : archive.backup
                    ? (files: backup.files, bytes: backup.bytes, models: UInt64(library.count))
                    : (
                        files: held.map(\.storedFiles).reduce(0, +), bytes: held.map(\.storedBytes).reduce(0, +),
                        models: UInt64(held.count)
                    )
                return Location(
                    kind: .archive, app: nil, name: archive.name, backup: archive.backup, path: archive.path,
                    present: archive.attached, files: counted.files, bytes: counted.bytes, sharedBytes: 0,
                    models: counted.models
                )
            },
            apps: ModelApp.allCases.map { app in
                let held = usage(of: app, in: library)
                return Location(
                    kind: .app, app: app, name: nil, backup: false, path: folder(of: app), present: true,
                    files: held.files, bytes: held.bytes, sharedBytes: held.bytes, models: held.models
                )
            }
        )
    }

    /// What a clean-up would remove. Once the owner has cleaned up, the
    /// orphans are gone; once the local copies went, they are not listed.
    static func gcPlan(
        offload: Bool, library: [LibraryModel] = models, cleaned: Bool = false, offloaded: Bool = false,
        freed: UInt64? = nil
    ) -> GcPlan {
        let orphans = cleaned
            ? []
            : [
                StoredFile(sha256: sha("orphan-one"), size: 4_920_000_000, gitOid: nil, knownAs: nil),
                StoredFile(sha256: sha("orphan-two"), size: 1840, gitOid: String(sha("orphan-two").prefix(40)), knownAs: nil),
            ]
        let away = offload && !offloaded && library.contains(where: canOffload)
            ? [
                StoredFile(
                    sha256: sha("model.safetensors"), size: offloadBytes, gitOid: nil,
                    knownAs: "\(smolMLX): model.safetensors"
                ),
            ]
            : []
        return GcPlan(
            orphans: orphans,
            offload: away,
            orphanBytes: orphans.map(\.size).reduce(0, +),
            offloadBytes: away.map(\.size).reduce(0, +),
            freed: freed
        )
    }

    /// The last check of the store: every file it holds was read and is
    /// intact.
    static func storeCheck(
        of library: [LibraryModel] = models, cleaned: Bool = false, offloaded: Bool = false
    ) -> StoreCheck {
        let store = stored(of: library, cleaned: cleaned, offloaded: offloaded)
        return StoreCheck(
            running: false, startedAt: 1_793_899_000, finishedAt: 1_793_900_000, checked: store.files,
            bytes: store.bytes, damaged: [], missing: [], onlyInArchive: 0, archiveAway: 0, error: nil
        )
    }
}
