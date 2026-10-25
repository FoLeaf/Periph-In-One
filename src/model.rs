use periph_hal::Connection;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedDevice {
    pub id: String,
    pub physical_id: String,
    pub kind: String,
    pub hardware_name: String,
    pub serial: String,
    pub uid: String,
    pub vid: u16,
    pub pid: u16,
    pub alias: String,
    pub battery: Option<u8>,
    pub last_success: Option<u64>,
    pub identity_conflict: bool,
}

#[derive(Clone, Debug)]
pub struct Reading {
    pub physical_id: String,
    pub kind: String,
    pub hardware_name: String,
    pub serial: String,
    pub uid: String,
    pub vid: u16,
    pub pid: u16,
    pub battery: Option<u8>,
    /// A verified model provides no battery reading. Connection is independent
    /// and this flag never makes a historical battery value fresh.
    pub no_battery: bool,
    pub connection: Connection,
    pub system_cached: bool,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct Device {
    pub saved: SavedDevice,
    pub present: bool,
    pub fresh: bool,
    pub connection: Connection,
    pub no_battery: bool,
    pub system_cached: bool,
    pub message: String,
}

pub fn serial_key(vid: u16, pid: u16, serial: &str) -> String {
    // JSON tuple avoids ambiguous concatenations, and does not normalize case.
    format!("sn:{}", serde_json::to_string(&(vid, pid, serial)).unwrap())
}

/// The protocol-reported per-unit identity. The pid is deliberately absent:
/// vendors reuse one uid across firmware personalities whose pid differs.
pub fn uid_key(vid: u16, uid: &str) -> String {
    format!("uid:{}", serde_json::to_string(&(vid, uid)).unwrap())
}

pub fn usb_id_label(vid: u16, pid: u16) -> String {
    format!("VID: 0x{vid:04X} · PID: 0x{pid:04X}")
}

fn name_used_as_serial(serial: &str, name: &str) -> bool {
    !serial.is_empty() && serial.trim().eq_ignore_ascii_case(name.trim())
}

fn saved_physical_id(saved: &SavedDevice) -> Option<String> {
    if !saved.physical_id.is_empty() {
        return Some(saved.physical_id.clone());
    }
    // Migrate v0.1.2 local:[vid,pid,"pid-nanoseconds:epoch:ContainerId"].
    // Read the tuple rather than matching names/VID/PID alone; separate
    // receivers of the same model must never collapse into one record.
    let (vid, pid, physical): (u16, u16, String) =
        serde_json::from_str(saved.id.strip_prefix("local:")?).ok()?;
    if (vid, pid) != (saved.vid, saved.pid) {
        return None;
    }
    if let Some((session, tail)) = physical.split_once(':')
        && session.split_once('-').is_some_and(|(pid, time)| {
            !pid.is_empty()
                && !time.is_empty()
                && pid.bytes().all(|c| c.is_ascii_digit())
                && time.bytes().all(|c| c.is_ascii_digit())
        })
        && let Some((epoch, identity)) = tail.split_once(':')
        && !epoch.is_empty()
        && epoch.bytes().all(|c| c.is_ascii_digit())
    {
        return Some(identity.into());
    }
    Some(physical)
}

fn reconcile_record(devices: &mut Vec<Device>, reading: &Reading, id: &str) -> usize {
    let candidates: Vec<_> = devices
        .iter()
        .enumerate()
        .filter_map(|(i, device)| {
            let saved = &device.saved;
            (saved.id == id
                || (!device.present
                    && saved.vid == reading.vid
                    && saved_physical_id(saved).as_deref() == Some(reading.physical_id.as_str())
                    // One port holds one device at a time, so the port anchors
                    // cross-personality merges; uid and serial must never
                    // contradict it.
                    && (saved.uid.is_empty()
                        || reading.uid.is_empty()
                        || saved.uid == reading.uid)
                    && (saved.serial.is_empty()
                        || reading.serial.is_empty()
                        || saved.serial == reading.serial
                        || name_used_as_serial(&saved.serial, &saved.hardware_name))))
            .then_some(i)
        })
        .collect();
    let Some(index) = candidates
        .iter()
        .copied()
        .max_by_key(|&i| (devices[i].saved.id == id, devices[i].saved.last_success))
    else {
        devices.push(Device {
            saved: SavedDevice {
                id: id.into(),
                ..Default::default()
            },
            present: false,
            fresh: false,
            connection: Connection::Disconnected,
            no_battery: false,
            system_cached: false,
            message: String::new(),
        });
        return devices.len() - 1;
    };
    let mut merged = devices[index].clone();
    // A local id is the weakest identity: a topology-only reading must never
    // downgrade a uid- or serial-keyed record.
    if merged.saved.id.starts_with("local:") || !id.starts_with("local:") {
        merged.saved.id = id.into();
    }
    if merged.saved.alias.is_empty()
        && let Some(source) = candidates
            .iter()
            .copied()
            .filter(|&i| !devices[i].saved.alias.is_empty())
            .max_by_key(|&i| devices[i].saved.last_success)
    {
        merged.saved.alias = devices[source].saved.alias.clone();
    }
    let mut remove = Vec::new();
    for i in candidates {
        let saved = &devices[i].saved;
        // Preserve a distinct user remark for explicit review/rebinding.
        if !saved.alias.is_empty() && saved.alias != merged.saved.alias {
            continue;
        }
        if saved.last_success > merged.saved.last_success {
            merged.saved.last_success = saved.last_success;
            merged.saved.battery = saved.battery;
        }
        remove.push(i);
    }
    for i in remove.into_iter().rev() {
        devices.remove(i);
    }
    devices.push(merged);
    devices.len() - 1
}

pub fn merge_readings(saved: &[SavedDevice], readings: Vec<Reading>, now: u64) -> Vec<Device> {
    let mut devices: Vec<Device> = saved
        .iter()
        .cloned()
        .map(|saved| Device {
            saved,
            present: false,
            fresh: false,
            connection: Connection::Disconnected,
            no_battery: false,
            system_cached: false,
            message: "设备已离线".into(),
        })
        .collect();
    let mut counts = HashMap::<String, usize>::new();
    let mut physical = HashSet::new();
    let readings: Vec<_> = readings
        .into_iter()
        .filter(|r| physical.insert((r.vid, r.pid, r.physical_id.clone())))
        .collect();
    for reading in &readings {
        if !reading.serial.is_empty() {
            *counts
                .entry(serial_key(reading.vid, reading.pid, &reading.serial))
                .or_default() += 1;
        }
    }
    // A serial collision is remembered: a later scan with only one receiver
    // must not silently attach a historical remark to the wrong receiver.
    for device in &mut devices {
        if name_used_as_serial(&device.saved.serial, &device.saved.hardware_name)
            || counts
                .get(&serial_key(
                    device.saved.vid,
                    device.saved.pid,
                    &device.saved.serial,
                ))
                .is_some_and(|n| *n > 1)
        {
            device.saved.identity_conflict = true;
        }
    }
    for reading in readings {
        let serial_id = serial_key(reading.vid, reading.pid, &reading.serial);
        let conflict = name_used_as_serial(&reading.serial, &reading.hardware_name)
            || counts.get(&serial_id).is_some_and(|n| *n > 1)
            || devices.iter().any(|d| {
                d.saved.vid == reading.vid
                    && d.saved.pid == reading.pid
                    && d.saved.serial == reading.serial
                    && d.saved.identity_conflict
            });
        let retained_serial = if reading.serial.is_empty() {
            devices
                .iter()
                .find(|d| {
                    !d.present
                        && !d.saved.identity_conflict
                        && !d.saved.serial.is_empty()
                        && (d.saved.vid, d.saved.pid) == (reading.vid, reading.pid)
                        && saved_physical_id(&d.saved).as_deref() == Some(&reading.physical_id)
                })
                .map(|d| d.saved.serial.clone())
        } else {
            None
        };
        let id = if !reading.uid.is_empty() {
            uid_key(reading.vid, &reading.uid)
        } else if let Some(serial) = &retained_serial {
            serial_key(reading.vid, reading.pid, serial)
        } else if reading.serial.is_empty() || conflict {
            format!(
                "local:{}",
                serde_json::to_string(&(reading.vid, &reading.physical_id)).unwrap()
            )
        } else {
            serial_id
        };
        let index = reconcile_record(&mut devices, &reading, &id);
        let device = &mut devices[index];
        device.saved.physical_id = reading.physical_id;
        device.saved.kind = reading.kind;
        if !reading.hardware_name.is_empty() {
            device.saved.hardware_name = reading.hardware_name;
        }
        device.saved.serial = retained_serial.unwrap_or(reading.serial);
        if !reading.uid.is_empty() {
            device.saved.uid = reading.uid.clone();
        }
        device.saved.vid = reading.vid;
        device.saved.pid = reading.pid;
        device.saved.identity_conflict |= conflict;
        device.present = true;
        device.connection = reading.connection;
        device.no_battery = reading.no_battery;
        device.system_cached = reading.system_cached;
        device.fresh = reading.battery.is_some_and(|level| level <= 100);
        device.message = reading.message;
        if let Some(battery) = reading.battery.filter(|b| *b <= 100) {
            device.saved.battery = Some(battery);
            device.saved.last_success = Some(now);
        }
    }
    devices
}

pub fn remove_offline(devices: &mut Vec<Device>, id: &str) -> Result<(), &'static str> {
    let index = devices
        .iter()
        .position(|d| d.saved.id == id)
        .ok_or("设备记录已不存在")?;
    if devices[index].present {
        return Err("设备已连接，无法删除");
    }
    devices.remove(index);
    Ok(())
}

/// Explicitly move an orphaned remark to a receiver selected by the user.
pub fn rebind(devices: &mut [Device], from: &str, to: &str) -> Result<(), &'static str> {
    if from == to {
        return Err("请选择不同的设备记录");
    }
    let source = devices
        .iter()
        .position(|d| d.saved.id == from && !d.present)
        .ok_or("原记录必须是离线设备")?;
    let target = devices
        .iter()
        .position(|d| d.saved.id == to && d.present)
        .ok_or("目标设备必须在线")?;
    if devices[source].saved.alias.is_empty() {
        return Err("原记录没有备注");
    }
    if !devices[target].saved.alias.is_empty() {
        return Err("请先清空目标设备的备注");
    }
    devices[target].saved.alias = std::mem::take(&mut devices[source].saved.alias);
    Ok(())
}
