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

fn archive(path: &Path, extra: &str) -> [u8; 32] {
    let mut unsigned = tempfile::tempfile().unwrap();
    {
        let mut zip = zip::ZipWriter::new(&mut unsigned);
        for (name, bytes) in [
            ("ota.json", serde_json::to_vec(&manifest()).unwrap()),
            ("periph-in-one.exe", vec![1]),
            ("licenses/LICENSE", vec![2]),
            (extra, vec![3]),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(&bytes).unwrap();
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
    let public = archive(&path, "../escaped");
    let stage = dir.path().join("staged");
    let floor = "0.22.0".parse().unwrap();
    assert!(package::stage(&path, &stage, &[], &floor, 2).is_err());
    assert!(package::stage(&path, &stage, &[public], &floor, 2).is_err());
    assert!(!stage.exists());
    assert!(!dir.path().join("escaped").exists());
}
