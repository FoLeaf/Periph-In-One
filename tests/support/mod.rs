#![allow(dead_code)]
use periph_hal::{
    Capabilities, Connection, Device, DeviceDescriptor, DeviceKey, Identity, IdentityKind,
    Snapshot,
    battery::{Battery, BatteryReading, BatteryState, Percent, Sample, SampleOrigin},
};

pub fn device(source: &str, route: &str, uid: &str, serial: &str) -> Device {
    let mut identities = vec![Identity {
        namespace: "test.location".into(),
        kind: IdentityKind::Location,
        subject: "device".into(),
        value: format!("{source}:{route}"),
    }];
    if !uid.is_empty() {
        identities.push(Identity {
            namespace: "test.unit".into(),
            kind: IdentityKind::UnitId,
            subject: "device".into(),
            value: uid.into(),
        });
    }
    if !serial.is_empty() {
        identities.push(Identity {
            namespace: "test.serial".into(),
            kind: IdentityKind::Serial,
            subject: "device".into(),
            value: serial.into(),
        });
    }
    Device {
        descriptor: DeviceDescriptor {
            key: DeviceKey {
                backend: source.into(),
                local: route.into(),
            },
            name: "Example device".into(),
            identities,
            roles: vec!["video.capture".into()],
            ..DeviceDescriptor::default()
        },
        connection: Connection::Connected,
        capabilities: Capabilities::default(),
        issue: None,
    }
}

pub fn snapshot(devices: Vec<Device>, completed: &[&str]) -> Snapshot {
    Snapshot {
        devices,
        completed: completed.iter().map(|source| (*source).into()).collect(),
        failures: vec![],
    }
}

pub fn battery(device: &mut Device, component: &str, level: Option<u8>) {
    let state = match level {
        Some(level) => BatteryState::Value(Sample {
            level: Percent::new(level).unwrap(),
            origin: SampleOrigin::DeviceResponse,
            observed_at: std::time::UNIX_EPOCH,
            sampled_at: None,
        }),
        None => BatteryState::Unavailable(periph_hal::HardwareError::new(
            periph_hal::ErrorKind::Unavailable,
            "no reading",
        )),
    };
    battery_state(device, component, state);
}

pub fn battery_state(device: &mut Device, component: &str, state: BatteryState) {
    device
        .capabilities
        .insert(
            component,
            Battery::new(move || {
                Ok(BatteryReading {
                    state: state.clone(),
                    facts: periph_hal::DeviceFacts::default(),
                })
            }),
        )
        .unwrap();
}
