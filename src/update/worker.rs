use super::{
    MAX_EXPANDED, MAX_PACKAGE,
    package::{self, Manifest},
    scheduler,
    storage::{self, Phase, Preferences, ReleaseInfo, State, Transaction},
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
    PreparationFailed(String),
    Release(Option<Box<ReleaseInfo>>),
    Preference {
        automatic: bool,
        error: Option<String>,
    },
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
    preferences: Option<Sender<bool>>,
    preference_writer: Option<thread::JoinHandle<()>>,
}
impl Controller {
    pub fn send(&self, command: Command) {
        if matches!(command, Command::Automatic(false) | Command::Stop) {
            self.cancelled.store(true, Ordering::Release);
        }
        if let Command::Automatic(enabled) = command {
            if let Some(preferences) = &self.preferences {
                let _ = preferences.send(enabled);
            }
            return;
        }
        let _ = self.sender.send(command);
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.send(Command::Stop);
        self.preferences.take();
        if let Some(writer) = self.preference_writer.take() {
            let _ = writer.join();
        }
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
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "cancelled",
            ));
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
    body: Option<String>,
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
    decode_release(&bytes)
}

fn decode_release(bytes: &[u8]) -> Result<Release, Failure> {
    let release: Release = serde_json::from_slice(bytes)
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
        ready: pending(state),
    });
}

fn pending(state: &State) -> bool {
    state.transaction.as_ref().is_some_and(|t| {
        t.phase == Phase::Verified
            && state
                .release
                .as_ref()
                .is_some_and(|release| release.version == t.manifest.version)
            && t.manifest.version
                > env!("CARGO_PKG_VERSION")
                    .parse::<Version>()
                    .expect("Cargo version")
    })
}

pub fn start(root: PathBuf, schema: u32, host: Host) -> Result<Controller, String> {
    let (sender, receiver) = mpsc::channel();
    let (preferences, requests) = mpsc::channel::<bool>();
    let preference_root = root.clone();
    let command_sender = sender.clone();
    // Preference writes never wait behind a stalled network request. Shutdown
    // joins this short FIFO writer so disabling updates survives an immediate quit.
    let preference_writer = thread::Builder::new()
        .name("ota-preferences".into())
        .spawn(move || {
            let path = storage::preferences_path(&preference_root);
            let mut saved = storage::read::<Preferences>(&path).ok();
            for enabled in requests {
                let next = Preferences { automatic: enabled };
                let result = if saved.is_none() {
                    Err("无法读取更新设置，请保留原文件".into())
                } else {
                    super::transaction::plain(&preference_root, &path)
                        .and_then(|_| storage::write(&path, &next))
                };
                match result {
                    Ok(()) => {
                        saved = Some(next);
                        (host.emit)(Event::Preference {
                            automatic: enabled,
                            error: None,
                        });
                        let _ = command_sender.send(Command::Automatic(enabled));
                    }
                    Err(error) => (host.emit)(Event::Preference {
                        automatic: saved.as_ref().is_some_and(|p| p.automatic),
                        error: Some(error),
                    }),
                }
            }
        })
        .map_err(|_| "无法启动更新设置服务")?;
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
                if pending(&state) {
                    "更新已就绪"
                } else if state
                    .transaction
                    .as_ref()
                    .is_some_and(|t| t.phase == Phase::RolledBack)
                {
                    "上次更新未成功，已恢复旧版本"
                } else {
                    ""
                },
                false,
            );
            let current: Version = env!("CARGO_PKG_VERSION").parse().expect("Cargo version");
            (host.emit)(Event::Release(
                state
                    .release
                    .clone()
                    .filter(|r| r.version > current && !state.rejected.contains(&r.version))
                    .map(Box::new),
            ));
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
                        preferences.automatic = enabled;
                        state.next_check = now() + scheduler::FIRST_CHECK.as_secs();
                        status(host, &preferences, &state, "", false);
                    }
                    Command::Check | Command::Prepare => {
                        let prepare = matches!(command, Command::Prepare);
                        if now() < state.retry_after {
                            status(
                                host,
                                &preferences,
                                &state,
                                "更新服务繁忙，请稍后重试",
                                false,
                            );
                            continue;
                        }
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
                                if !pending(&state) {
                                    return Err("更新尚未就绪".to_string().into());
                                }
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
                                if let Err(error) =
                                    validate_cached(&root, transaction, schema, host)
                                {
                                    if matches!(error, package::StageError::Untrusted(_)) {
                                        state.rejected.push(transaction.manifest.version.clone());
                                        state.transaction = None;
                                        state.release = None;
                                        (host.emit)(Event::Release(None));
                                    }
                                    return Err(error.to_string().into());
                                }
                                if stop.load(Ordering::Acquire) {
                                    return Err("更新已取消".to_string().into());
                                }
                                (host.emit)(Event::Prepared(Box::new(transaction.clone())));
                                return Ok(());
                            }
                            if state
                                .transaction
                                .as_ref()
                                .is_some_and(|t| t.phase == Phase::Verified && !pending(&state))
                            {
                                state.transaction = None;
                            }
                            super::transaction::cleanup(&root, &state);
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
                            if state.rejected.contains(&candidate) {
                                return Err("此版本更新未通过验证，请等待后续版本"
                                    .to_string()
                                    .into());
                            }
                            if candidate <= *floor
                                || !candidate.pre.is_empty()
                                || !candidate.build.is_empty()
                            {
                                return Ok(());
                            }
                            state.release = Some(ReleaseInfo {
                                version: candidate.clone(),
                                tag: remote.tag_name.clone(),
                                body: remote.body.clone().unwrap_or_default(),
                            });
                            (host.emit)(Event::Release(state.release.clone().map(Box::new)));
                            let keys = super::trusted_keys()?;
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
                            storage::write(&directory.join("owner.json"), &super::APP_ID)?;
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
                            let mut quarantine = false;
                            let verified = (|| -> Result<Transaction, String> {
                                let manifest = package::stage(
                                    &archive,
                                    &directory.join("staged"),
                                    &keys,
                                    floor,
                                    schema,
                                )
                                .map_err(|error| {
                                    quarantine = matches!(error, package::StageError::Untrusted(_));
                                    error.to_string()
                                })?;
                                if manifest.version != candidate {
                                    quarantine = true;
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
                                    old_payload: Vec::new(),
                                    config_sha256: None,
                                    token: nonce()?,
                                })
                            })();
                            match verified {
                                Ok(transaction) => {
                                    state.recovery_backup = state
                                        .transaction
                                        .as_ref()
                                        .filter(|t| {
                                            matches!(t.phase, Phase::Committed | Phase::RolledBack)
                                        })
                                        .map(|t| t.id.clone());
                                    state.highest_accepted = Some(candidate);
                                    state.transaction = Some(transaction);
                                }
                                Err(error) => {
                                    if quarantine && !stop.load(Ordering::Acquire) {
                                        state.rejected.push(candidate);
                                        state.release = None;
                                        (host.emit)(Event::Release(None));
                                        storage::write(&storage::state_path(&root), &state)?;
                                    }
                                    return Err(error.into());
                                }
                            }
                            Ok(())
                        })();
                        let preparation_failed = prepare && result.is_err();
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
                        state.retry_after =
                            server.map_or(0, |delay| now().saturating_add(delay.as_secs()));
                        super::transaction::diagnostic(
                            &root,
                            if state.failures == 0 {
                                "check-ok"
                            } else {
                                "check-failed"
                            },
                        );
                        state.next_check = now().saturating_add(
                            scheduler::delay(
                                state.failures,
                                now() ^ std::process::id() as u64,
                                server,
                            )
                            .as_secs(),
                        );
                        if let Err(error) = storage::write(&storage::state_path(&root), &state) {
                            state = storage::read(&storage::state_path(&root)).unwrap_or_default();
                            status(host, &preferences, &state, error, false);
                        } else {
                            if preparation_failed {
                                (host.emit)(Event::PreparationFailed(message.clone()));
                            }
                            status(host, &preferences, &state, message, false);
                        }
                    }
                }
            }
        })
        .map_err(|_| "无法启动更新服务")?;
    Ok(Controller {
        sender,
        cancelled,
        preferences: Some(preferences),
        preference_writer: Some(preference_writer),
    })
}

pub fn validate_cached(
    root: &Path,
    transaction: &Transaction,
    schema: u32,
    host: Host,
) -> Result<(), package::StageError> {
    let directory = storage::transaction_path(root, &transaction.id)?;
    super::transaction::plain(root, &directory)?;
    let expected_name = format!(
        "periph-in-one-{}-windows-x64-ota.zip",
        transaction.manifest.version
    );
    if transaction.asset_name != expected_name
        || package::hash_file(&directory.join(&expected_name))? != transaction.archive_sha256
    {
        return Err("缓存的更新包校验失败".into());
    }
    let check_directory =
        tempfile::tempdir_in(&directory).map_err(|_| "更新目录不可写".to_string())?;
    let check = check_directory.path().join("verified");
    let manifest = package::stage(
        &directory.join(expected_name),
        &check,
        &super::trusted_keys()?,
        &Version::parse(env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string())?,
        schema,
    )?;
    if manifest != transaction.manifest {
        return Err("缓存的更新清单不一致".into());
    }
    for payload in &manifest.payload {
        let path = directory.join("staged").join(&payload.path);
        super::transaction::plain(root, &path)?;
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
        .map_err(package::StageError::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn changelog_and_tag_are_read_from_the_same_public_release_response() {
        let bytes = br###"{"id":1,"tag_name":"v0.23.0","body":"## Fixes\n- Battery","draft":false,"prerelease":false,"assets":[]}"###;
        let release = decode_release(bytes).unwrap();
        assert_eq!(release.tag_name, "v0.23.0");
        assert_eq!(release.body.as_deref(), Some("## Fixes\n- Battery"));
        for flags in [(true, false), (false, true)] {
            let json = format!(
                r#"{{"id":1,"tag_name":"v0.23.0","body":null,"draft":{},"prerelease":{},"assets":[]}}"#,
                flags.0, flags.1
            );
            assert!(decode_release(json.as_bytes()).is_err());
        }
    }
    #[test]
    fn disabling_updates_is_durable_even_when_the_network_worker_stops_first() {
        let root = tempfile::tempdir().unwrap();
        storage::write(
            &storage::preferences_path(root.path()),
            &Preferences { automatic: true },
        )
        .unwrap();
        let controller = start(
            root.path().to_owned(),
            2,
            Host {
                probe: |_, _| Ok(()),
                available_space: |_| Ok(u64::MAX),
                emit: |_| {},
            },
        )
        .unwrap();
        controller.send(Command::Automatic(false));
        drop(controller);
        assert!(
            !storage::read::<Preferences>(&storage::preferences_path(root.path()))
                .unwrap()
                .automatic
        );
    }
    #[test]
    fn cancellation_aborts_copy_without_interrupted_write_retry_loop() {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let cancelled = AtomicBool::new(true);
            let result = io::copy(
                &mut io::Cursor::new(b"payload"),
                &mut Sink {
                    output: Vec::new(),
                    cancelled: &cancelled,
                },
            );
            sender.send(result.unwrap_err().kind()).unwrap();
        });
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            io::ErrorKind::ConnectionAborted
        );
    }
}
