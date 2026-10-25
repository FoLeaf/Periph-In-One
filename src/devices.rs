//! Application-facing compatibility facade. Hardware access lives in periph-hal.
use crate::{
    hardware::{self, ScanReport},
    model::Reading,
};
use periph_hal::Hardware;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct UsbIdentifiers {
    pub hardware_name: String,
    pub vid: u16,
    pub pid: u16,
}

pub fn enumerate_identifiers() -> Result<Vec<UsbIdentifiers>, String> {
    let inventory = Hardware::system()
        .map_err(|_| "当前系统没有可用的设备后端".to_string())?
        .inventory();
    if !inventory.failures.is_empty() {
        return Err("暂时无法读取完整设备列表，请稍后重试".into());
    }
    Ok(inventory
        .devices
        .into_iter()
        .map(|descriptor| {
            let id = descriptor.identifiers.first();
            UsbIdentifiers {
                hardware_name: descriptor.name,
                vid: id.map_or(0, |id| id.vendor),
                pid: id.map_or(0, |id| id.product),
            }
        })
        .collect())
}

pub fn scan_report() -> Result<ScanReport, String> {
    let snapshot = Hardware::system()
        .map_err(|_| "当前系统没有可用的设备后端".to_string())?
        .scan();
    Ok(hardware::from_snapshot(snapshot))
}

/// Legacy callers have no way to handle partial coverage, so fail explicitly.
/// The application uses scan_report and preserves failed-source history.
pub fn scan() -> Result<Vec<Reading>, String> {
    let report = scan_report()?;
    if !report.failures.is_empty() {
        return Err("暂时无法读取完整设备状态，请稍后刷新".into());
    }
    Ok(report.readings)
}
