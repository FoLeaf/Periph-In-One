use super::{
    MAX_EXPANDED, MAX_PACKAGE,
    package::{self, Manifest},
    scheduler,
    storage::{self, Phase, Preferences, State, Transaction},
};
use semver::Version;
use serde::Deserialize;
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError, Sender},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const API: &str = "https://api.github.com/repos/FoLeaf/Periph-In-One/releases";

pub enum Command {
    Automatic(bool),
    Check,
    Prepare,
    Stop,
}
#[derive(Clone, Debug)]
pub enum Event {
    Status {
        message: String,
        working: bool,
        automatic: bool,
        ready: bool,
    },
    Prepared(Box<Transaction>),
}

#[derive(Clone, Copy)]
pub struct Host {
    pub probe: fn(&Path, &Manifest) -> Result<(), String>,
    pub available_space: fn(&Path) -> Result<u64, String>,
    pub emit: fn(Event),
}

pub struct Controller {
    sender: Sender<Command>,
    cancelled: Arc<AtomicBool>,
}
impl Controller {
    pub fn send(&self, command: Command) {
        if matches!(command, Command::Automatic(false) | Command::Stop) {
            self.cancelled.store(true, Ordering::Release);
        }
        let _ = self.sender.send(command);
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.send(Command::Stop);
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn nonce() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| "无法初始化更新任务")?;
    Ok(hex::encode(bytes))
}

struct Sink<'a, W> {
    output: W,
    cancelled: &'a AtomicBool,
}
impl<W: Write> Write for Sink<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        self.output.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

#[derive(Debug)]
struct Failure {
    message: String,
    retry: Option<Duration>,
}
impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self {
            message,
            retry: None,
        }
    }
}
impl From<self_update::errors::Error> for Failure {
    fn from(error: self_update::errors::Error) -> Self {
        Self {
            message: "检查或下载失败，请稍后重试".into(),
            retry: error.rate_limit_delay(),
        }
    }
}

#[derive(Deserialize)]
struct Release {
    id: u64,
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}
#[derive(Deserialize)]
struct Asset {
    name: String,
    size: u64,
    browser_download_url: String,
}

fn release(id: Option<u64>, cancelled: &AtomicBool) -> Result<Release, Failure> {
    let url = id.map_or_else(|| format!("{API}/latest"), |id| format!("{API}/{id}"));
    let mut bytes = Vec::new();
    self_update::Download::from_url(url)
        .timeout(Duration::from_secs(25))
        .max_download_size(1024 * 1024)
        .request_header("Accept", "application/vnd.github+json")
        .download_to(&mut Sink {
            output: &mut bytes,
            cancelled,
        })?;
    let release: Release = serde_json::from_slice(&bytes)
        .map_err(|_| Failure::from("更新源返回无效数据".to_string()))?;
    if release.draft || release.prerelease {
        return Err("未找到正式发布的更新".to_string().into());
    }
    Ok(release)
}

fn status(
    host: Host,
    preferences: &Preferences,
    state: &State,
    message: impl Into<String>,
    working: bool,
) {
    (host.emit)(Event::Status {
        message: message.into(),
        working,
        automatic: preferences.automatic,
        ready: state
            .transaction
            .as_ref()
            .is_some_and(|t| t.phase == Phase::Verified),
    });
}

pub fn start(root: PathBuf, schema: u32, host: Host) -> Result<Controller, String> {
    let (sender, receiver) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let stop = cancelled.clone();
    thread::Builder::new()
        .name("ota-update".into())
        .spawn(move || {
            let initial = (|| -> Result<_, String> {
                let preferences: Preferences = storage::read(&storage::preferences_path(&root))?;
                let mut state: State = storage::read(&storage::state_path(&root))?;
                state.next_check = state
                    .next_check
                    .max(now() + scheduler::FIRST_CHECK.as_secs());
                Ok((preferences, state))
            })();
            let (mut preferences, mut state) = match initial {
                Ok(value) => value,
                Err(error) => {
                    status(
                        host,
                        &Preferences::default(),
                        &State::default(),
                        error,
                        false,
                    );
                    return;
                }
            };
            status(
                host,
                &preferences,
                &state,
                if state
                    .transaction
                    .as_ref()
                    .is_some_and(|t| t.phase == Phase::Verified)
                {
                    "更新已就绪"
                } else {
                    ""
                },
                false,
            );
            loop {
                let command = match receiver.recv_timeout(Duration::from_secs(1)) {
                    Ok(command) => command,
                    Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout)
                        if preferences.automatic && now() >= state.next_check =>
                    {
                        Command::Check
                    }
                    Err(RecvTimeoutError::Timeout) => continue,
                };
                match command {
                    Command::Stop => break,
                    Command::Automatic(enabled) => {
                        let next = Preferences { automatic: enabled };
                        match storage::write(&storage::preferences_path(&root), &next) {
                            Ok(()) => {
                                preferences = next;
                                state.next_check = now() + scheduler::FIRST_CHECK.as_secs();
                                status(host, &preferences, &state, "", false);
                            }
                            Err(error) => status(host, &preferences, &state, error, false),
                        }
                    }
                    Command::Check | Command::Prepare => {
                        let prepare = matches!(command, Command::Prepare);
                        stop.store(false, Ordering::Release);
                        status(
                            host,
                            &preferences,
                            &state,
                            if prepare {
                                "正在准备重启…"
                            } else {
                                "正在检查更新…"
                            },
                            true,
                        );
                        let _lock = match storage::lock(&root) {
                            Ok(lock) => lock,
                            Err(error) => {
                                status(host, &preferences, &state, error, false);
                                continue;
                            }
                        };
                        let result = (|| -> Result<(), Failure> {
                            // Reload after locking: another process or recovery may have committed.
                            state = storage::read(&storage::state_path(&root))?;
                            if prepare {
                                let transaction = state
                                    .transaction
                                    .as_ref()
                                    .filter(|t| t.phase == Phase::Verified)
                                    .ok_or_else(|| Failure::from("更新尚未就绪".to_string()))?;
                                let remote = release(Some(transaction.release_id), &stop)?;
                                if remote.tag_name != format!("v{}", transaction.manifest.version)
                                    || !remote
                                        .assets
                                        .iter()
                                        .any(|a| a.name == transaction.asset_name)
                                {
                                    return Err("此更新已撤回".to_string().into());
                                }
                                validate_cached(&root, transaction, schema, host)?;
                                if stop.load(Ordering::Acquire) {
                                    return Err("更新已取消".to_string().into());
                                }
                                (host.emit)(Event::Prepared(Box::new(transaction.clone())));
                                return Ok(());
                            }
                            if state
                                .transaction
                                .as_ref()
                                .is_some_and(|t| t.phase == Phase::Verified)
                            {
                                return Ok(());
                            }
                            let keys = super::trusted_keys()?;
                            let remote = release(None, &stop)?;
                            let current = Version::parse(env!("CARGO_PKG_VERSION"))
                                .map_err(|e| e.to_string())?;
                            let floor = state
                                .highest_accepted
                                .as_ref()
                                .unwrap_or(&current)
                                .max(&current);
                            let candidate = Version::parse(
                                remote
                                    .tag_name
                                    .strip_prefix('v')
                                    .ok_or_else(|| Failure::from("更新版本无效".to_string()))?,
                            )
                            .map_err(|e| e.to_string())?;
                            if candidate <= *floor
                                || !candidate.pre.is_empty()
                                || !candidate.build.is_empty()
                                || state.rejected.contains(&candidate)
                            {
                                return Ok(());
                            }
                            let name = format!("periph-in-one-{candidate}-windows-x64-ota.zip");
                            let asset =
                                remote
                                    .assets
                                    .iter()
                                    .find(|a| a.name == name)
                                    .ok_or_else(|| {
                                        Failure::from("新版本尚未提供可验证的更新包".to_string())
                                    })?;
                            if asset.size > MAX_PACKAGE
                                || !asset.browser_download_url.starts_with(
                                    "https://github.com/FoLeaf/Periph-In-One/releases/download/",
                                )
                            {
                                return Err("更新资产无效".to_string().into());
                            }
                            if (host.available_space)(&root)? < MAX_PACKAGE + MAX_EXPANDED * 2 {
                                return Err("可用空间不足，请清理后重试".to_string().into());
                            }
                            let id = nonce()?;
                            let directory = storage::transaction_path(&root, &id)?;
                            fs::create_dir(&directory).map_err(|_| "更新目录不可写".to_string())?;
                            let archive = directory.join(&name);
                            let mut file = fs::OpenOptions::new()
                                .write(true)
                                .create_new(true)
                                .open(&archive)
                                .map_err(|_| "更新目录不可写".to_string())?;
                            status(host, &preferences, &state, "正在下载更新…", true);
                            self_update::Download::from_url(&asset.browser_download_url)
                                .timeout(Duration::from_secs(120))
                                .max_download_size(MAX_PACKAGE)
                                .show_download_progress(false)
                                .download_to(&mut Sink {
                                    output: &mut file,
                                    cancelled: &stop,
                                })?;
                            file.sync_all().map_err(|_| "无法保存更新".to_string())?;
                            drop(file);
                            if fs::metadata(&archive).map_err(|e| e.to_string())?.len()
                                != asset.size
                            {
                                return Err("更新下载不完整".to_string().into());
                            }
                            let verified = (|| -> Result<Transaction, String> {
                                let manifest = package::stage(
                                    &archive,
                                    &directory.join("staged"),
                                    &keys,
                                    floor,
                                    schema,
                                )?;
                                if manifest.version != candidate {
                                    return Err("更新版本与发布不符".into());
                                }
                                (host.probe)(
                                    &directory.join("staged/periph-in-one.exe"),
                                    &manifest,
                                )?;
                                if stop.load(Ordering::Acquire) {
                                    return Err("更新已取消".into());
                                }
                                Ok(Transaction {
                                    id,
                                    release_id: remote.id,
                                    asset_name: name,
                                    archive_sha256: package::hash_file(&archive)?,
                                    manifest,
                                    phase: Phase::Verified,
                                    old_sha256: None,
                                    config_sha256: None,
                                    token: nonce()?,
                                })
                            })();
                            match verified {
                                Ok(transaction) => {
                                    state.highest_accepted = Some(candidate);
                                    state.transaction = Some(transaction);
                                }
                                Err(error) => {
                                    if !stop.load(Ordering::Acquire) {
                                        state.rejected.push(candidate);
                                        storage::write(&storage::state_path(&root), &state)?;
                                    }
                                    return Err(error.into());
                                }
                            }
                            Ok(())
                        })();
                        let (message, server) = match result {
                            Ok(()) => {
                                state.failures = 0;
                                (
                                    if state
                                        .transaction
                                        .as_ref()
                                        .is_some_and(|t| t.phase == Phase::Verified)
                                    {
                                        "更新已就绪"
                                    } else {
                                        "已是最新版本"
                                    }
                                    .to_string(),
                                    None,
                                )
                            }
                            Err(error) => {
                                state.failures = state.failures.saturating_add(1);
                                (error.message, error.retry)
                            }
                        };
                        state.next_check = now().saturating_add(
                            scheduler::delay(
                                state.failures,
                                now() ^ std::process::id() as u64,
                                server,
                            )
                            .as_secs(),
                        );
                        if let Err(error) = storage::write(&storage::state_path(&root), &state) {
                            status(host, &preferences, &state, error, false);
                        } else {
                            status(host, &preferences, &state, message, false);
                        }
                    }
                }
            }
        })
        .map_err(|_| "无法启动更新服务")?;
    Ok(Controller { sender, cancelled })
}

pub fn validate_cached(
    root: &Path,
    transaction: &Transaction,
    schema: u32,
    host: Host,
) -> Result<(), String> {
    let directory = storage::transaction_path(root, &transaction.id)?;
    let expected_name = format!(
        "periph-in-one-{}-windows-x64-ota.zip",
        transaction.manifest.version
    );
    if transaction.asset_name != expected_name
        || package::hash_file(&directory.join(&expected_name))? != transaction.archive_sha256
    {
        return Err("缓存的更新包校验失败".into());
    }
    let check = directory.join(nonce()?);
    let manifest = package::stage(
        &directory.join(expected_name),
        &check,
        &super::trusted_keys()?,
        &Version::parse(env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string())?,
        schema,
    )?;
    for payload in &manifest.payload {
        let path = directory.join("staged").join(&payload.path);
        if fs::symlink_metadata(&path)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
            || package::hash_file(&path)? != payload.sha256
        {
            return Err("缓存的更新文件校验失败".into());
        }
    }
    (host.probe)(&directory.join("staged/periph-in-one.exe"), &manifest)
}
