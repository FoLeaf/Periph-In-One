//! Version-1 knowledge is confined to one-time configuration migration.
use crate::{
    battery::SavedBattery,
    config::Config,
    model::{EndpointRoute, SavedComponent, SavedDevice},
};
use periph_hal::{
    ComponentDescriptor, ComponentKey, ComponentRoute, DeviceDescriptor, DeviceKey, EndpointKey,
    HardwareIdentifier, Identity, IdentityKind, Link,
};
use serde::Deserialize;

#[derive(Default, Deserialize)]
#[serde(default)]
struct LegacyDevice {
    id: String,
    physical_id: String,
    kind: String,
    hardware_name: String,
    serial: String,
    uid: String,
    vid: u16,
    pid: u16,
    alias: String,
    battery: Option<u8>,
    last_success: Option<u64>,
    identity_conflict: bool,
}

#[derive(Deserialize)]
#[serde(default)]
struct LegacyConfig {
    width: f32,
    height: f32,
    devices: Vec<LegacyDevice>,
}
impl Default for LegacyConfig {
    fn default() -> Self {
        Self {
            width: 360.0,
            height: 640.0,
            devices: vec![],
        }
    }
}

fn physical_id(old: &LegacyDevice) -> String {
    if !old.physical_id.is_empty() {
        return old.physical_id.clone();
    }
    let Some(tuple) = old.id.strip_prefix("local:") else {
        return String::new();
    };
    let Ok((vid, pid, value)) = serde_json::from_str::<(u16, u16, String)>(tuple) else {
        return String::new();
    };
    if (vid, pid) != (old.vid, old.pid) {
        return String::new();
    }
    if let Some((session, tail)) = value.split_once(':')
        && session.split_once('-').is_some_and(|(pid, time)| {
            !pid.is_empty()
                && !time.is_empty()
                && pid
                    .bytes()
                    .chain(time.bytes())
                    .all(|byte| byte.is_ascii_digit())
        })
        && let Some((epoch, identity)) = tail.split_once(':')
        && !epoch.is_empty()
        && epoch.bytes().all(|byte| byte.is_ascii_digit())
    {
        return identity.into();
    }
    value
}

pub(crate) fn version_one(bytes: &[u8]) -> Result<Config, serde_json::Error> {
    let old: LegacyConfig = serde_json::from_slice(bytes)?;
    let mut config = Config {
        width: old.width,
        height: old.height,
        ..Config::default()
    };
    for old in old.devices {
        let physical = physical_id(&old);
        let bluetooth = old.kind == "bt-headset" || physical.starts_with("bt:");
        let backend = if bluetooth {
            "windows.bluetooth.audio"
        } else {
            "windows.hid"
        };
        let slot_receiver = !bluetooth
            && old.vid == 0x046d
            && matches!(
                old.pid,
                0xc539
                    | 0xc53a
                    | 0xc53d
                    | 0xc53f
                    | 0xc541
                    | 0xc545
                    | 0xc547
                    | 0xc54d
                    | 0xc52b
                    | 0xc532
                    | 0xc548
            );
        // V1 single-slot readings omitted #1; HAL logical routes are stable.
        let route = if slot_receiver && !physical.contains('#') {
            format!("{physical}#1")
        } else {
            physical.clone()
        };
        let target = if slot_receiver {
            route
                .rsplit_once('#')
                .map(|(_, slot)| format!("slot:{slot}"))
                .unwrap_or_default()
        } else {
            String::new()
        };
        let key = DeviceKey {
            backend: backend.into(),
            local: route,
        };
        let endpoint = EndpointKey {
            backend: backend.into(),
            local: if bluetooth {
                physical.clone()
            } else if slot_receiver {
                physical.split('#').next().unwrap_or(&physical).into()
            } else {
                physical.clone()
            },
        };
        let mut identities = Vec::new();
        if bluetooth {
            if let Some(address) = physical
                .strip_prefix("bt:")
                .filter(|address| !address.is_empty())
            {
                identities.push(Identity {
                    namespace: "bluetooth.classic".into(),
                    kind: IdentityKind::Address,
                    subject: "device".into(),
                    value: address.to_ascii_uppercase(),
                });
            }
        } else if !physical.is_empty() {
            let location = if slot_receiver {
                physical.split('#').next().unwrap_or(&physical)
            } else {
                &physical
            };
            identities.push(Identity {
                namespace: "windows.usb-port".into(),
                kind: IdentityKind::Location,
                subject: "receiver".into(),
                value: location.into(),
            });
        }
        let invalid_serial = !old.serial.is_empty()
            && old
                .serial
                .trim()
                .eq_ignore_ascii_case(old.hardware_name.trim());
        if !old.serial.is_empty() && !invalid_serial {
            identities.push(Identity {
                namespace: format!("usb:{:04x}:{:04x}", old.vid, old.pid),
                kind: IdentityKind::Serial,
                subject: "receiver".into(),
                value: old.serial,
            });
        }
        if !old.uid.is_empty() {
            identities.push(Identity {
                namespace: format!("usb:{:04x}", old.vid),
                kind: IdentityKind::UnitId,
                subject: "device".into(),
                value: old.uid,
            });
        }
        let role = match old.kind.as_str() {
            "mouse" => "input.mouse",
            "keyboard" => "input.keyboard",
            "gamepad" => "input.gamepad",
            "headset" | "bt-headset" => "audio.headset",
            _ => "device.unknown",
        };
        let id = if old.id.is_empty() {
            format!("migrated:{}", serde_json::to_string(&key).unwrap())
        } else {
            old.id
        };
        if let Some(level) = old.battery.filter(|level| *level <= 100) {
            config.batteries.push(SavedBattery {
                device_id: id.clone(),
                component: "main".into(),
                level,
                observed_at: old.last_success.unwrap_or(0),
            });
        }
        config.devices.push(SavedDevice {
            id,
            alias: old.alias,
            identity_conflict: old.identity_conflict || invalid_serial,
            routes: vec![key.clone()],
            endpoint_routes: vec![EndpointRoute {
                route: key.clone(),
                endpoint: endpoint.clone(),
            }],
            components: Vec::new(),
            descriptor: DeviceDescriptor {
                key,
                endpoint,
                components: Vec::new(),
                target,
                name: old.hardware_name,
                roles: vec![role.into()],
                identities,
                identifiers: if old.vid == 0 && old.pid == 0 {
                    vec![]
                } else {
                    vec![HardwareIdentifier {
                        namespace: if bluetooth { "bluetooth.pnp" } else { "usb" }.into(),
                        vendor: old.vid,
                        product: old.pid,
                    }]
                },
                links: vec![Link {
                    transport: if bluetooth {
                        "bluetooth.classic"
                    } else {
                        "usb"
                    }
                    .into(),
                    protocol: if bluetooth {
                        "windows.device-property"
                    } else {
                        "hid"
                    }
                    .into(),
                }],
            },
            last_dpi_stage: None,
            last_lighting_on: None,
        });
    }
    Ok(config)
}

/// Normalize additive endpoint fields on existing v2 records. Platform
/// identity rules stay at the persistence boundary instead of in live model
/// reconciliation.
pub(crate) fn normalize_v2(config: &mut Config) {
    for saved in &mut config.devices {
        if saved.descriptor.endpoint.is_empty() {
            let route = &saved.descriptor.key;
            let local = if route.backend == "windows.hid" {
                saved
                    .descriptor
                    .identities
                    .iter()
                    .find(|identity| {
                        identity.namespace == "windows.usb-port"
                            && identity.kind == IdentityKind::Location
                            && identity.subject == "receiver"
                    })
                    .map(|identity| identity.value.clone())
                    .unwrap_or_else(|| route.local.clone())
            } else {
                route.local.clone()
            };
            saved.descriptor.endpoint = EndpointKey {
                backend: route.backend.clone(),
                local,
            };
        }
        let descriptor_backend = saved.descriptor.key.backend.clone();
        let descriptor_endpoint = saved.descriptor.endpoint.clone();
        for route in saved
            .routes
            .iter()
            .chain(std::iter::once(&saved.descriptor.key))
        {
            if saved
                .endpoint_routes
                .iter()
                .any(|mapping| &mapping.route == route)
            {
                continue;
            }
            let endpoint = if route.backend == descriptor_backend {
                descriptor_endpoint.clone()
            } else {
                EndpointKey {
                    backend: route.backend.clone(),
                    local: route.local.clone(),
                }
            };
            saved.endpoint_routes.push(EndpointRoute {
                route: route.clone(),
                endpoint,
            });
        }
        let legacy_main = ComponentKey::legacy("main");
        if !saved
            .descriptor
            .components
            .iter()
            .any(|component| component.key == legacy_main)
        {
            saved.descriptor.components.push(ComponentDescriptor {
                key: legacy_main.clone(),
                ..ComponentDescriptor::default()
            });
        }
        if !saved
            .components
            .iter()
            .any(|component| component.id == "main")
        {
            let mut component_routes = Vec::new();
            for route in saved
                .routes
                .iter()
                .chain(std::iter::once(&saved.descriptor.key))
            {
                let component_route = ComponentRoute {
                    device: route.clone(),
                    component: legacy_main.clone(),
                };
                if !component_routes.contains(&component_route) {
                    component_routes.push(component_route);
                }
            }
            saved.components.push(SavedComponent {
                id: "main".into(),
                routes: component_routes,
                ..SavedComponent::default()
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::battery::SavedBattery;

    #[test]
    fn v2_normalization_adds_implicit_main_without_changing_saved_history() {
        let descriptor_route = DeviceKey {
            backend: "windows.hid".into(),
            local: "receiver/slot-1".into(),
        };
        let secondary_route = DeviceKey {
            backend: "windows.hid".into(),
            local: "receiver/slot-2".into(),
        };
        let mut config = Config {
            devices: vec![SavedDevice {
                id: "old-record-id".into(),
                alias: "my headset".into(),
                routes: vec![secondary_route.clone()],
                descriptor: DeviceDescriptor {
                    key: descriptor_route.clone(),
                    identities: vec![Identity {
                        namespace: "windows.usb-port".into(),
                        kind: IdentityKind::Location,
                        subject: "receiver".into(),
                        value: "port-7".into(),
                    }],
                    ..DeviceDescriptor::default()
                },
                ..SavedDevice::default()
            }],
            batteries: vec![SavedBattery {
                device_id: "old-record-id".into(),
                component: "main".into(),
                level: 63,
                observed_at: 1234,
            }],
            ..Config::default()
        };

        normalize_v2(&mut config);

        let saved = &config.devices[0];
        assert_eq!(saved.id, "old-record-id");
        assert_eq!(saved.alias, "my headset");
        assert!(
            saved
                .descriptor
                .components
                .iter()
                .any(|component| component.key == ComponentKey::legacy("main"))
        );
        let main = saved
            .components
            .iter()
            .find(|component| component.id == "main")
            .unwrap();
        assert_eq!(main.routes.len(), 2);
        assert!(main.routes.contains(&ComponentRoute {
            device: descriptor_route,
            component: ComponentKey::legacy("main"),
        }));
        assert!(main.routes.contains(&ComponentRoute {
            device: secondary_route,
            component: ComponentKey::legacy("main"),
        }));
        assert_eq!(config.batteries[0].component, "main");
        assert_eq!(config.batteries[0].level, 63);
        assert_eq!(config.batteries[0].observed_at, 1234);
    }
}
