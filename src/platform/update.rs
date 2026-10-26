use periph_in_one::{
    config::Config,
    update::{self, package::Manifest},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    os::windows::{ffi::OsStrExt, process::CommandExt},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Serialize, Deserialize)]
struct Probe {
    version: String,
    target: String,
    protocol: u32,
    schema: u32,
}

pub fn internal_entry() -> Option<Result<(), String>> {
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--ota-agent")) {
        return Some(agent());
    }
    if std::env::args_os().nth(1).as_deref() != Some(std::ffi::OsStr::new("--ota-probe")) {
        return None;
    }
    Some((|| {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let staged = exe.parent().ok_or("更新路径无效")?;
        let transaction = staged.parent().ok_or("更新路径无效")?;
        let ota = transaction.parent().ok_or("更新路径无效")?;
        if staged.file_name().and_then(|n| n.to_str()) != Some("staged")
            || ota.file_name().and_then(|n| n.to_str()) != Some(".ota")
        {
            return Err("预检只能从更新暂存目录运行".into());
        }
        let root = ota.parent().ok_or("更新路径无效")?;
        let checkpoint = tempfile::tempdir().map_err(|e| e.to_string())?;
        let source = root.join("config/settings.json");
        let copy = checkpoint.path().join("settings.json");
        if source.exists() {
            fs::copy(source, &copy).map_err(|_| "无法读取当前配置")?;
        }
        let config = Config::load(&copy).map_err(|_| "新版本无法读取当前配置")?;
        let result = Probe {
            version: env!("CARGO_PKG_VERSION").into(),
            target: update::BUILD_TARGET.into(),
            protocol: update::PROTOCOL,
            schema: config.version,
        };
        println!(
            "{}",
            serde_json::to_string(&result).map_err(|e| e.to_string())?
        );
        Ok(())
    })())
}

use periph_in_one::update::{
    package,
    storage::{self, Phase, State, Transaction},
    transaction, worker,
};
use std::sync::atomic::{AtomicBool, Ordering};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
    System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
        WaitForSingleObject,
    },
};

pub fn root() -> Result<std::path::PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if exe.file_name().and_then(|p| p.to_str()) != Some("periph-in-one.exe") {
        return Err("更新需要使用原始程序文件名".into());
    }
    Ok(exe.parent().ok_or("无法确定程序目录")?.to_owned())
}

fn task_lock(root: &Path, timeout: Duration) -> Result<fs::File, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(lock) = storage::lock(root) {
            return Ok(lock);
        }
        if Instant::now() >= deadline {
            return Err("正在完成另一个更新任务，请稍后重试".into());
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn load_transaction(root: &Path, id: &str) -> Result<Transaction, String> {
    let _: std::path::PathBuf = storage::transaction_path(root, id)?;
    let state: State = storage::read(&storage::state_path(root))?;
    state
        .transaction
        .filter(|t| t.id == id)
        .ok_or("更新事务已变化".into())
}

pub struct Ready {
    root: std::path::PathBuf,
    transaction: Transaction,
    released: bool,
}
impl Ready {
    pub fn release(mut self) -> Result<(), String> {
        let directory = storage::transaction_path(&self.root, &self.transaction.id)?;
        storage::write(&directory.join("go.json"), &self.transaction.token)?;
        self.released = true;
        Ok(())
    }
}
impl Drop for Ready {
    fn drop(&mut self) {
        if !self.released
            && let Ok(path) = storage::transaction_path(&self.root, &self.transaction.id)
        {
            let _ = storage::write(&path.join("abort.json"), &self.transaction.token);
        }
    }
}

pub fn handoff(expected: &Transaction, cancelled: &AtomicBool) -> Result<Ready, String> {
    let root = root()?;
    let _lock = task_lock(&root, Duration::from_secs(5))?;
    worker::validate_cached(
        &root,
        expected,
        2,
        worker::Host {
            probe,
            available_space,
            emit: |_| {},
        },
    )?;
    if cancelled.load(Ordering::Acquire) {
        return Err("更新已延后".into());
    }
    let transaction = transaction::prepare(&root, expected)?;
    drop(_lock);
    let result = launch_agent(root.clone(), transaction.clone(), "apply", cancelled);
    if result.is_err()
        && let Ok(_lock) = task_lock(&root, Duration::from_secs(1))
    {
        let _ = transaction::cancel(&root, &transaction);
    }
    result
}

fn launch_agent(
    root: std::path::PathBuf,
    transaction: Transaction,
    mode: &str,
    cancelled: &AtomicBool,
) -> Result<Ready, String> {
    let directory = storage::transaction_path(&root, &transaction.id)?;
    transaction::plain(&root, &directory)?;
    for name in [
        "ready.json",
        "go.json",
        "abort.json",
        "health.json",
        "user-exit.json",
    ] {
        let path = directory.join(name);
        transaction::plain(&root, &path)?;
        if path.exists() {
            fs::remove_file(path).map_err(|_| "无法重置更新交接")?;
        }
    }
    let mut child = Command::new(directory.join("agent.exe"))
        .arg("--ota-agent")
        .arg(&transaction.id)
        .arg(std::process::id().to_string())
        .arg(mode)
        .current_dir(&root)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "更新助手无法启动，已保留当前版本")?;
    let ready = Ready {
        root,
        transaction,
        released: false,
    };
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err("更新已延后".into());
        }
        if signal(&directory.join("ready.json"), &ready.transaction.token) {
            return Ok(ready);
        }
        if child
            .try_wait()
            .map_err(|_| "无法确认更新助手状态")?
            .is_some()
            || Instant::now() >= deadline
        {
            return Err("更新助手未就绪，已保留当前版本".into());
        }
        thread::sleep(Duration::from_millis(40));
    }
}

fn signal(path: &Path, token: &str) -> bool {
    storage::read::<String>(path).is_ok_and(|s| s == token)
}

struct Process(HANDLE);
impl Drop for Process {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn parent_process(pid: u32, expected: &Path) -> Result<Process, String> {
    // The parent remains alive until our ready acknowledgment. Acquire and
    // validate its process handle before acknowledging; never poll a bare PID.
    let handle = unsafe { OpenProcess(0x00100000 | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return Err("原进程已退出，已取消交接".into());
    }
    let process = Process(handle);
    let mut name = vec![0u16; 32768];
    let mut size = name.len() as u32;
    if unsafe { QueryFullProcessImageNameW(handle, 0, name.as_mut_ptr(), &mut size) } == 0 {
        return Err("无法确认原进程".into());
    }
    let actual = std::path::PathBuf::from(String::from_utf16_lossy(&name[..size as usize]));
    if fs::canonicalize(actual).map_err(|_| "无法确认原进程路径")?
        != fs::canonicalize(expected).map_err(|_| "无法确认程序路径")?
    {
        return Err("原进程路径不匹配".into());
    }
    Ok(process)
}

fn agent() -> Result<(), String> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments.len() != 4 || !matches!(arguments[3].as_str(), "apply" | "recover") {
        return Err("更新助手参数无效".into());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let directory = exe.parent().ok_or("更新助手路径无效")?;
    let ota = directory.parent().ok_or("更新助手路径无效")?;
    let root = ota.parent().ok_or("更新助手路径无效")?;
    if exe.file_name().and_then(|v| v.to_str()) != Some("agent.exe")
        || ota.file_name().and_then(|v| v.to_str()) != Some(".ota")
        || directory != storage::transaction_path(root, &arguments[1])?
    {
        return Err("更新助手路径无效".into());
    }
    let _lock = task_lock(root, Duration::from_secs(5))?;
    let transaction = load_transaction(root, &arguments[1])?;
    if !matches!(
        transaction.phase,
        Phase::Preparing | Phase::Applying | Phase::AwaitingHealth
    ) || Some(package::hash_file(&exe)?) != transaction.old_sha256
    {
        return Err("更新助手身份验证失败".into());
    }
    transaction::verify_backup(root, &transaction)?;
    worker::validate_cached(
        root,
        &transaction,
        2,
        worker::Host {
            probe: |_, _| Ok(()),
            available_space,
            emit: |_| {},
        },
    )?;
    let parent = parent_process(
        arguments[2].parse().map_err(|_| "原进程参数无效")?,
        &root.join("periph-in-one.exe"),
    )?;
    storage::write(&directory.join("ready.json"), &transaction.token)?;
    let deadline = Instant::now() + Duration::from_secs(20);
    while !signal(&directory.join("go.json"), &transaction.token) {
        if signal(&directory.join("abort.json"), &transaction.token) || Instant::now() >= deadline {
            if transaction.phase == Phase::Preparing {
                transaction::cancel(root, &transaction)?;
            }
            return Ok(());
        }
        thread::sleep(Duration::from_millis(40));
    }
    if unsafe { WaitForSingleObject(parent.0, 30_000) } != WAIT_OBJECT_0 {
        if transaction.phase == Phase::Preparing {
            transaction::cancel(root, &transaction)?;
        }
        return Err("原进程尚未退出，已取消更新".into());
    }
    transaction::diagnostic(root, "parent-exited");
    let apply = if arguments[3] == "apply" {
        apply_and_confirm(root, &transaction)
    } else {
        Err("恢复未完成更新".into())
    };
    if apply.is_err() {
        transaction::diagnostic(root, "rollback-start");
        transaction::rollback(root, &transaction)?;
        transaction::diagnostic(root, "rolled-back");
        if !signal(&directory.join("user-exit.json"), &transaction.token) {
            Command::new(root.join("periph-in-one.exe"))
                .arg("--autostart")
                .current_dir(root)
                .creation_flags(CREATE_NO_WINDOW)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|_| "旧版本已恢复，请重新启动")?;
        }
    }
    Ok(())
}

#[derive(Default, Serialize, Deserialize)]
struct Heartbeat {
    token: String,
    counter: u64,
}

fn apply_and_confirm(root: &Path, transaction: &Transaction) -> Result<(), String> {
    transaction::apply(root, transaction)?;
    transaction::diagnostic(root, "awaiting-health");
    let mut child = Command::new(root.join("periph-in-one.exe"))
        .arg("--ota-resume")
        .arg(&transaction.id)
        .arg(&transaction.token)
        .current_dir(root)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "新版本无法启动")?;
    let directory = storage::transaction_path(root, &transaction.id)?;
    let startup_deadline = Instant::now() + Duration::from_secs(30);
    let mut health_deadline = None;
    let mut counter = 0;
    let mut last_progress = Instant::now();
    let result = (|| {
        loop {
            if child
                .try_wait()
                .map_err(|_| "无法确认新版本状态")?
                .is_some()
            {
                return Err("新版本提前退出".into());
            }
            if let Ok(health) = storage::read::<Heartbeat>(&directory.join("health.json"))
                && health.token == transaction.token
                && health.counter > counter
            {
                counter = health.counter;
                last_progress = Instant::now();
                health_deadline.get_or_insert(Instant::now() + Duration::from_secs(30));
            }
            if (counter == 0 && Instant::now() >= startup_deadline)
                || (counter != 0 && last_progress.elapsed() > Duration::from_secs(5))
            {
                return Err("新版本未响应".into());
            }
            if health_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        if counter == 0 {
            return Err("新版本未就绪".into());
        }
        transaction::commit(root, transaction)?;
        transaction::diagnostic(root, "committed");
        Ok(())
    })();
    if result.is_err() {
        // Only the child we spawned is terminated; no HID/AV/external process killing.
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

pub fn before_start() -> Result<bool, String> {
    let root = match root() {
        Ok(root) => root,
        Err(_) => return Ok(true),
    };
    let state: State = storage::read(&storage::state_path(&root))?;
    let Some(transaction) = state.transaction else {
        return Ok(true);
    };
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments.first().map(String::as_str) == Some("--ota-resume") {
        if arguments.len() != 3
            || arguments[1] != transaction.id
            || arguments[2] != transaction.token
        {
            return Err("更新启动凭据无效".into());
        }
        if transaction.phase == Phase::AwaitingHealth {
            return Ok(true);
        }
        return Err("更新启动状态无效".into());
    }
    if !matches!(
        transaction.phase,
        Phase::Preparing | Phase::Applying | Phase::AwaitingHealth
    ) {
        return Ok(true);
    }
    let lock = task_lock(&root, Duration::from_secs(65))?;
    let transaction = load_transaction(&root, &transaction.id)?;
    if transaction.phase == Phase::Preparing {
        transaction::cancel(&root, &transaction)?;
        return Ok(true);
    }
    drop(lock);
    if matches!(transaction.phase, Phase::Applying | Phase::AwaitingHealth) {
        launch_agent(root, transaction, "recover", &AtomicBool::new(false))?.release()?;
        return Ok(false);
    }
    Ok(true)
}

pub struct Health {
    root: std::path::PathBuf,
    transaction: Transaction,
    counter: u64,
}
impl Health {
    pub fn begin() -> Result<Option<Self>, String> {
        if std::env::args().nth(1).as_deref() != Some("--ota-resume") {
            return Ok(None);
        }
        let root = root()?;
        let id = std::env::args().nth(2).ok_or("更新启动凭据缺失")?;
        let transaction = load_transaction(&root, &id)?;
        let expected = transaction
            .manifest
            .payload
            .iter()
            .find(|p| p.path == "periph-in-one.exe")
            .ok_or("更新程序清单缺失")?;
        if package::hash_file(&root.join("periph-in-one.exe"))? != expected.sha256 {
            return Err("新版本程序校验失败".into());
        }
        if transaction.phase == Phase::Committed {
            return Ok(None);
        }
        Ok(Some(Self {
            root,
            transaction,
            counter: 0,
        }))
    }
    pub fn tick(&mut self) -> Result<bool, String> {
        self.counter += 1;
        let directory = storage::transaction_path(&self.root, &self.transaction.id)?;
        storage::write(
            &directory.join("health.json"),
            &Heartbeat {
                token: self.transaction.token.clone(),
                counter: self.counter,
            },
        )?;
        Ok(load_transaction(&self.root, &self.transaction.id)?.phase == Phase::Committed)
    }
    pub fn user_exit(&self) {
        if let Ok(directory) = storage::transaction_path(&self.root, &self.transaction.id) {
            let _ = storage::write(&directory.join("user-exit.json"), &self.transaction.token);
        }
    }
}

pub fn probe(candidate: &Path, manifest: &Manifest) -> Result<(), String> {
    let mut child = Command::new(candidate)
        .arg("--ota-probe")
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "新版本无法启动，已保留当前版本")?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if child
            .try_wait()
            .map_err(|_| "无法获取更新预检结果")?
            .is_some()
        {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("新版本启动预检超时".into());
        }
        thread::sleep(Duration::from_millis(30));
    }
    let output = child
        .wait_with_output()
        .map_err(|_| "无法获取更新预检结果")?;
    if !output.status.success() || output.stdout.len() > 4096 {
        return Err("新版本无法读取当前配置".into());
    }
    let result: Probe = serde_json::from_slice(&output.stdout).map_err(|_| "新版本预检结果无效")?;
    if result.version != manifest.version.to_string()
        || result.target != manifest.target
        || result.protocol != manifest.protocol
        || !(manifest.minimum_schema..=manifest.maximum_schema).contains(&result.schema)
    {
        return Err("新版本预检不兼容".into());
    }
    Ok(())
}

pub fn available_space(root: &Path) -> Result<u64, String> {
    let path: Vec<_> = root.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available = 0;
    if unsafe {
        windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            path.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err("无法确认可用空间".into());
    }
    Ok(available)
}

pub fn open_release(version: &str) -> Result<(), String> {
    let version = semver::Version::parse(version).map_err(|_| "版本信息无效")?;
    let url = super::wide(&format!(
        "https://github.com/FoLeaf/Periph-In-One/releases/tag/v{version}"
    ));
    let result = unsafe {
        windows_sys::Win32::UI::Shell::ShellExecuteW(
            std::ptr::null_mut(),
            super::wide("open").as_ptr(),
            url.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    };
    if result as isize <= 32 {
        return Err("无法打开发布页面，请稍后重试".into());
    }
    Ok(())
}
