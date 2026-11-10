//! Reclaim, verify and gc against scratch folders.
#![cfg(target_os = "macos")]

use dedupd_core::adopt::{self, Options};
use dedupd_core::db::Db;
use dedupd_core::maintain;
use dedupd_core::place::{App, AppDirs};
use dedupd_core::store::Store;
use dedupd_core::{Home, archive};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::sync::Mutex;
use std::time::Duration;

struct Fx {
    _dir: tempfile::TempDir,
    dirs: AppDirs,
    store: Store,
    db: Mutex<Db>,
    archive: Home,
}

fn fx() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let archive = Home::new(dir.path().join("trunk/dedupd"));
    fs::create_dir_all(dir.path().join("trunk")).unwrap();
    Fx {
        dirs: AppDirs::under(&dir.path().join("apps")),
        store: Store::open(&home)
            .unwrap()
            .with_archive(Some(archive.clone())),
        db: Mutex::new(Db::open(&home.db_path()).unwrap()),
        archive,
        _dir: dir,
    }
}

async fn adopt_all(fx: &Fx) -> Vec<adopt::RepoOutcome> {
    let repos = adopt::scan_apps(&fx.db, &fx.dirs, &App::ALL).unwrap();
    adopt::adopt(
        &fx.db,
        &fx.store,
        None,
        repos,
        &Options::default(),
        &mut |_| {},
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn a_second_copy_is_swapped_for_a_clone() {
    let fx = fx();
    let weights = vec![9u8; 200_000];
    // Two repositories in Bionic hold the same bytes as separate copies.
    let a = fx.dirs.bionic.join("org/A/model.gguf");
    let b = fx.dirs.bionic.join("org/B/model.gguf");
    for p in [&a, &b] {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, &weights).unwrap();
    }
    adopt_all(&fx).await;
    let shared: Vec<bool> = fx
        .db
        .lock()
        .unwrap()
        .placements()
        .unwrap()
        .iter()
        .map(|p| p.shared)
        .collect();
    assert_eq!(shared.iter().filter(|s| **s).count(), 1);

    // A file written a moment ago is left alone.
    let young = maintain::reclaim(&fx.db, &fx.store, Duration::from_secs(3600), false).unwrap();
    assert_eq!(young.swapped, 0);
    assert_eq!(young.skipped[0].1, "written too recently");

    let before: Vec<_> = [&a, &b].iter().map(|p| fs::metadata(p).unwrap()).collect();
    let dry = maintain::reclaim(&fx.db, &fx.store, Duration::ZERO, true).unwrap();
    assert_eq!((dry.swapped, dry.bytes), (1, 200_000));
    let done = maintain::reclaim(&fx.db, &fx.store, Duration::ZERO, false).unwrap();
    assert_eq!((done.swapped, done.skipped.len()), (1, 0));

    // Same bytes, same time and permissions; exactly one file was replaced.
    let after: Vec<_> = [&a, &b].iter().map(|p| fs::metadata(p).unwrap()).collect();
    let replaced = (0..2)
        .filter(|&i| before[i].ino() != after[i].ino())
        .count();
    assert_eq!(replaced, 1);
    for i in 0..2 {
        assert_eq!(before[i].mtime_nsec(), after[i].mtime_nsec());
        assert_eq!(before[i].mtime(), after[i].mtime());
        assert_eq!(before[i].mode(), after[i].mode());
    }
    assert_eq!(fs::read(&a).unwrap(), weights);
    assert_eq!(fs::read(&b).unwrap(), weights);
    assert!(
        fx.db
            .lock()
            .unwrap()
            .placements()
            .unwrap()
            .iter()
            .all(|p| p.shared)
    );

    // Nothing left to do, and a later scan sees nothing new.
    assert_eq!(
        maintain::reclaim(&fx.db, &fx.store, Duration::ZERO, false)
            .unwrap()
            .swapped,
        0
    );
    let again = adopt_all(&fx).await;
    assert!(
        again
            .iter()
            .flat_map(|r| &r.files)
            .all(|f| f.action == adopt::Action::Unchanged)
    );
}

#[tokio::test]
async fn verify_finds_damage_and_gc_removes_only_what_is_unreferenced() {
    let fx = fx();
    let keep = fx.dirs.bionic.join("org/Keep/model.gguf");
    let drop = fx.dirs.bionic.join("org/Drop/model.gguf");
    fs::create_dir_all(keep.parent().unwrap()).unwrap();
    fs::create_dir_all(drop.parent().unwrap()).unwrap();
    fs::write(&keep, vec![1u8; 50_000]).unwrap();
    fs::write(&drop, vec![2u8; 60_000]).unwrap();
    let out = adopt_all(&fx).await;
    let sha = |repo: &str| {
        out.iter().find(|r| r.name == repo).unwrap().files[0]
            .sha256
            .clone()
    };
    let (keep_sha, drop_sha) = (sha("org/Keep"), sha("org/Drop"));

    let clean = maintain::verify(&fx.db, &fx.store, false, 2).unwrap();
    assert_eq!(
        (clean.checked, clean.damaged.len(), clean.missing.len()),
        (2, 0, 0)
    );

    // While an app holds the files, nothing is a candidate for removal.
    let plan = maintain::gc_plan(&fx.db, &fx.store, true).unwrap();
    assert!(plan.orphans.is_empty() && plan.offload.is_empty());
    assert!(maintain::forget(&fx.db, "org/Drop").is_err());

    // The model is deleted in the app. Its content stays in the store until
    // the owner runs gc.
    archive::sync(&fx.db, &fx.store, &fx.archive, &mut |_, _| {}).unwrap();
    fs::remove_file(&drop).unwrap();
    assert_eq!(maintain::prune_placements(&fx.db).unwrap(), 1);
    assert!(fx.store.contains(&drop_sha));
    let plan = maintain::gc_plan(&fx.db, &fx.store, false).unwrap();
    assert_eq!(plan.orphans.len(), 1);
    assert_eq!(plan.orphans[0].sha256, drop_sha);
    assert_eq!(
        maintain::gc_apply(&fx.db, &fx.store, &plan).unwrap(),
        60_000
    );
    assert!(!fx.store.contains(&drop_sha) && fx.store.contains(&keep_sha));
    assert!(!fx.archive.blob_path(&drop_sha).exists());

    // Damage is reported, in the store and on the archive disk separately.
    let blob = fx.store.blob_path(&keep_sha);
    fs::set_permissions(&blob, std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
    fs::write(&blob, vec![3u8; 50_000]).unwrap();
    assert_eq!(
        maintain::verify(&fx.db, &fx.store, false, 2)
            .unwrap()
            .damaged,
        std::slice::from_ref(&keep_sha)
    );
    assert!(
        maintain::verify(&fx.db, &fx.store, true, 2)
            .unwrap()
            .damaged
            .is_empty()
    );
}

#[tokio::test]
async fn offload_frees_local_space_and_the_file_comes_back() {
    let fx = fx();
    let file = fx.dirs.bionic.join("org/Cold/model.gguf");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, vec![5u8; 70_000]).unwrap();
    let out = adopt_all(&fx).await;
    let sha = out[0].files[0].sha256.clone();
    {
        // Give the file an archived revision, as a pulled model has.
        let db = fx.db.lock().unwrap();
        db.conn()
            .execute_batch(&format!(
                "INSERT INTO revisions (id, repo_id, commit_sha, fetched_at, info_json, tree_json)
                 VALUES (1, 1, 'c', 0, '{{}}', '[]');
                 INSERT INTO revision_files VALUES (1, 'model.gguf', 70000, 'g', '{sha}');"
            ))
            .unwrap();
    }
    archive::sync(&fx.db, &fx.store, &fx.archive, &mut |_, _| {}).unwrap();

    // Removed from the app: not an orphan (a revision lists it), but it can
    // leave the local disk because the archive has it.
    fs::remove_file(&file).unwrap();
    maintain::prune_placements(&fx.db).unwrap();
    assert!(
        maintain::gc_plan(&fx.db, &fx.store, false)
            .unwrap()
            .offload
            .is_empty()
    );
    let plan = maintain::gc_plan(&fx.db, &fx.store, true).unwrap();
    assert!(plan.orphans.is_empty());
    assert_eq!(plan.offload.len(), 1);
    assert_eq!(
        maintain::gc_apply(&fx.db, &fx.store, &plan).unwrap(),
        70_000
    );
    assert!(!fx.store.contains(&sha));
    assert_eq!(
        maintain::verify(&fx.db, &fx.store, false, 1)
            .unwrap()
            .only_in_archive,
        1
    );

    assert!(fx.store.ensure_local(&sha).unwrap());
    assert_eq!(
        fs::read(fx.store.blob_path(&sha)).unwrap(),
        vec![5u8; 70_000]
    );
}
