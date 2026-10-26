//! Application update policy and verified packages; HAL has no update dependencies.
pub mod package;

pub const APP_ID: &str = "io.github.FoLeaf.PeriphInOne";
pub const TARGET: &str = "x86_64-pc-windows-msvc";
pub const PROTOCOL: u32 = 1;
pub const UPDATER_VERSION: u32 = 1;
pub const MAX_PACKAGE: u64 = 64 * 1024 * 1024;
pub const MAX_EXPANDED: u64 = 192 * 1024 * 1024;

pub fn trusted_keys() -> Result<Vec<[u8; 32]>, String> {
    let value = option_env!("PERIPH_OTA_PUBLIC_KEYS").unwrap_or("");
    let keys: Result<Vec<_>, _> = value
        .split(',')
        .filter(|v| !v.trim().is_empty())
        .map(|key| {
            hex::decode(key.trim())
                .map_err(|_| "更新公钥无效".to_string())?
                .try_into()
                .map_err(|_| "更新公钥无效".to_string())
        })
        .collect();
    let keys = keys?;
    if keys.is_empty() {
        return Err("此构建尚未配置更新签名".into());
    }
    Ok(keys)
}
