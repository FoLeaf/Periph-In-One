//! Coordinate device-core facts and optional battery-domain observations.
use crate::{
    battery::{Batteries, Observation},
    model::{self, Device},
};
use periph_hal::{
    Capability, Snapshot,
    battery::{Battery, BatteryReadRequest, BatteryState},
};
use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct ScanReport {
    pub snapshot: Snapshot,
    pub batteries: Vec<Observation>,
}

/// Explicit application workflow: discover first, then invoke only the battery
/// capability. Other applications can choose entirely different capabilities.
pub fn from_snapshot(snapshot: Snapshot) -> ScanReport {
    from_snapshot_with_timeout(snapshot, periph_hal::DEFAULT_OPERATION_TIMEOUT)
}

pub fn from_snapshot_with_timeout(snapshot: Snapshot, timeout: Duration) -> ScanReport {
    from_snapshot_inner(snapshot, timeout, None)
}

pub fn from_snapshot_cancellable(
    snapshot: Snapshot,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> ScanReport {
    from_snapshot_inner(snapshot, timeout, Some(cancelled))
}

fn from_snapshot_inner(
    mut snapshot: Snapshot,
    timeout: Duration,
    cancelled: Option<&AtomicBool>,
) -> ScanReport {
    let deadline = Instant::now() + timeout;
    let mut pending = Vec::<(usize, periph_hal::DeviceKey, String, BatteryReadRequest)>::new();
    for (device_index, device) in snapshot.devices.iter().enumerate() {
        let components: Vec<_> = device
            .capabilities
            .descriptors()
            .filter(|capability| {
                capability.id == Battery::ID && capability.version == Battery::VERSION
            })
            .map(|capability| capability.component.clone())
            .collect();
        for component in components {
            if let Some(battery) = device.capabilities.get::<Battery>(&component) {
                pending.push((
                    device_index,
                    device.descriptor.key.clone(),
                    component,
                    battery.request_read(),
                ));
            }
        }
    }

    // Every queued component shares this one refresh deadline. A stalled HID
    // read cannot reset the clock or delay a result from another backend.
    let batteries = pending
        .into_iter()
        .map(|(device_index, key, component, request)| {
            let result = match cancelled {
                Some(cancelled) => request.wait_cancellable(
                    deadline.saturating_duration_since(Instant::now()),
                    cancelled,
                ),
                None => request.wait(deadline.saturating_duration_since(Instant::now())),
            };
            let state = match result {
                Ok(reading) => {
                    reading.facts.apply_to(&mut snapshot.devices[device_index]);
                    reading.state
                }
                Err(error) => {
                    snapshot.devices[device_index].issue = Some(error.clone());
                    BatteryState::Unavailable(error)
                }
            };
            Observation {
                key,
                component,
                state,
            }
        })
        .collect();
    ScanReport {
        snapshot,
        batteries,
    }
}

pub fn merge_report(
    previous: &[Device],
    batteries: &mut Batteries,
    report: ScanReport,
    now: u64,
) -> Vec<Device> {
    let merged = model::merge_snapshot(previous, &report.snapshot);
    batteries.apply(&report.batteries, &merged, now);
    merged.devices
}
