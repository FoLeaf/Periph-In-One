use super::{APP_ID, MAX_EXPANDED, MAX_PACKAGE, PROTOCOL, TARGET, UPDATER_VERSION};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    path::Path,
};

#[derive(Debug)]
pub enum StageError {
    Untrusted(String),
    Unavailable(String),
}
impl From<&str> for StageError {
    fn from(value: &str) -> Self {
        Self::Untrusted(value.into())
    }
}
impl From<String> for StageError {
    fn from(value: String) -> Self {
        Self::Unavailable(value)
    }
}
impl std::fmt::Display for StageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Untrusted(message) | Self::Unavailable(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for StageError {}
impl From<StageError> for String {
    fn from(value: StageError) -> Self {
        value.to_string()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Payload {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub app_id: String,
    pub version: Version,
    pub target: String,
    pub protocol: u32,
    pub minimum_updater: u32,
    pub minimum_schema: u32,
    pub maximum_schema: u32,
    pub payload: Vec<Payload>,
}

pub fn payload_path(path: &str) -> bool {
    if path == "periph-in-one.exe" {
        return true;
    }
    path.starts_with("licenses/")
        && path.len() < 240
        && path.split('/').all(|part| {
            !part.is_empty()
                && !matches!(part, "." | "..")
                && !part.ends_with('.')
                && !part.ends_with(' ')
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_. +".contains(&c))
                && !matches!(
                    part.split('.')
                        .next()
                        .unwrap_or("")
                        .to_ascii_uppercase()
                        .as_str(),
                    "CON"
                        | "PRN"
                        | "AUX"
                        | "NUL"
                        | "COM1"
                        | "COM2"
                        | "COM3"
                        | "COM4"
                        | "COM5"
                        | "COM6"
                        | "COM7"
                        | "COM8"
                        | "COM9"
                        | "LPT1"
                        | "LPT2"
                        | "LPT3"
                        | "LPT4"
                        | "LPT5"
                        | "LPT6"
                        | "LPT7"
                        | "LPT8"
                        | "LPT9"
                )
        })
}

pub fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hex::encode(hash.finalize()))
}

impl Manifest {
    pub fn validate(&self, floor: &Version, schema: u32) -> Result<(), String> {
        if self.app_id != APP_ID
            || self.target != TARGET
            || self.protocol != PROTOCOL
            || self.minimum_updater > UPDATER_VERSION
        {
            return Err("此更新与当前程序不兼容".into());
        }
        if self.version <= *floor || !self.version.pre.is_empty() || !self.version.build.is_empty()
        {
            return Err("更新版本无效或已过期".into());
        }
        if self.minimum_schema > schema
            || self.maximum_schema < schema
            || self.minimum_schema > self.maximum_schema
        {
            return Err("此更新无法读取当前配置".into());
        }
        let mut names = BTreeSet::new();
        let mut size = 0u64;
        if self.payload.is_empty() || self.payload.len() > 4096 {
            return Err("更新文件清单无效".into());
        }
        for entry in &self.payload {
            if !payload_path(&entry.path)
                || !names.insert(entry.path.to_ascii_lowercase())
                || entry.sha256.len() != 64
                || !entry.sha256.bytes().all(|c| c.is_ascii_hexdigit())
            {
                return Err("更新文件清单无效".into());
            }
            size = size.checked_add(entry.size).ok_or("更新包过大")?;
            if size > MAX_EXPANDED {
                return Err("更新包过大".into());
            }
        }
        if !names.contains("periph-in-one.exe") || !names.iter().any(|n| n.starts_with("licenses/"))
        {
            return Err("更新缺少程序或许可文件".into());
        }
        Ok(())
    }
}

/// Verify first; never extract or execute bytes from an unsigned archive. The
/// caller supplies an empty, private staging directory on the installation volume.
pub fn stage(
    path: &Path,
    destination: &Path,
    keys: &[[u8; 32]],
    floor: &Version,
    schema: u32,
) -> Result<Manifest, StageError> {
    if keys.is_empty() {
        return Err("此构建尚未配置更新签名".into());
    }
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_PACKAGE {
        return Err("更新包过大或无效".into());
    }
    self_update::verify_signature(path, keys).map_err(|error| {
        if matches!(error, self_update::errors::Error::Io(_)) {
            StageError::Unavailable("无法读取更新包，请稍后重试".into())
        } else {
            StageError::Untrusted("更新签名验证失败".into())
        }
    })?;
    let mut archive = zip::ZipArchive::new(fs::File::open(path).map_err(|e| e.to_string())?)
        .map_err(|_| "更新包损坏")?;
    if archive.len() > 4097 {
        return Err("更新文件过多".into());
    }
    let mut json = Vec::new();
    archive
        .by_name("ota.json")
        .map_err(|_| "更新缺少版本清单")?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut json)
        .map_err(|e| e.to_string())?;
    if json.len() > 1024 * 1024 {
        return Err("更新清单过大".into());
    }
    let manifest: Manifest = serde_json::from_slice(&json).map_err(|_| "更新清单损坏")?;
    manifest
        .validate(floor, schema)
        .map_err(StageError::Untrusted)?;
    let mut seen = BTreeSet::new();
    let mut total = 0u64;
    // Validate the entire archive before creating even the first output file.
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|_| "更新包损坏")?;
        let name = entry.name().map_err(|_| "更新路径编码无效")?;
        if !seen.insert(name.to_ascii_lowercase())
            || entry.is_dir()
            || entry.is_symlink()
            || entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 != 0 && mode & 0o170000 != 0o100000)
            || (name != "ota.json" && !payload_path(&name))
        {
            return Err("更新包含有不允许的路径".into());
        }
        if name != "ota.json" {
            let payload = manifest
                .payload
                .iter()
                .find(|p| p.path == name)
                .ok_or("更新包含清单外文件")?;
            if payload.size != entry.size() {
                return Err("更新文件大小不符".into());
            }
        }
        total = total.checked_add(entry.size()).ok_or("更新包过大")?;
        if total > MAX_EXPANDED {
            return Err("更新包过大".into());
        }
    }
    if seen.len() != manifest.payload.len() + 1 {
        return Err("更新包缺少文件".into());
    }
    // create_dir refuses existing directories and reparse points. Only freshly
    // created descendants are used; no ZIP metadata or link attributes restored.
    fs::create_dir(destination).map_err(|_| "无法创建更新暂存目录".to_string())?;
    for payload in &manifest.payload {
        let output = destination.join(&payload.path);
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut source = archive.by_name(&payload.path).map_err(|_| "更新包损坏")?;
        let mut target = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&output)
            .map_err(|e| e.to_string())?;
        let count = std::io::copy(&mut source.by_ref().take(payload.size + 1), &mut target)
            .map_err(|_| "无法解压更新".to_string())?;
        target
            .flush()
            .and_then(|_| target.sync_all())
            .map_err(|_| "无法保存更新".to_string())?;
        if count != payload.size || hash_file(&output)? != payload.sha256.to_ascii_lowercase() {
            return Err("更新文件校验失败".into());
        }
    }
    Ok(manifest)
}
