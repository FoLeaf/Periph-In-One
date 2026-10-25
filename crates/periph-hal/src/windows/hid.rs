//! HID discovery and driver dispatch. Native topology and I/O are separate.
use super::topology::{container_id, port_location, raw_serial_number};
use crate::{
    Capabilities, Connection, Device, DeviceDescriptor, DeviceFacts, DeviceKey, ErrorKind,
    HardwareError, HardwareIdentifier, Identity, IdentityKind, Link,
    battery::{Battery, BatteryReading, BatteryState, Percent, Sample, SampleOrigin},
    vendor,
};
use hidapi::{DeviceInfo, HidApi};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::SystemTime,
};

fn receiver_interfaces(api: &HidApi) -> BTreeMap<String, Vec<&DeviceInfo>> {
    let mut groups: BTreeMap<String, Vec<&DeviceInfo>> = BTreeMap::new();
    for info in api
        .device_list()
        .filter(|d| vendor::ADAPTERS.iter().any(|adapter| adapter.matches(d)))
    {
        groups.entry(container_id(info)).or_default().push(info);
    }
    for interfaces in groups.values_mut() {
        // FF05 first: the ATK dongle's protocol interface.
        interfaces.sort_by_key(|d| d.usage_page() != 0xff05);
    }
    groups
}

/// Passive discovery does not open protocol handles or issue device queries.
pub fn inventory() -> Result<Vec<DeviceDescriptor>, HardwareError> {
    let api = HidApi::new().map_err(HardwareError::transport)?;
    Ok(receiver_interfaces(&api)
        .values()
        .map(|interfaces| {
            let info = interfaces[0];
            descriptor(info, port_location(info), "", None, String::new())
        })
        .collect())
}

fn descriptor(
    info: &DeviceInfo,
    route: String,
    role: &str,
    name: Option<String>,
    uid: String,
) -> DeviceDescriptor {
    let mut identities = vec![Identity {
        namespace: "windows.usb-port".into(),
        kind: IdentityKind::Location,
        subject: "receiver".into(),
        value: port_location(info),
    }];
    let serial = raw_serial_number(info);
    if !serial.is_empty() {
        identities.push(Identity {
            namespace: format!("usb:{:04x}:{:04x}", info.vendor_id(), info.product_id()),
            kind: IdentityKind::Serial,
            subject: "receiver".into(),
            value: serial,
        });
    }
    if !uid.is_empty() {
        identities.push(Identity {
            namespace: format!("usb:{:04x}", info.vendor_id()),
            kind: IdentityKind::UnitId,
            subject: "device".into(),
            value: uid,
        });
    }
    DeviceDescriptor {
        key: DeviceKey {
            backend: "windows.hid".into(),
            local: route,
        },
        name: name.unwrap_or_else(|| info.product_string().unwrap_or_default().into()),
        roles: if role.is_empty() {
            vec![]
        } else {
            vec![if role == "headset" {
                "audio.headset".into()
            } else {
                format!("input.{role}")
            }]
        },
        identifiers: vec![HardwareIdentifier {
            namespace: "usb".into(),
            vendor: info.vendor_id(),
            product: info.product_id(),
        }],
        identities,
        links: vec![Link {
            transport: format!("{:?}", info.bus_type()).to_lowercase(),
            protocol: "hid".into(),
        }],
        target: String::new(),
    }
}

pub fn discover() -> Result<Vec<Device>, HardwareError> {
    let api = HidApi::new().map_err(HardwareError::transport)?;
    let mut result = Vec::new();
    for interfaces in receiver_interfaces(&api).values() {
        let info = interfaces[0];
        let Some(adapter) = vendor::ADAPTERS.iter().find(|a| a.matches(info)) else {
            continue;
        };
        let lock = Arc::new(Mutex::new(()));
        for outcome in adapter.discover(&api, info) {
            let mut route = port_location(info);
            if let Some(slot) = outcome.slot {
                route.push_str(&format!("#{slot}"));
            }
            let mut capabilities = Capabilities::default();
            let mut description = descriptor(
                info,
                route,
                adapter.kind(),
                outcome.hardware_name,
                outcome.uid,
            );
            description.target = outcome
                .slot
                .map(|slot| format!("slot:{slot}"))
                .unwrap_or_default();
            let expected = info.clone();
            let expected_description = description.clone();
            let slot = outcome.slot;
            let unsupported = outcome.unsupported;
            let lock = lock.clone();
            capabilities.insert(
                "main",
                Battery::new(move || {
                    if unsupported {
                        return Ok(BatteryReading {
                            state: BatteryState::Unsupported,
                            facts: DeviceFacts::default(),
                        });
                    }
                    let _guard = lock.lock().map_err(|_| {
                        HardwareError::new(ErrorKind::Unavailable, "receiver session lock poisoned")
                    })?;
                    let api = HidApi::new().map_err(HardwareError::transport)?;
                    let current = api
                        .device_list()
                        .find(|current| {
                            current.path() == expected.path()
                                && current.vendor_id() == expected.vendor_id()
                                && current.product_id() == expected.product_id()
                        })
                        .ok_or_else(|| {
                            HardwareError::new(
                                ErrorKind::Unavailable,
                                "device endpoint disappeared",
                            )
                        })?;
                    let current_serial = raw_serial_number(current);
                    if expected_description.identities.iter().any(|identity| {
                        identity.kind == IdentityKind::Serial
                            && !current_serial.is_empty()
                            && identity.value != current_serial
                    }) {
                        return Err(HardwareError::new(
                            ErrorKind::Conflict,
                            "endpoint identity changed",
                        ));
                    }
                    let adapter = vendor::ADAPTERS
                        .iter()
                        .find(|adapter| adapter.matches(current))
                        .ok_or_else(|| {
                            HardwareError::new(ErrorKind::Unsupported, "no matching driver")
                        })?;
                    let result = adapter.read_target(&api, current, slot);
                    if !result.uid.is_empty()
                        && expected_description.identities.iter().any(|identity| {
                            identity.kind == IdentityKind::UnitId && identity.value != result.uid
                        })
                    {
                        return Err(HardwareError::new(
                            ErrorKind::Conflict,
                            "peripheral identity changed",
                        ));
                    }
                    let state = if let Some(level) = result.battery {
                        BatteryState::Value(Sample {
                            level: Percent::new(level)?,
                            origin: SampleOrigin::DeviceResponse,
                            observed_at: SystemTime::now(),
                            sampled_at: None,
                        })
                    } else if result.unsupported {
                        BatteryState::Unsupported
                    } else {
                        BatteryState::Unavailable(result.issue.unwrap_or_else(|| {
                            HardwareError::new(ErrorKind::Unavailable, "battery level unavailable")
                        }))
                    };
                    let identities = if result.uid.is_empty() {
                        vec![]
                    } else {
                        vec![Identity {
                            namespace: format!("usb:{:04x}", current.vendor_id()),
                            kind: IdentityKind::UnitId,
                            subject: "device".into(),
                            value: result.uid,
                        }]
                    };
                    Ok(BatteryReading {
                        state,
                        facts: DeviceFacts {
                            connection: (result.connection != Connection::Unknown)
                                .then_some(result.connection),
                            name: result.hardware_name,
                            identities,
                        },
                    })
                }),
            )?;
            result.push(Device {
                descriptor: description,
                connection: outcome.connection,
                capabilities,
                issue: outcome.issue,
            });
        }
    }
    Ok(result)
}
