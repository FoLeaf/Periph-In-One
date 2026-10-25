//! Application hardware entry points. No legacy reading or platform classifier.
use crate::hardware::{self, ScanReport};
use periph_hal::{Hardware, HardwareIdentifier, Link};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct InventoryEntry {
    pub name: String,
    pub roles: Vec<String>,
    pub identifiers: Vec<HardwareIdentifier>,
    pub links: Vec<Link>,
}

pub fn enumerate_identifiers() -> Result<Vec<InventoryEntry>, String> {
    let inventory = Hardware::system()
        .map_err(|_| "当前系统没有可用的设备后端".to_string())?
        .inventory();
    if !inventory.failures.is_empty() {
        return Err("暂时无法读取完整设备列表，请稍后重试".into());
    }
    Ok(inventory
        .devices
        .into_iter()
        .map(|descriptor| InventoryEntry {
            name: descriptor.name,
            roles: descriptor.roles,
            identifiers: descriptor.identifiers,
            links: descriptor.links,
        })
        .collect())
}

pub fn refresh(hardware: &mut Hardware) -> ScanReport {
    hardware::from_snapshot(hardware.discover())
}

pub fn scan() -> Result<ScanReport, String> {
    let mut hardware = Hardware::system().map_err(|_| "当前系统没有可用的设备后端".to_string())?;
    Ok(refresh(&mut hardware))
}
