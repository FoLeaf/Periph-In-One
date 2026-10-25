//! Coordinate device-core facts and optional battery-domain observations.
use crate::{
    battery::{Batteries, Observation},
    model::{self, Device},
};
use periph_hal::{Capability, Snapshot, battery::BatteryState};

#[derive(Debug)]
pub struct ScanReport {
    pub snapshot: Snapshot,
    pub batteries: Vec<Observation>,
}

pub fn from_snapshot(snapshot: Snapshot) -> ScanReport {
    let batteries = snapshot
        .devices
        .iter()
        .flat_map(|device| {
            device
                .capabilities
                .descriptors()
                .filter(|capability| capability.id == BatteryState::ID)
                .filter_map(|capability| {
                    device
                        .capabilities
                        .get::<BatteryState>(&capability.component)
                        .map(|state| Observation {
                            key: device.descriptor.key.clone(),
                            component: capability.component.clone(),
                            state: state.clone(),
                        })
                })
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
