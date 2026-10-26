#![cfg(windows)]

use std::{
    fs,
    io::{BufRead, BufReader},
    os::windows::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
};

const V1: &str = env!("CARGO_BIN_EXE_ota-fixture-v1");
const V2: &str = env!("CARGO_BIN_EXE_ota-fixture-v2");
const CREATE_NO_WINDOW: u32 = 0x08000000;

fn version(path: &Path) -> String {
    let output = Command::new(path)
        .arg("--version")
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().into()
}

#[test]
fn direct_move_preserves_canonical_on_failure_and_supports_persistent_rollback() {
    let dir = tempfile::Builder::new()
        .prefix("OTA 中文 空格 ")
        .tempdir()
        .unwrap();
    let canonical = dir.path().join("periph-in-one.exe");
    let staged = dir.path().join("candidate.exe");
    let backup = dir.path().join("backup.exe");
    fs::copy(V1, &canonical).unwrap();
    fs::copy(V2, &staged).unwrap();
    fs::copy(&canonical, &backup).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&backup)
        .unwrap()
        .sync_all()
        .unwrap();

    // Wait on this exact process object, never a recycled PID. Windows refuses
    // replacement of a mapped executable; an error must preserve the entry.
    let mut old = Command::new(&canonical)
        .arg("--hold")
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut ready = String::new();
    BufReader::new(old.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready.trim(), "ready");
    assert!(
        self_update::Move::from_source(&staged)
            .to_dest(&canonical)
            .is_err()
    );
    assert!(canonical.is_file());
    old.kill().unwrap();
    old.wait().unwrap();
    assert_eq!(version(&canonical), "0.0.1");

    // One library rename over the unlocked destination. Never move it aside.
    self_update::Move::from_source(&staged)
        .to_dest(&canonical)
        .unwrap();
    assert_eq!(version(&canonical), "0.0.2");
    assert_eq!(version(&backup), "0.0.1");
    let restore = dir.path().join("restore.exe");
    fs::copy(&backup, &restore).unwrap();
    self_update::Move::from_source(&restore)
        .to_dest(&canonical)
        .unwrap();
    assert_eq!(version(&canonical), "0.0.1");
    assert!(
        self_update::Move::from_source(&staged)
            .to_dest(&canonical)
            .is_err()
    );
    assert_eq!(version(&canonical), "0.0.1");
}

#[test]
fn self_replace_missing_candidate_demonstrates_lost_entry() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = dir.path().join("fixture.exe");
    fs::copy(V1, &canonical).unwrap();
    let output = Command::new(&canonical)
        .arg("--self-replace-missing")
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "failed=true canonical=false"
    );
    assert!(!canonical.exists());
}

#[test]
fn move_all_rolls_back_an_ordinary_error_but_is_not_the_selected_entry_primitive() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let source = dir.path().join("source");
    let backup = dir.path().join("backup");
    fs::create_dir(&backup).unwrap();
    fs::write(&target, b"old").unwrap();
    fs::write(&source, b"new").unwrap();
    assert!(
        self_update::MoveAll::from_temp(&backup)
            .add(&source, &target)
            .add(dir.path().join("absent"), dir.path().join("other"))
            .commit()
            .is_err()
    );
    assert_eq!(fs::read(target).unwrap(), b"old");
}
