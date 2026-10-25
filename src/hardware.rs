//! Application boundary for the reusable HAL. Legacy record keys and Chinese
//! presentation stay here; the HAL knows neither settings nor UI messages.
use crate::model::{self, Device, Reading};
use periph_hal::{
    BackendFailure, Connection, ErrorKind, IdentityKind, Snapshot,
    battery::{BatteryState, SampleOrigin},
};
use std::collections::HashMap;

#[derive(Debug)]
pub struct ScanReport {
    pub readings: Vec<Reading>,
    pub failures: Vec<BackendFailure>,
}

pub fn from_snapshot(snapshot: Snapshot) -> ScanReport {
    // Keep existing single-receiver record keys in this compatibility boundary.
    // The HAL itself always exposes the full, stable route including its slot.
    let mut receiver_counts = HashMap::<String, usize>::new();
    for device in &snapshot.devices {
        if let Some(location) = device
            .descriptor
            .identities
            .iter()
            .find(|identity| identity.kind == IdentityKind::Location)
        {
            *receiver_counts.entry(location.value.clone()).or_default() += 1;
        }
    }
    let readings = snapshot.devices.into_iter().filter_map(|device| {
        if device.connection == Connection::Disconnected { return None }
        let state = device.capabilities.get::<BatteryState>("main")?;
        let descriptor = &device.descriptor;
        let bluetooth = descriptor.links.iter().any(|link| link.transport == "bluetooth.classic");
        let kind = if bluetooth { "bt-headset" } else {
            descriptor.roles.first().and_then(|role| role.split_once('.').map(|(_, kind)| kind)).unwrap_or("unknown")
        };
        let battery = match state {
            BatteryState::Value(sample) if device.connection == Connection::Connected => Some(sample.level.get()),
            _ => None,
        };
        let no_battery = matches!(state, BatteryState::Unsupported);
        let message = if no_battery {
            "该型号不提供电量".into()
        } else if battery.is_some() {
            "已连接".into()
        } else if device.connection == Connection::Connected {
            "电量暂不可用".into()
        } else {
            match device.issue.as_ref().map(|error| error.kind) {
                Some(ErrorKind::NoPairedDevice) => "接收器上没有已配对的设备".into(),
                Some(ErrorKind::Timeout) | None => "设备未响应，请确认设备已开机".into(),
                _ => "读取失败，请重新连接设备后刷新".into(),
            }
        };
        let mut physical_id = descriptor.key.local.clone();
        if let Some(location) = descriptor.identities.iter().find(|identity| identity.kind == IdentityKind::Location)
            && receiver_counts.get(&location.value) == Some(&1) {
            physical_id = location.value.clone();
        }
        let identity = |kind| descriptor.identities.iter().find(|identity| identity.kind == kind).map(|identity| identity.value.clone()).unwrap_or_default();
        let identifiers = descriptor.identifiers.first();
        Some(Reading {
            physical_id,
            kind: kind.into(),
            hardware_name: descriptor.name.clone(),
            serial: identity(IdentityKind::Serial),
            uid: identity(IdentityKind::UnitId),
            vid: identifiers.map_or(0, |id| id.vendor),
            pid: identifiers.map_or(0, |id| id.product),
            battery,
            no_battery,
            connection: device.connection,
            system_cached: matches!(state, BatteryState::Value(sample) if sample.origin == SampleOrigin::SystemCache),
            message,
        })
    }).collect();
    ScanReport {
        readings,
        failures: snapshot.failures,
    }
}

/// An unavailable backend cannot authoritatively report its records offline.
/// This compatibility classifier is confined to the app's existing v1 keys.
pub fn merge_report(previous: &[Device], report: ScanReport, now: u64) -> Vec<Device> {
    let saved: Vec<_> = previous.iter().map(|device| device.saved.clone()).collect();
    let mut devices = model::merge_readings(&saved, report.readings, now);
    for device in &mut devices {
        let backend = if device.saved.physical_id.starts_with("bt:") {
            "windows.bluetooth.audio"
        } else {
            "windows.hid"
        };
        if !device.present
            && report
                .failures
                .iter()
                .any(|failure| failure.backend == backend)
        {
            device.connection = Connection::Unknown;
            device.fresh = false;
            device.message = "暂时无法读取设备，请稍后刷新".into();
            if let Some(old) = previous.iter().find(|old| old.saved.id == device.saved.id) {
                device.present = old.present;
            }
        }
    }
    devices
}
