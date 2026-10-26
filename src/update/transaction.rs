//! Durable checkpoints and deterministic recovery, using the library's direct
//! replacement. Every mutating operation requires the installation task lock.
use super::{
    package::{self, Payload},
    storage::{self, Phase, State, Transaction},
};
use std::{
    fs,
    io::Write,
    path::{Component, Path},
};

pub fn plain(root: &Path, path: &Path) -> Result<(), String> {
    let relative = path.strip_prefix(root).map_err(|_| "更新路径越界")?;
    let mut current = root.to_owned();
    for part in relative.components() {
        let Component::Normal(part) = part else {
            return Err("更新路径无效".into());
        };
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if metadata.file_attributes() & 0x400 != 0 {
                        return Err("更新目录包含链接，请换到普通目录".into());
                    }
                }
                if metadata.file_type().is_symlink() {
                    return Err("更新目录包含链接".into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("无法检查更新路径".into()),
        }
    }
    Ok(())
}

pub fn copy(root: &Path, source: &Path, destination: &Path) -> Result<(), String> {
    plain(root, source)?;
    plain(root, destination)?;
    let parent = destination.parent().ok_or("更新目录无效")?;
    fs::create_dir_all(parent).map_err(|_| "更新目录不可写")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|_| "更新目录不可写")?;
    std::io::copy(
        &mut fs::File::open(source).map_err(|_| "无法读取更新文件")?,
        temporary.as_file_mut(),
    )
    .map_err(|_| "无法复制更新文件")?;
    temporary
        .as_file_mut()
        .sync_all()
        .map_err(|_| "无法保存更新文件")?;
    let (_, temporary) = temporary.keep().map_err(|_| "无法保存更新文件")?;
    let result = self_update::Move::from_source(&temporary)
        .to_dest(destination)
        .map_err(|_| "文件被占用或目录不可写，已保留当前版本".to_string());
    let _ = fs::remove_file(temporary);
    result
}

fn collect(root: &Path, directory: &Path, entries: &mut Vec<Payload>) -> Result<(), String> {
    plain(root, directory)?;
    if !directory.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(directory).map_err(|_| "无法读取许可目录")? {
        let path = entry.map_err(|_| "无法读取许可目录")?.path();
        plain(root, &path)?;
        let metadata = fs::symlink_metadata(&path).map_err(|_| "无法读取许可文件")?;
        if metadata.is_dir() {
            collect(root, &path, entries)?;
        } else {
            let path_name = path
                .strip_prefix(root)
                .map_err(|_| "更新路径越界")?
                .to_string_lossy()
                .replace('\\', "/");
            if !package::payload_path(&path_name) || !metadata.is_file() {
                return Err("许可目录包含不支持的文件".into());
            }
            entries.push(Payload {
                path: path_name,
                size: metadata.len(),
                sha256: package::hash_file(&path)?,
            });
            if entries.len() > 4096 {
                return Err("许可文件过多，更新已延后".into());
            }
        }
    }
    Ok(())
}

fn snapshot_hash(root: &Path) -> Result<Option<String>, String> {
    let path = root.join("config/settings.json");
    plain(root, &path)?;
    if path.exists() {
        Ok(Some(package::hash_file(&path)?))
    } else {
        Ok(None)
    }
}

pub fn prepare(root: &Path, expected: &Transaction) -> Result<Transaction, String> {
    let mut state: State = storage::read(&storage::state_path(root))?;
    let t = state
        .transaction
        .as_mut()
        .filter(|t| t.id == expected.id && t.token == expected.token && t.phase == Phase::Verified)
        .ok_or("更新任务已变化")?;
    let directory = storage::transaction_path(root, &t.id)?;
    plain(root, &directory)?;
    t.old_payload.clear();
    collect(root, &root.join("licenses"), &mut t.old_payload)?;
    t.old_sha256 = Some(package::hash_file(&root.join("periph-in-one.exe"))?);
    t.config_sha256 = snapshot_hash(root)?;
    let backup = directory.join("backup");
    copy(
        root,
        &root.join("periph-in-one.exe"),
        &backup.join("periph-in-one.exe"),
    )?;
    for entry in &t.old_payload {
        copy(root, &root.join(&entry.path), &backup.join(&entry.path))?;
    }
    if t.config_sha256.is_some() {
        copy(
            root,
            &root.join("config/settings.json"),
            &backup.join("config/settings.json"),
        )?;
    }
    copy(
        root,
        &root.join("periph-in-one.exe"),
        &directory.join("agent.exe"),
    )?;
    t.phase = Phase::Preparing;
    let transaction = t.clone();
    storage::write(&storage::state_path(root), &state)?;
    Ok(transaction)
}

pub fn verify_backup(root: &Path, transaction: &Transaction) -> Result<(), String> {
    let directory = storage::transaction_path(root, &transaction.id)?.join("backup");
    plain(root, &directory)?;
    let old = transaction
        .old_sha256
        .as_ref()
        .ok_or("更新缺少旧版本检查点")?;
    if package::hash_file(&directory.join("periph-in-one.exe"))? != *old {
        return Err("旧版本备份校验失败".into());
    }
    for entry in &transaction.old_payload {
        if !package::payload_path(&entry.path) || entry.path == "periph-in-one.exe" {
            return Err("旧版本许可检查点无效".into());
        }
        plain(root, &directory.join(&entry.path))?;
        if package::hash_file(&directory.join(&entry.path))? != entry.sha256 {
            return Err("旧版本许可备份校验失败".into());
        }
    }
    if let Some(hash) = &transaction.config_sha256
        && package::hash_file(&directory.join("config/settings.json"))? != *hash
    {
        return Err("配置检查点校验失败".into());
    }
    Ok(())
}

fn set_phase(root: &Path, transaction: &Transaction, phase: Phase) -> Result<(), String> {
    let mut state: State = storage::read(&storage::state_path(root))?;
    let current = state
        .transaction
        .as_mut()
        .filter(|t| t.id == transaction.id && t.token == transaction.token)
        .ok_or("更新任务已变化")?;
    let allowed = match phase {
        Phase::Verified => matches!(current.phase, Phase::Preparing | Phase::Verified),
        Phase::Applying => current.phase == Phase::Preparing,
        Phase::AwaitingHealth => current.phase == Phase::Applying,
        Phase::Committed => current.phase == Phase::AwaitingHealth,
        Phase::RolledBack => matches!(
            current.phase,
            Phase::Preparing | Phase::Applying | Phase::AwaitingHealth
        ),
        Phase::Preparing => false,
    };
    if !allowed {
        return Err("更新事务状态无效".into());
    }
    current.phase = phase;
    if phase == Phase::Committed {
        state.recovery_backup = None;
    }
    if phase == Phase::RolledBack && !state.rejected.contains(&transaction.manifest.version) {
        state.rejected.push(transaction.manifest.version.clone());
    }
    storage::write(&storage::state_path(root), &state)
}

pub fn cancel(root: &Path, transaction: &Transaction) -> Result<(), String> {
    set_phase(root, transaction, Phase::Verified)
}

pub fn apply(root: &Path, transaction: &Transaction) -> Result<(), String> {
    verify_backup(root, transaction)?;
    if Some(package::hash_file(&root.join("periph-in-one.exe"))?) != transaction.old_sha256 {
        return Err("程序文件已变化，更新已延后".into());
    }
    if snapshot_hash(root)? != transaction.config_sha256 {
        return Err("配置已变化，更新已延后".into());
    }
    let directory = storage::transaction_path(root, &transaction.id)?;
    for entry in &transaction.manifest.payload {
        plain(root, &directory.join("staged").join(&entry.path))?;
        if package::hash_file(&directory.join("staged").join(&entry.path))? != entry.sha256 {
            return Err("暂存文件校验失败".into());
        }
    }
    set_phase(root, transaction, Phase::Applying)?;
    for entry in transaction
        .manifest
        .payload
        .iter()
        .filter(|p| p.path != "periph-in-one.exe")
    {
        copy(
            root,
            &directory.join("staged").join(&entry.path),
            &root.join(&entry.path),
        )?;
    }
    // The canonical EXE is never removed, renamed away, or copied over in place.
    // A separately flushed temporary file is renamed over it by self_update.
    copy(
        root,
        &directory.join("staged/periph-in-one.exe"),
        &root.join("periph-in-one.exe"),
    )?;
    set_phase(root, transaction, Phase::AwaitingHealth)
}

pub fn commit(root: &Path, transaction: &Transaction) -> Result<(), String> {
    if snapshot_hash(root)? != transaction.config_sha256 {
        return Err("配置在更新期间发生变化，已保留原文件与检查点".into());
    }
    for entry in &transaction.manifest.payload {
        plain(root, &root.join(&entry.path))?;
        if package::hash_file(&root.join(&entry.path))? != entry.sha256 {
            return Err("安装后的文件校验失败".into());
        }
    }
    set_phase(root, transaction, Phase::Committed)?;
    if let Ok(state) = storage::read(&storage::state_path(root)) {
        cleanup(root, &state);
    }
    Ok(())
}

pub fn rollback(root: &Path, transaction: &Transaction) -> Result<(), String> {
    let state: State = storage::read(&storage::state_path(root))?;
    if !state.transaction.as_ref().is_some_and(|t| {
        t.id == transaction.id
            && t.token == transaction.token
            && matches!(
                t.phase,
                Phase::Preparing | Phase::Applying | Phase::AwaitingHealth
            )
    }) {
        return Err("更新事务已结束，不能回退".into());
    }
    verify_backup(root, transaction)?;
    let directory = storage::transaction_path(root, &transaction.id)?;
    let exe = root.join("periph-in-one.exe");
    plain(root, &exe)?;
    let installed = package::hash_file(&exe)?;
    let new = transaction
        .manifest
        .payload
        .iter()
        .find(|p| p.path == "periph-in-one.exe")
        .ok_or("更新清单缺少程序")?;
    let old = transaction.old_sha256.as_ref().ok_or("旧版本检查点缺失")?;
    if installed != *old && installed != new.sha256 {
        return Err("程序文件已变化，请保留更新检查点".into());
    }
    for entry in &transaction.old_payload {
        copy(
            root,
            &directory.join("backup").join(&entry.path),
            &root.join(&entry.path),
        )?;
    }
    for entry in transaction.manifest.payload.iter().filter(|p| {
        p.path.starts_with("licenses/")
            && !transaction
                .old_payload
                .iter()
                .any(|old| old.path.eq_ignore_ascii_case(&p.path))
    }) {
        let path = root.join(&entry.path);
        plain(root, &path)?;
        if path.exists() && package::hash_file(&path)? == entry.sha256 {
            fs::remove_file(path).map_err(|_| "无法恢复许可文件")?;
        }
    }
    if installed != *old {
        copy(root, &directory.join("backup/periph-in-one.exe"), &exe)?;
    }
    // No update-era save is permitted; if another process edited the config,
    // preserve both copies rather than overwriting user changes on rollback.
    set_phase(root, transaction, Phase::RolledBack)
}

pub fn diagnostic(root: &Path, phase: &str) {
    let path = root.join(".ota/diagnostics.log");
    if plain(root, &path).is_err() {
        return;
    }
    if fs::metadata(&path).is_ok_and(|m| m.len() >= 64 * 1024) {
        let previous = path.with_extension("previous.log");
        let _ = fs::remove_file(&previous);
        let _ = fs::rename(&path, previous);
    }
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        // Fixed categories only. No device facts, paths, config, or server body.
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let _ = writeln!(file, "{timestamp} {phase}");
    }
}

/// Remove only tagged cache directories, preserving the active transaction and
/// the last recovery checkpoint until its successor commits.
pub fn cleanup(root: &Path, state: &State) {
    let ota = root.join(".ota");
    if plain(root, &ota).is_err() {
        return;
    }
    let Ok(entries) = fs::read_dir(&ota) else {
        return;
    };
    for entry in entries.flatten() {
        let id = entry.file_name().to_string_lossy().into_owned();
        if state.transaction.as_ref().is_some_and(|t| t.id == id)
            || state.recovery_backup.as_deref() == Some(&id)
        {
            continue;
        }
        let Ok(directory) = storage::transaction_path(root, &id) else {
            continue;
        };
        if plain(root, &directory).is_err()
            || storage::read::<String>(&directory.join("owner.json"))
                .ok()
                .as_deref()
                != Some(super::APP_ID)
        {
            continue;
        }
        fn safe_tree(root: &Path, directory: &Path) -> bool {
            let Ok(entries) = fs::read_dir(directory) else {
                return false;
            };
            for entry in entries {
                let Ok(entry) = entry else {
                    return false;
                };
                let path = entry.path();
                if plain(root, &path).is_err() {
                    return false;
                }
                if path.is_dir() && !safe_tree(root, &path) {
                    return false;
                }
            }
            true
        }
        if safe_tree(root, &directory) {
            let _ = fs::remove_dir_all(directory);
        }
    }
}
