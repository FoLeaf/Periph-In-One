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
            target: env!("PERIPH_BUILD_TARGET").into(),
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
