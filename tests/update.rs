use periph_in_one::update::{
    self,
    package::{self, Manifest, Payload},
    storage::{self, Phase, State, Transaction},
    transaction,
};
use std::{fs, path::Path};

fn payload(root: &Path, name: &str, bytes: &[u8]) -> Payload {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, bytes).unwrap();
    Payload {
        path: name.into(),
        size: bytes.len() as u64,
        sha256: package::hash_file(&path).unwrap(),
    }
}

fn fixture() -> (tempfile::TempDir, Transaction) {
    let root = tempfile::tempdir().unwrap();
    payload(root.path(), "periph-in-one.exe", b"old executable");
    payload(root.path(), "licenses/A", b"old A");
    payload(root.path(), "licenses/B", b"old B");
    payload(
        root.path(),
        "config/settings.json",
        b"{\"version\":1,\"devices\":[]}",
    );
    let id = "a".repeat(32);
    let staging = storage::transaction_path(root.path(), &id)
        .unwrap()
        .join("staged");
    let contents = vec![
        payload(&staging, "periph-in-one.exe", b"new executable"),
        payload(&staging, "licenses/A", b"new A"),
        payload(&staging, "licenses/B", b"new B"),
        payload(&staging, "licenses/C", b"new C"),
    ];
    let manifest = Manifest {
        app_id: update::APP_ID.into(),
        version: "0.23.0".parse().unwrap(),
        target: update::TARGET.into(),
        protocol: 1,
        minimum_updater: 1,
        minimum_schema: 1,
        maximum_schema: 2,
        payload: contents,
    };
    let transaction = Transaction {
        id,
        release_id: 123,
        asset_name: "periph-in-one-0.23.0-windows-x64-ota.zip".into(),
        archive_sha256: "00".repeat(32),
        manifest,
        phase: Phase::Verified,
        old_sha256: None,
        old_payload: vec![],
        config_sha256: None,
        token: "b".repeat(32),
    };
    let state = State {
        highest_accepted: Some(transaction.manifest.version.clone()),
        transaction: Some(transaction.clone()),
        ..Default::default()
    };
    storage::write(&storage::state_path(root.path()), &state).unwrap();
    (root, transaction)
}

fn current(root: &Path) -> Transaction {
    storage::read::<State>(&storage::state_path(root))
        .unwrap()
        .transaction
        .unwrap()
}

#[test]
fn checkpoint_and_cancel_leave_canonical_and_config_untouched() {
    let (root, transaction) = fixture();
    let root = root.path();
    let _lock = storage::lock(root).unwrap();
    assert!(storage::lock(root).is_err());
    let prepared = transaction::prepare(root, &transaction).unwrap();
    assert_eq!(current(root).phase, Phase::Preparing);
    transaction::verify_backup(root, &prepared).unwrap();
    assert_eq!(
        fs::read(root.join("periph-in-one.exe")).unwrap(),
        b"old executable"
    );
    assert_eq!(
        fs::read(root.join("config/settings.json")).unwrap(),
        b"{\"version\":1,\"devices\":[]}"
    );
    transaction::cancel(root, &prepared).unwrap();
    assert_eq!(current(root).phase, Phase::Verified);
}

#[test]
fn reload_after_executable_commit_restores_old_program_licenses_and_preserves_config() {
    let (root, transaction) = fixture();
    let root = root.path();
    let prepared = transaction::prepare(root, &transaction).unwrap();
    transaction::apply(root, &prepared).unwrap();
    assert_eq!(current(root).phase, Phase::AwaitingHealth);
    assert_eq!(
        fs::read(root.join("periph-in-one.exe")).unwrap(),
        b"new executable"
    );
    // Re-read durable state as a later recovery process would after a crash.
    transaction::rollback(root, &current(root)).unwrap();
    assert_eq!(current(root).phase, Phase::RolledBack);
    assert_eq!(
        fs::read(root.join("periph-in-one.exe")).unwrap(),
        b"old executable"
    );
    assert_eq!(fs::read(root.join("licenses/A")).unwrap(), b"old A");
    assert!(!root.join("licenses/C").exists());
    assert_eq!(
        fs::read(root.join("config/settings.json")).unwrap(),
        b"{\"version\":1,\"devices\":[]}"
    );
    assert!(
        storage::read::<State>(&storage::state_path(root))
            .unwrap()
            .rejected
            .contains(&transaction.manifest.version)
    );
}

#[test]
fn healthy_commit_keeps_old_recovery_checkpoint_without_migrating_config() {
    let (root, transaction) = fixture();
    let root = root.path();
    let prepared = transaction::prepare(root, &transaction).unwrap();
    transaction::apply(root, &prepared).unwrap();
    transaction::commit(root, &current(root)).unwrap();
    let committed = current(root);
    assert_eq!(committed.phase, Phase::Committed);
    transaction::verify_backup(root, &committed).unwrap();
    assert!(transaction::rollback(root, &committed).is_err());
    assert_eq!(
        fs::read(root.join("periph-in-one.exe")).unwrap(),
        b"new executable"
    );
    assert_eq!(
        fs::read(root.join("config/settings.json")).unwrap(),
        b"{\"version\":1,\"devices\":[]}"
    );
}

#[test]
fn cleanup_preserves_active_previous_and_unowned_directories() {
    let (root, transaction) = fixture();
    let root = root.path();
    let active = storage::transaction_path(root, &transaction.id).unwrap();
    storage::write(&active.join("owner.json"), &update::APP_ID).unwrap();
    let previous_id = "c".repeat(32);
    let obsolete_id = "d".repeat(32);
    let unowned_id = "e".repeat(32);
    for id in [&previous_id, &obsolete_id] {
        let path = storage::transaction_path(root, id).unwrap();
        fs::create_dir(&path).unwrap();
        storage::write(&path.join("owner.json"), &update::APP_ID).unwrap();
    }
    let unowned = storage::transaction_path(root, &unowned_id).unwrap();
    fs::create_dir(&unowned).unwrap();
    fs::write(unowned.join("user-file"), b"keep").unwrap();
    let mut state: State = storage::read(&storage::state_path(root)).unwrap();
    state.recovery_backup = Some(previous_id.clone());
    transaction::cleanup(root, &state);
    assert!(active.exists());
    assert!(
        storage::transaction_path(root, &previous_id)
            .unwrap()
            .exists()
    );
    assert!(
        !storage::transaction_path(root, &obsolete_id)
            .unwrap()
            .exists()
    );
    assert!(unowned.join("user-file").exists());
}

#[test]
fn concurrent_config_edits_are_never_overwritten_by_apply_or_rollback() {
    let (root, transaction) = fixture();
    let root = root.path();
    let prepared = transaction::prepare(root, &transaction).unwrap();
    fs::write(root.join("config/settings.json"), b"external edit").unwrap();
    assert!(transaction::apply(root, &prepared).is_err());
    transaction::rollback(root, &prepared).unwrap();
    assert_eq!(
        fs::read(root.join("config/settings.json")).unwrap(),
        b"external edit"
    );
    assert_eq!(
        fs::read(root.join("periph-in-one.exe")).unwrap(),
        b"old executable"
    );
}

#[test]
fn tampered_stage_or_backup_is_rejected_before_target_mutation() {
    let (root, transaction) = fixture();
    let root = root.path();
    let prepared = transaction::prepare(root, &transaction).unwrap();
    let directory = storage::transaction_path(root, &prepared.id).unwrap();
    fs::write(directory.join("staged/periph-in-one.exe"), b"tampered").unwrap();
    assert!(transaction::apply(root, &prepared).is_err());
    assert_eq!(
        fs::read(root.join("periph-in-one.exe")).unwrap(),
        b"old executable"
    );
    fs::write(directory.join("backup/periph-in-one.exe"), b"tampered").unwrap();
    assert!(transaction::rollback(root, &prepared).is_err());
    assert_eq!(
        fs::read(root.join("periph-in-one.exe")).unwrap(),
        b"old executable"
    );
}

#[cfg(windows)]
#[test]
fn locked_license_leaves_entry_present_and_partial_apply_can_be_recovered() {
    use std::os::windows::fs::OpenOptionsExt;
    let (root, transaction) = fixture();
    let root = root.path();
    let prepared = transaction::prepare(root, &transaction).unwrap();
    let locked = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(root.join("licenses/B"))
        .unwrap();
    assert!(transaction::apply(root, &prepared).is_err());
    assert_eq!(current(root).phase, Phase::Applying);
    assert_eq!(
        fs::read(root.join("periph-in-one.exe")).unwrap(),
        b"old executable"
    );
    assert_eq!(fs::read(root.join("licenses/A")).unwrap(), b"new A");
    drop(locked);
    transaction::rollback(root, &current(root)).unwrap();
    assert_eq!(fs::read(root.join("licenses/A")).unwrap(), b"old A");
}

#[test]
fn transaction_paths_and_diagnostics_are_bounded() {
    let dir = tempfile::tempdir().unwrap();
    assert!(storage::transaction_path(dir.path(), "../outside").is_err());
    assert!(transaction::plain(dir.path(), &dir.path().join("../outside")).is_err());
    fs::create_dir(dir.path().join(".ota")).unwrap();
    fs::write(dir.path().join(".ota/diagnostics.log"), vec![b'X'; 65536]).unwrap();
    transaction::diagnostic(dir.path(), "test");
    assert!(
        fs::metadata(dir.path().join(".ota/diagnostics.log"))
            .unwrap()
            .len()
            < 1024
    );
    assert_eq!(
        fs::metadata(dir.path().join(".ota/diagnostics.previous.log"))
            .unwrap()
            .len(),
        65536
    );
}
