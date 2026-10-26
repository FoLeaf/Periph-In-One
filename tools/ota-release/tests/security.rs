use periph_in_one::update::{
    self,
    package::{self, Manifest, Payload},
};
use std::{fs, io::Write, path::Path};

fn manifest() -> Manifest {
    let hash = "00".repeat(32);
    Manifest {
        app_id: update::APP_ID.into(),
        version: "0.23.0".parse().unwrap(),
        target: update::TARGET.into(),
        protocol: 1,
        minimum_updater: 1,
        minimum_schema: 1,
        maximum_schema: 2,
        payload: vec![
            Payload {
                path: "periph-in-one.exe".into(),
                size: 1,
                sha256: hash.clone(),
            },
            Payload {
                path: "licenses/LICENSE".into(),
                size: 1,
                sha256: hash,
            },
        ],
    }
}

#[test]
fn rejects_replay_prerelease_wrong_platform_schema_and_windows_path_aliases() {
    let floor = "0.22.0".parse().unwrap();
    manifest().validate(&floor, 2).unwrap();
    assert!(package::payload_path(
        "licenses/dependencies/toml-1.1.8+spec-1.1.0/LICENSE-APACHE"
    ));
    for version in ["0.22.0", "0.21.0", "0.23.0-beta.1", "0.23.0+build"] {
        let mut m = manifest();
        m.version = version.parse().unwrap();
        assert!(m.validate(&floor, 2).is_err());
    }
    let mut m = manifest();
    m.target = "aarch64-pc-windows-msvc".into();
    assert!(m.validate(&floor, 2).is_err());
    let mut m = manifest();
    m.minimum_schema = 3;
    assert!(m.validate(&floor, 2).is_err());
    for path in [
        "config/settings.json",
        "../bad",
        "licenses/../bad",
        "licenses/C:/bad",
        "licenses/CON.txt",
        "licenses/file.",
        "licenses/file ",
        "licenses/a\\b",
        "/licenses/a",
        "licenses//a",
    ] {
        assert!(!package::payload_path(path), "{path}");
    }
    let mut m = manifest();
    m.payload[1].path = "PERIPH-IN-ONE.EXE".into();
    assert!(m.validate(&floor, 2).is_err());
}

fn archive(path: &Path, extra: Option<&str>) -> [u8; 32] {
    let mut unsigned = tempfile::tempfile().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let mut manifest = manifest();
    for (index, byte) in [1u8, 2].iter().enumerate() {
        let file = directory.path().join(index.to_string());
        fs::write(&file, [*byte]).unwrap();
        manifest.payload[index].sha256 = package::hash_file(&file).unwrap();
    }
    {
        let mut zip = zip::ZipWriter::new(&mut unsigned);
        for (name, bytes) in [
            ("ota.json", serde_json::to_vec(&manifest).unwrap()),
            ("periph-in-one.exe", vec![1]),
            ("licenses/LICENSE", vec![2]),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(&bytes).unwrap();
        }
        if let Some(extra) = extra {
            if extra == "__symlink" {
                zip.add_symlink(
                    "licenses/link",
                    "../../outside",
                    zip::write::SimpleFileOptions::default(),
                )
                .unwrap();
            } else {
                zip.start_file(extra, zip::write::SimpleFileOptions::default())
                    .unwrap();
                zip.write_all(&[3]).unwrap();
            }
        }
        zip.finish().unwrap();
    }
    use std::io::{Seek, SeekFrom};
    unsigned.seek(SeekFrom::Start(0)).unwrap();
    let key = zipsign_api::SigningKey::from_bytes(&[42; 32]);
    let public = key.verifying_key().to_bytes();
    let mut signed = fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    zipsign_api::sign::copy_and_sign_zip(
        &mut unsigned,
        &mut signed,
        &[key],
        Some(path.file_name().unwrap().to_str().unwrap().as_bytes()),
    )
    .unwrap();
    public
}

#[test]
fn rejects_empty_trust_store_and_signed_path_traversal_before_creating_staging() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("candidate.zip");
    let public = archive(&path, Some("../escaped"));
    let stage = dir.path().join("staged");
    let floor = "0.22.0".parse().unwrap();
    assert!(package::stage(&path, &stage, &[], &floor, 2).is_err());
    assert!(package::stage(&path, &stage, &[public], &floor, 2).is_err());
    assert!(!stage.exists());
    assert!(!dir.path().join("escaped").exists());
}

#[test]
fn valid_signature_and_payload_hashes_stage_exactly_the_whitelisted_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("candidate.zip");
    let public = archive(&path, None);
    let staging = dir.path().join("staged");
    let manifest =
        package::stage(&path, &staging, &[public], &"0.22.0".parse().unwrap(), 2).unwrap();
    assert_eq!(manifest.version.to_string(), "0.23.0");
    assert_eq!(fs::read(staging.join("periph-in-one.exe")).unwrap(), [1]);
    assert_eq!(fs::read(staging.join("licenses/LICENSE")).unwrap(), [2]);
    assert!(!staging.join("ota.json").exists());
}

#[test]
fn signed_symlinks_case_aliases_and_expansion_limits_are_rejected() {
    let floor = "0.22.0".parse().unwrap();
    for extra in ["__symlink", "licenses/license"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("candidate.zip");
        let public = archive(&path, Some(extra));
        let staging = dir.path().join("staged");
        assert!(matches!(
            package::stage(&path, &staging, &[public], &floor, 2),
            Err(package::StageError::Untrusted(_))
        ));
        assert!(!staging.exists());
    }
    let mut manifest = manifest();
    manifest.payload[0].size = update::MAX_EXPANDED + 1;
    assert!(manifest.validate(&floor, 2).is_err());
    manifest.payload[0].size = u64::MAX;
    assert!(manifest.validate(&floor, 2).is_err());
}

#[test]
fn temporary_disk_failures_are_distinct_from_untrusted_packages() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("candidate.zip");
    let public = archive(&path, None);
    let staging = dir.path().join("staged");
    fs::write(&staging, b"occupied").unwrap();
    assert!(matches!(
        package::stage(&path, &staging, &[public], &"0.22.0".parse().unwrap(), 2),
        Err(package::StageError::Unavailable(_))
    ));
}
