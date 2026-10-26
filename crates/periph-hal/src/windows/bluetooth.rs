//! Bluetooth classic audio devices (BTHENUM). Windows records the battery a
//! headset reports over HFP/AVRCP on the Hands-Free AG service node — a
//! sibling of the device node, keyed by the shared MAC address — so the
//! enumeration collects device nodes and battery-carrying children in one
//! pass and joins them. There is no HID and no reliable serial, so the MAC
//! address is the stable physical identity. Only audio/video major-class
//! devices are listed.

use crate::{
    Capabilities, Connection, Device, DeviceDescriptor, DeviceFacts, DeviceKey, EndpointKey,
    ErrorKind, HardwareError, HardwareIdentifier, Identity, IdentityKind, Link,
    battery::{Battery, BatteryReading, BatteryState, Percent, Sample, SampleOrigin},
    windows::wide,
};
use std::time::SystemTime;
use windows_sys::Win32::Devices::{DeviceAndDriverInstallation::*, Properties::*};
use windows_sys::Win32::Foundation::{DEVPROPKEY, ERROR_NO_MORE_ITEMS, GetLastError};
use windows_sys::core::GUID;

/// DEVPKEY_Bluetooth_* (bthdef.h): {2BD67D8B-8BEB-48D5-87E0-6CDA3428040A}.
const BLUETOOTH_PROPKEY_GUID: GUID = GUID::from_u128(0x2bd67d8b_8beb_48d5_87e0_6cda3428040a);
fn bluetooth_propkey(pid: u32) -> DEVPROPKEY {
    DEVPROPKEY {
        fmtid: BLUETOOTH_PROPKEY_GUID,
        pid,
    }
}

/// Battery a headset reported over HFP/AVRCP, as recorded by Windows on the
/// Hands-Free AG service node: {104EA319-6EE2-4701-BD47-8DDBF425BBE5},
/// property 2, a single byte holding the percentage.
const BLUETOOTH_BATTERY_KEY: DEVPROPKEY = DEVPROPKEY {
    fmtid: GUID::from_u128(0x104ea319_6ee2_4701_bd47_8ddbf425bbe5),
    pid: 2,
};

/// Live connection state on the device node: {83DA6326-97A6-4088-9453-
/// A1923F573B29}, property 15. BTHENUM nodes survive a disconnect — including
/// the Hands-Free node with its stale battery byte — so this flag is the only
/// reliable "actually connected" signal.
const BLUETOOTH_ISCONNECTED_KEY: DEVPROPKEY = DEVPROPKEY {
    fmtid: GUID::from_u128(0x83da6326_97a6_4088_9453_a1923f573b29),
    pid: 15,
};

unsafe fn device_property(
    set: isize,
    data: &SP_DEVINFO_DATA,
    key: &DEVPROPKEY,
    expected: u32,
) -> Option<Vec<u8>> {
    let mut kind = 0;
    let mut needed = 0;
    unsafe {
        if SetupDiGetDevicePropertyW(
            set,
            data,
            key,
            &mut kind,
            std::ptr::null_mut(),
            0,
            &mut needed,
            0,
        ) == 0
            && needed == 0
        {
            return None;
        }
        let mut buffer = vec![0u8; needed as usize];
        let mut written = 0;
        if SetupDiGetDevicePropertyW(
            set,
            data,
            key,
            &mut kind,
            buffer.as_mut_ptr(),
            needed,
            &mut written,
            0,
        ) == 0
        {
            return None;
        }
        if kind != expected || written as usize > buffer.len() {
            return None;
        }
        buffer.truncate(written as usize);
        Some(buffer)
    }
}

fn property_string(set: isize, data: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> String {
    let bytes = unsafe { device_property(set, data, key, DEVPROP_TYPE_STRING) }.unwrap_or_default();
    let utf16: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    let end = utf16.iter().position(|&c| c == 0).unwrap_or(utf16.len());
    String::from_utf16_lossy(&utf16[..end]).trim().to_string()
}

fn property_u16(set: isize, data: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> Option<u16> {
    let bytes = unsafe { device_property(set, data, key, DEVPROP_TYPE_UINT16) }.unwrap_or_default();
    bytes.try_into().ok().map(u16::from_le_bytes)
}

fn property_u32(set: isize, data: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> Option<u32> {
    let bytes = unsafe { device_property(set, data, key, DEVPROP_TYPE_UINT32) }.unwrap_or_default();
    bytes.try_into().ok().map(u32::from_le_bytes)
}

fn connection_value(bytes: Option<&[u8]>) -> Connection {
    match bytes {
        Some([0]) => Connection::Disconnected,
        Some([0xff]) => Connection::Connected,
        _ => Connection::Unknown,
    }
}

fn battery_value(bytes: &[u8]) -> Option<u8> {
    match bytes {
        [level @ 0..=100] => Some(*level),
        _ => None,
    }
}

fn collect(
    battery_for: Option<&str>,
) -> Result<Vec<(DeviceDescriptor, Connection, Option<u8>)>, HardwareError> {
    /// A remote device node ("BTHENUM\DEV_…") or a battery-carrying service
    /// child of one, both identified by the shared Bluetooth MAC address.
    struct BtNode {
        address: String,
        is_device: bool,
        name: String,
        vid: Option<u16>,
        pid: Option<u16>,
        id_namespace: String,
        audio: bool,
        connection: Connection,
        battery: Option<u8>,
    }
    let mut nodes: Vec<BtNode> = Vec::new();
    unsafe {
        // Enumerating the BTHENUM bus instead of the Bluetooth device class:
        // the battery-carrying Hands-Free node belongs to a different device
        // class than the device node it belongs to. A NULL class requires
        // DIGCF_ALLCLASSES.
        let enumerator = wide("BTHENUM");
        let set = SetupDiGetClassDevsW(
            std::ptr::null(),
            enumerator.as_ptr(),
            std::ptr::null_mut(),
            DIGCF_PRESENT | DIGCF_ALLCLASSES,
        );
        if set == -1 {
            return Err(HardwareError::new(
                ErrorKind::Transport,
                "Bluetooth enumeration failed",
            ));
        }
        let mut index = 0u32;
        loop {
            let mut data = SP_DEVINFO_DATA {
                cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
                ..std::mem::zeroed()
            };
            if SetupDiEnumDeviceInfo(set, index, &mut data) == 0 {
                let error = GetLastError();
                if error != ERROR_NO_MORE_ITEMS {
                    SetupDiDestroyDeviceInfoList(set);
                    return Err(HardwareError::new(
                        ErrorKind::Transport,
                        format!("Bluetooth enumeration error {error}"),
                    ));
                }
                break;
            }
            index += 1;
            let instance = property_string(set, &data, &DEVPKEY_Device_InstanceId);
            // The MAC is the twelve-hex-digit token in the instance id; the
            // device node keeps it right after "DEV_", service children carry
            // it in their parent path segment.
            let Some(address) = instance
                .split(['\\', '&', '_'])
                .find(|token| token.len() == 12 && token.bytes().all(|b| b.is_ascii_hexdigit()))
                .map(str::to_ascii_uppercase)
            else {
                continue;
            };
            let is_device = instance.starts_with("BTHENUM\\DEV_");
            let battery = if battery_for.is_some_and(|wanted| wanted == format!("bt:{address}")) {
                device_property(set, &data, &BLUETOOTH_BATTERY_KEY, DEVPROP_TYPE_BYTE)
                    .and_then(|bytes| battery_value(&bytes))
            } else {
                None
            };
            match nodes
                .iter_mut()
                .find(|node| node.address == address && node.is_device == is_device)
            {
                Some(node) if !node.is_device => {
                    node.battery = node.battery.or(battery);
                }
                _ => {
                    // Bluetooth class of device, major class in bits 8..13:
                    // 4 = audio/video. Service children inherit the parent's
                    // class through the same property.
                    let class = property_u32(set, &data, &bluetooth_propkey(10))
                        .or_else(|| property_u32(set, &data, &bluetooth_propkey(4)))
                        .unwrap_or(0);
                    let name = property_string(set, &data, &DEVPKEY_Device_FriendlyName);
                    // Missing state is unknown; retained devnodes and cached
                    // battery values cannot establish a live connection.
                    let connection_bytes = device_property(
                        set,
                        &data,
                        &BLUETOOTH_ISCONNECTED_KEY,
                        DEVPROP_TYPE_BOOLEAN,
                    );
                    let connection = connection_value(connection_bytes.as_deref());
                    let id_namespace =
                        match device_property(set, &data, &bluetooth_propkey(6), DEVPROP_TYPE_BYTE)
                            .and_then(|bytes| bytes.first().copied())
                        {
                            Some(2) => "usb",
                            Some(1) => "bluetooth-sig",
                            _ => "bluetooth.pnp",
                        }
                        .into();
                    nodes.push(BtNode {
                        address,
                        is_device,
                        name,
                        vid: property_u16(set, &data, &bluetooth_propkey(7)),
                        pid: property_u16(set, &data, &bluetooth_propkey(8)),
                        audio: (class >> 8) & 0x1f == 0x04,
                        connection,
                        id_namespace,
                        battery,
                    });
                }
            }
        }
        SetupDiDestroyDeviceInfoList(set);
    }
    nodes
        .iter()
        .filter(|node| node.is_device && node.audio)
        .map(|device| {
            let battery = nodes
                .iter()
                .filter(|node| !node.is_device && node.address == device.address)
                .find_map(|node| node.battery)
                .or(device.battery);
            Ok((
                DeviceDescriptor {
                    key: DeviceKey {
                        backend: "windows.bluetooth.audio".into(),
                        local: format!("bt:{}", device.address),
                    },
                    endpoint: EndpointKey {
                        backend: "windows.bluetooth.audio".into(),
                        local: format!("bt:{}", device.address),
                    },
                    name: device.name.clone(),
                    roles: vec!["audio.headset".into()],
                    identifiers: device
                        .vid
                        .zip(device.pid)
                        .map(|(vendor, product)| HardwareIdentifier {
                            namespace: device.id_namespace.clone(),
                            vendor,
                            product,
                        })
                        .into_iter()
                        .collect(),
                    identities: vec![Identity {
                        namespace: "bluetooth.classic".into(),
                        kind: IdentityKind::Address,
                        subject: "device".into(),
                        value: device.address.clone(),
                    }],
                    links: vec![Link {
                        transport: "bluetooth.classic".into(),
                        protocol: "windows.device-property".into(),
                    }],
                    target: String::new(),
                },
                device.connection,
                battery,
            ))
        })
        .collect()
}

pub fn inventory() -> Result<Vec<DeviceDescriptor>, HardwareError> {
    collect(None).map(|devices| {
        devices
            .into_iter()
            .map(|(descriptor, _, _)| descriptor)
            .collect()
    })
}

pub fn discover() -> Result<Vec<Device>, HardwareError> {
    collect(None)?
        .into_iter()
        .map(|(descriptor, connection, _)| {
            let key = descriptor.key.local.clone();
            let mut capabilities = Capabilities::default();
            capabilities.insert(
                "main",
                Battery::new(move || {
                    let (descriptor, connection, level) = collect(Some(&key))?
                        .into_iter()
                        .find(|(descriptor, _, _)| descriptor.key.local == key)
                        .ok_or_else(|| {
                            HardwareError::new(
                                ErrorKind::Unavailable,
                                "Bluetooth device disappeared",
                            )
                        })?;
                    let state = match level {
                        Some(level) => BatteryState::Value(Sample {
                            level: Percent::new(level)?,
                            origin: SampleOrigin::SystemCache,
                            observed_at: SystemTime::now(),
                            sampled_at: None,
                        }),
                        None => BatteryState::Unavailable(HardwareError::new(
                            ErrorKind::Unavailable,
                            "Windows has no battery property",
                        )),
                    };
                    Ok(BatteryReading {
                        state,
                        facts: DeviceFacts {
                            connection: Some(connection),
                            name: (!descriptor.name.is_empty()).then_some(descriptor.name),
                            identities: descriptor.identities,
                        },
                    })
                }),
            )?;
            Ok(Device {
                descriptor,
                connection,
                capabilities,
                issue: None,
            })
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_or_malformed_connection_is_never_connected() {
        for bytes in [None, Some(&[][..]), Some(&[1][..]), Some(&[0xff, 0][..])] {
            assert_eq!(connection_value(bytes), Connection::Unknown);
        }
        assert_eq!(connection_value(Some(&[0xff])), Connection::Connected);
        assert_eq!(connection_value(Some(&[0])), Connection::Disconnected);
    }

    #[test]
    fn property_battery_requires_one_valid_percentage_byte() {
        assert_eq!(battery_value(&[0]), Some(0));
        assert_eq!(battery_value(&[100]), Some(100));
        for bytes in [&[][..], &[101][..], &[255][..], &[80, 0][..]] {
            assert_eq!(battery_value(bytes), None);
        }
    }
}
