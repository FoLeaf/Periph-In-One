use periph_in_one::update::{
    self,
    package::{self, Manifest, Payload},
};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

fn files(root: &Path, path: &Path, output: &mut Vec<Payload>) -> Result<(), String> {
    for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let metadata = fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err("License links are not supported".into());
        }
        if metadata.is_dir() {
            files(root, &entry.path(), output)?;
        } else {
            let name = format!(
                "licenses/{}",
                entry
                    .path()
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            );
            if !package::payload_path(&name) {
                return Err(format!("Invalid license path: {name}"));
            }
            output.push(Payload {
                path: name,
                size: metadata.len(),
                sha256: package::hash_file(&entry.path())?,
            });
        }
    }
    Ok(())
}

fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("prepare") if args.len() == 4 => prepare(Path::new(&args[1]), Path::new(&args[2]), &args[3])?,
        Some("pack") if args.len() == 5 => {
            let exe = Path::new(&args[1]);
            let licenses = Path::new(&args[2]);
            let output = Path::new(&args[3]);
            let version = semver::Version::parse(&args[4]).map_err(|e| e.to_string())?;
            let mut payload = vec![Payload { path: "periph-in-one.exe".into(), size: fs::metadata(exe).map_err(|e| e.to_string())?.len(), sha256: package::hash_file(exe)? }];
            files(licenses, licenses, &mut payload)?;
            payload.sort_by(|a,b| a.path.cmp(&b.path));
            let manifest = Manifest { app_id: update::APP_ID.into(), version, target: update::TARGET.into(), protocol: update::PROTOCOL, minimum_updater: update::UPDATER_VERSION, minimum_schema: 1, maximum_schema: 2, payload };
            manifest.validate(&semver::Version::new(0,0,0), 2)?;
            let target = fs::OpenOptions::new().create_new(true).write(true).open(output).map_err(|e| e.to_string())?;
            let mut zip = zip::ZipWriter::new(target);
            let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            zip.start_file("ota.json", options).map_err(|e| e.to_string())?;
            zip.write_all(&serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            for entry in manifest.payload {
                zip.start_file(&entry.path, options).map_err(|e| e.to_string())?;
                let source = if entry.path == "periph-in-one.exe" { exe.to_owned() } else { licenses.join(entry.path.strip_prefix("licenses/").unwrap()) };
                std::io::copy(&mut fs::File::open(source).map_err(|e| e.to_string())?, &mut zip).map_err(|e| e.to_string())?;
            }
            zip.finish().map_err(|e| e.to_string())?.sync_all().map_err(|e| e.to_string())?;
        }
        Some("sign") if args.len() == 2 => {
            let path = Path::new(&args[1]);
            // The protected job provides the seed. Never accept it in argv or log it.
            let secret = std::env::var("PERIPH_OTA_SIGNING_SEED").map_err(|_| "Signing secret is not configured")?;
            let seed: [u8;32] = hex::decode(secret.trim()).map_err(|_| "Invalid signing secret")?.try_into().map_err(|_| "Invalid signing secret")?;
            let key = zipsign_api::SigningKey::from_bytes(&seed);
            let public = key.verifying_key().to_bytes();
            if !update::trusted_keys()?.contains(&public) { return Err("Signing key does not match the client's embedded trust store".into()); }
            let mut signed = tempfile::NamedTempFile::new_in(path.parent().ok_or("Invalid output path")?).map_err(|e| e.to_string())?;
            let mut source = fs::File::open(path).map_err(|e| e.to_string())?;
            zipsign_api::sign::copy_and_sign_zip(&mut source, signed.as_file_mut(), &[key], Some(path.file_name().ok_or("Invalid filename")?.to_str().ok_or("Invalid filename")?.as_bytes())).map_err(|e| e.to_string())?;
            signed.as_file_mut().sync_all().map_err(|e| e.to_string())?;
            drop(source);
            signed.persist(path).map_err(|e| e.to_string())?;
            verify(path)?;
        }
        Some("verify") if args.len() == 2 => verify(Path::new(&args[1]))?,
        _ => return Err("Usage: ota-release pack <exe> <licenses> <output.zip> <version> | sign <package.zip> | verify <package.zip>".into()),
    }
    Ok(())
}

fn copy_tree(source: &Path, target: &Path) -> Result<(), String> {
    fs::create_dir_all(target).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(source).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let metadata = fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err("Links are not allowed in release input".into());
        }
        let target = target.join(entry.file_name());
        if metadata.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn archive_tree(
    root: &Path,
    directory: &Path,
    zip: &mut zip::ZipWriter<fs::File>,
) -> Result<(), String> {
    let mut entries = fs::read_dir(directory)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            archive_tree(root, &entry.path(), zip)?;
        } else {
            let path = entry.path();
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            zip.start_file(
                name,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated),
            )
            .map_err(|e| e.to_string())?;
            std::io::copy(&mut fs::File::open(path).map_err(|e| e.to_string())?, zip)
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn prepare(exe: &Path, output: &Path, version: &str) -> Result<(), String> {
    fs::create_dir_all(output).map_err(|e| e.to_string())?;
    let stage = tempfile::tempdir_in(output).map_err(|e| e.to_string())?;
    let root = stage.path();
    fs::copy(exe, root.join("periph-in-one.exe")).map_err(|e| e.to_string())?;
    for file in [
        "LICENSE",
        "README.md",
        "contribution.md",
        "docs/ADAPTATION.md",
        "docs/HARDWARE-ARCHITECTURE.md",
        ".github/CICD.md",
        "crates/periph-hal/README.md",
        "assets/slint-badge.svg",
    ] {
        let target = root.join(file);
        fs::create_dir_all(target.parent().unwrap()).map_err(|e| e.to_string())?;
        fs::copy(file, target).map_err(|e| e.to_string())?;
    }
    copy_tree(Path::new("assets/readme"), &root.join("assets/readme"))?;
    copy_tree(Path::new("licenses"), &root.join("licenses"))?;
    let metadata = std::process::Command::new("cargo")
        .args([
            "metadata",
            "--locked",
            "--format-version",
            "1",
            "--filter-platform",
            update::TARGET,
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if !metadata.status.success() {
        return Err("Cannot read dependency metadata".into());
    }
    let metadata: serde_json::Value =
        serde_json::from_slice(&metadata.stdout).map_err(|e| e.to_string())?;
    let mut notices = String::from(
        "# Dependency notices\n\nSlint is used under the Slint Royalty-free License 2.0.\n\n",
    );
    for dependency in metadata["packages"]
        .as_array()
        .ok_or("Missing dependency metadata")?
    {
        let name = dependency["name"].as_str().ok_or("Invalid package name")?;
        let dependency_version = dependency["version"]
            .as_str()
            .ok_or("Invalid package version")?;
        notices.push_str(&format!(
            "- {name}-{dependency_version}: {}\n",
            dependency["license"]
                .as_str()
                .unwrap_or("See package license")
        ));
        let source = Path::new(
            dependency["manifest_path"]
                .as_str()
                .ok_or("Missing manifest")?,
        )
        .parent()
        .ok_or("Invalid manifest")?;
        let target = root.join(format!("licenses/dependencies/{name}-{dependency_version}"));
        for entry in fs::read_dir(source).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().to_ascii_uppercase();
            if entry.file_type().map_err(|e| e.to_string())?.is_file()
                && ["LICENSE", "LICENCE", "COPYING", "NOTICE"]
                    .iter()
                    .any(|p| name.starts_with(p))
            {
                fs::create_dir_all(&target).map_err(|e| e.to_string())?;
                fs::copy(entry.path(), target.join(entry.file_name()))
                    .map_err(|e| e.to_string())?;
            }
        }
        if source.join("LICENSES").is_dir() {
            copy_tree(&source.join("LICENSES"), &target.join("LICENSES"))?;
        }
    }
    fs::write(root.join("licenses/DEPENDENCIES.md"), notices).map_err(|e| e.to_string())?;
    fs::write(
        root.join("SHA256SUMS.txt"),
        format!("{}  periph-in-one.exe\n", package::hash_file(exe)?),
    )
    .map_err(|e| e.to_string())?;
    let portable = output.join(format!("periph-in-one-{version}-windows-x64.zip"));
    let mut archive = zip::ZipWriter::new(
        fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&portable)
            .map_err(|e| e.to_string())?,
    );
    archive_tree(root, root, &mut archive)?;
    archive
        .finish()
        .map_err(|e| e.to_string())?
        .sync_all()
        .map_err(|e| e.to_string())?;
    let ota = output.join(format!("periph-in-one-{version}-windows-x64-ota.zip"));
    let status = std::process::Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
        .arg("pack")
        .arg(exe)
        .arg(root.join("licenses"))
        .arg(&ota)
        .arg(version)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("OTA packaging failed".into());
    }
    println!(
        "Portable: {}\nUnsigned OTA: {}",
        portable.display(),
        ota.display()
    );
    Ok(())
}

fn verify(path: &Path) -> Result<(), String> {
    let temporary = tempfile::tempdir().map_err(|e| e.to_string())?;
    let staging: PathBuf = temporary.path().join("staged");
    let manifest = package::stage(
        path,
        &staging,
        &update::trusted_keys()?,
        &semver::Version::new(0, 0, 0),
        2,
    )?;
    println!("Verified {} {}", manifest.app_id, manifest.version);
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
