//! Battery observations are one optional domain contract, not device-core data.
use crate::{Capability, ErrorKind, HardwareError};
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Percent(u8);

impl Percent {
    pub fn new(value: u8) -> Result<Self, HardwareError> {
        if value <= 100 {
            Ok(Self(value))
        } else {
            Err(HardwareError::new(
                ErrorKind::InvalidResponse,
                "battery percentage exceeds 100",
            ))
        }
    }

    pub const fn get(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleOrigin {
    DeviceResponse,
    SystemCache,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sample {
    pub level: Percent,
    pub origin: SampleOrigin,
    pub observed_at: SystemTime,
    /// None when the backend cannot establish the device's sampling time.
    pub sampled_at: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatteryState {
    Value(Sample),
    /// A verified device/model lacks this capability, not a failed read.
    Unsupported,
    Unavailable(HardwareError),
}

impl Capability for BatteryState {
    const ID: &'static str = "power.battery";
}
