use periph_hal::{
    BackendFailure, Capabilities, Connection, Device, DeviceDescriptor, DeviceKey, ErrorKind,
    HardwareError, HardwareIdentifier, Identity, IdentityKind, Link, Snapshot,
    battery::{BatteryState, Percent, Sample, SampleOrigin},
};
use periph_in_one::{hardware, model};
use std::time::UNIX_EPOCH;

fn headset(state: BatteryState, connection: Connection) -> Device {
    let mut capabilities = Capabilities::default();
    capabilities.insert("main", state).unwrap();
    Device {
        descriptor: DeviceDescriptor {
            key: DeviceKey {
                backend: "windows.bluetooth.audio".into(),
                local: "bt:001122334455".into(),
            },
            name: "Headset".into(),
            roles: vec!["audio.headset".into()],
            identifiers: vec![HardwareIdentifier {
                namespace: "bluetooth-sig".into(),
                vendor: 2,
                product: 3,
            }],
            identities: vec![],
            links: vec![Link {
                transport: "bluetooth.classic".into(),
                protocol: "system-property".into(),
            }],
        },
        connection,
        capabilities,
        issue: None,
    }
}

fn value(level: u8) -> BatteryState {
    BatteryState::Value(Sample {
        level: Percent::new(level).unwrap(),
        origin: SampleOrigin::SystemCache,
        observed_at: UNIX_EPOCH,
        sampled_at: None,
    })
}

fn report(devices: Vec<Device>) -> hardware::ScanReport {
    hardware::from_snapshot(Snapshot {
        devices,
        completed: vec!["windows.bluetooth.audio".into()],
        failures: vec![],
    })
}

#[test]
fn missing_battery_keeps_connection_but_cannot_refresh_history() {
    let first = hardware::merge_report(
        &[],
        report(vec![headset(value(80), Connection::Connected)]),
        10,
    );
    let missing = BatteryState::Unavailable(HardwareError::new(
        ErrorKind::Unavailable,
        "missing property",
    ));
    let second = hardware::merge_report(
        &first,
        report(vec![headset(missing, Connection::Connected)]),
        20,
    );
    assert_eq!(second[0].connection, Connection::Connected);
    assert_eq!(second[0].saved.battery, Some(80));
    assert_eq!(second[0].saved.last_success, Some(10));
    assert!(!second[0].fresh);
    assert!(!second[0].no_battery);
    assert_eq!(second[0].saved.id, first[0].saved.id);
}

#[test]
fn cached_battery_never_establishes_connection() {
    let uncertain = report(vec![headset(value(80), Connection::Unknown)]);
    assert_eq!(uncertain.readings[0].connection, Connection::Unknown);
    assert_eq!(uncertain.readings[0].battery, None);
    let disconnected = report(vec![headset(value(80), Connection::Disconnected)]);
    assert!(disconnected.readings.is_empty());
    let zero = report(vec![headset(value(0), Connection::Connected)]);
    assert_eq!(zero.readings[0].battery, Some(0));
}

#[test]
fn failed_source_preserves_presence_and_history_without_affecting_other_sources() {
    let first = hardware::merge_report(
        &[],
        report(vec![headset(value(80), Connection::Connected)]),
        10,
    );
    let saved = first[0].saved.clone();
    let failed = hardware::ScanReport {
        readings: vec![],
        failures: vec![BackendFailure {
            backend: "windows.bluetooth.audio".into(),
            error: HardwareError::new(ErrorKind::Transport, "enumeration failed"),
        }],
    };
    let second = hardware::merge_report(&first, failed, 20);
    assert!(second[0].present);
    assert_eq!(second[0].connection, Connection::Unknown);
    assert!(!second[0].fresh);
    assert_eq!(second[0].saved.id, saved.id);
    assert_eq!(second[0].saved.last_success, saved.last_success);
    let completed_empty = hardware::merge_report(&first, report(vec![]), 30);
    assert!(!completed_empty[0].present);
    assert_eq!(completed_empty[0].connection, Connection::Disconnected);
}

#[test]
fn existing_usb_keys_and_remarks_survive_the_hal_boundary() {
    let mut device = headset(value(65), Connection::Connected);
    device.descriptor.key = DeviceKey {
        backend: "windows.hid".into(),
        local: "port-a#1".into(),
    };
    device.descriptor.roles = vec!["input.mouse".into()];
    device.descriptor.links = vec![Link {
        transport: "usb".into(),
        protocol: "hid".into(),
    }];
    device.descriptor.identifiers = vec![HardwareIdentifier {
        namespace: "usb".into(),
        vendor: 0x046d,
        product: 0xc539,
    }];
    device.descriptor.identities = vec![Identity {
        namespace: "windows.usb-port".into(),
        kind: IdentityKind::Location,
        subject: "receiver".into(),
        value: "port-a".into(),
    }];
    let readings = report(vec![device]).readings;
    assert_eq!(readings[0].physical_id, "port-a");
    let first = model::merge_readings(&[], readings.clone(), 1);
    let mut saved = first[0].saved.clone();
    saved.alias = "Work mouse".into();
    let second = model::merge_readings(&[saved], readings, 2);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].saved.id, first[0].saved.id);
    assert_eq!(second[0].saved.alias, "Work mouse");
}

#[test]
fn application_does_not_require_a_battery_on_every_device() {
    let mut camera = headset(value(20), Connection::Connected);
    camera.descriptor.roles = vec!["video.capture".into()];
    camera.capabilities = Capabilities::default();
    assert!(report(vec![camera]).readings.is_empty());
}
