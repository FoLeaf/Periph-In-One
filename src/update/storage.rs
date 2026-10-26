use super::package::Manifest;
use semver::Version;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Preferences {
    // The bootstrap stays opt-in until the real cross-release acceptance gate.
    pub automatic: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    Verified,
    Preparing,
    Applying,
    AwaitingHealth,
    Committed,
    RolledBack,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transaction {
    pub id: String,
    pub release_id: u64,
    pub asset_name: String,
    pub archive_sha256: String,
    pub manifest: Manifest,
    pub phase: Phase,
    pub old_sha256: Option<String>,
    #[serde(default)]
    pub old_payload: Vec<super::package::Payload>,
    pub config_sha256: Option<String>,
    pub token: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct State {
    pub release: Option<ReleaseInfo>,
    pub highest_accepted: Option<Version>,
    pub rejected: Vec<Version>,
    pub transaction: Option<Transaction>,
    pub next_check: u64,
    pub failures: u32,
    pub retry_after: u64,
    pub recovery_backup: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseInfo {
    pub version: Version,
    pub tag: String,
    pub body: String,
}

pub fn read<T: DeserializeOwned + Default>(path: &Path) -> Result<T, String> {
    match fs::read(path) {
        Ok(bytes) if bytes.len() <= 8 * 1024 * 1024 => {
            serde_json::from_slice(&bytes).map_err(|_| "更新状态损坏，请保留原文件".into())
        }
        Ok(_) => Err("更新状态文件过大".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(_) => Err("无法读取更新设置".into()),
    }
}

pub fn write<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let parent = path.parent().ok_or("更新目录无效")?;
    fs::create_dir_all(parent).map_err(|_| "更新目录不可写")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|_| "更新目录不可写")?;
    serde_json::to_writer_pretty(temporary.as_file_mut(), value).map_err(|_| "无法保存更新状态")?;
    temporary
        .write_all(b"\n")
        .and_then(|_| temporary.as_file_mut().sync_all())
        .map_err(|_| "无法保存更新状态")?;
    temporary.persist(path).map_err(|_| "无法保存更新状态")?;
    Ok(())
}

pub fn state_path(root: &Path) -> PathBuf {
    root.join(".ota/state.json")
}
pub fn preferences_path(root: &Path) -> PathBuf {
    root.join("config/update-settings.json")
}

pub fn transaction_path(root: &Path, id: &str) -> Result<PathBuf, String> {
    if id.len() != 32 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("更新事务无效".into());
    }
    Ok(root.join(".ota").join(id))
}

pub fn lock(root: &Path) -> Result<fs::File, String> {
    super::transaction::plain(root, &root.join(".ota/task.lock"))?;
    fs::create_dir_all(root.join(".ota")).map_err(|_| "更新目录不可写")?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(".ota/task.lock"))
        .map_err(|_| "无法创建更新锁")?;
    file.try_lock().map_err(|_| "另一个更新任务正在运行")?;
    Ok(file)
}
