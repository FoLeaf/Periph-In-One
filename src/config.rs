use crate::battery::SavedBattery;
use crate::model::SavedDevice;
use serde::{Deserialize, Serialize};
use std::{
    fs::OpenOptions,
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub width: f32,
    pub height: f32,
    pub devices: Vec<SavedDevice>,
    pub batteries: Vec<SavedBattery>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 2,
            width: 360.0,
            height: 640.0,
            devices: Vec::new(),
            batteries: Vec::new(),
        }
    }
}

impl Config {
    fn version(header: &serde_json::Value) -> io::Result<u64> {
        match header.get("version") {
            None => Ok(1),
            Some(value) => value
                .as_u64()
                .ok_or_else(|| io::Error::other("配置版本格式无效")),
        }
    }
    pub fn path() -> io::Result<PathBuf> {
        let exe = std::env::current_exe()?;
        Ok(exe
            .parent()
            .ok_or_else(|| io::Error::other("无法确定程序目录"))?
            .join("config/settings.json"))
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let header: serde_json::Value =
                    serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                let version = Self::version(&header)?;
                let mut config: Self = match version {
                    1 => crate::migration::version_one(&bytes).map_err(io::Error::other)?,
                    2 => serde_json::from_slice(&bytes).map_err(io::Error::other)?,
                    _ => return Err(io::Error::other("不支持的配置版本，请保留原文件")),
                };
                crate::migration::normalize_v2(&mut config);
                if !config.width.is_finite() || !config.height.is_finite() {
                    return Err(io::Error::other("窗口尺寸无效"));
                }
                config.width = config.width.clamp(320.0, 3840.0);
                config.height = config.height.clamp(340.0, 2160.0);
                if config.batteries.iter().any(|battery| battery.level > 100) {
                    return Err(io::Error::other("配置电量值无效"));
                }
                Ok(config)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if self.version != 2 {
            return Err(io::Error::other("无法保存不支持的配置版本"));
        }
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("配置目录无效"))?;
        std::fs::create_dir_all(parent)?;
        // Preserve exact V1 bytes before the first atomic replacement. Loading
        // and CLI discovery remain read-only; actual saves perform migration.
        let original = match std::fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        if let Some(original) = original {
            let header: serde_json::Value =
                serde_json::from_slice(&original).map_err(io::Error::other)?;
            let version = Self::version(&header)?;
            match version {
                1 => {
                    let backup = path.with_extension("json.v1.bak");
                    match OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&backup)
                    {
                        Ok(mut file) => {
                            file.write_all(&original)?;
                            file.sync_all()?;
                        }
                        Err(error)
                            if error.kind() == io::ErrorKind::AlreadyExists
                                && std::fs::read(&backup)? == original => {}
                        Err(error) => return Err(error),
                    }
                }
                2 => {}
                _ => return Err(io::Error::other("原配置版本未知，已保留原文件")),
            }
        }
        // Atomic replacement: interruption must not truncate existing remarks.
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(file.as_file_mut(), self).map_err(io::Error::other)?;
        file.write_all(b"\n")?;
        file.as_file_mut().sync_all()?;
        file.persist(path).map_err(|e| e.error)?;
        Ok(())
    }
}
