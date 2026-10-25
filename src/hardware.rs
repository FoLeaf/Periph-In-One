//! Coordinate device-core facts and optional battery-domain observations.
use crate::{
    battery::{Batteries, Observation},
    model::{self, Device},
};
use periph_hal::{
    Capability, Snapshot,
    battery::{Battery, BatteryState},
};

#[derive(Debug)]
pub struct ScanReport {
    pub snapshot: Snapshot,
    pub batteries: Vec<Observation>,
}

/// Explicit application workflow: discover first, then invoke only the battery
/// capability. Other applications can choose entirely different capabilities.
pub fn from_snapshot(mut snapshot: Snapshot) -> ScanReport {
    let batteries = snapshot
        .devices
        .iter_mut()
        .flat_map(|device| {
            let components: Vec<_> = device
                .capabilities
                .descriptors()
                .filter(|capability| {
                    capability.id == Battery::ID && capability.version == Battery::VERSION
                })
                .map(|capability| capability.component.clone())
                .collect();
            components
                .into_iter()
                .filter_map(|component| {
                    let battery = device.capabilities.get::<Battery>(&component)?.clone();
                    let state = match battery.read() {
                        Ok(reading) => {
                            reading.facts.apply_to(device);
                            reading.state
                        }
                        Err(error) => {
                            device.issue = Some(error.clone());
                            BatteryState::Unavailable(error)
                        }
                    };
                    Some(Observation {
                        key: device.descriptor.key.clone(),
                        component,
                        state,
                    })
                })
                .collect::<Vec<_>>()
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
