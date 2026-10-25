//! Battery observations are one optional domain contract, not device-core data.
use crate::{Capability, DeviceFacts, ErrorKind, HardwareError};
use std::{
    sync::{Arc, Mutex},
    time::SystemTime,
};

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

#[derive(Clone, Debug)]
pub struct BatteryReading {
    pub state: BatteryState,
    pub facts: DeviceFacts,
}

pub trait BatteryReader: Send + Sync {
    fn read(&self) -> Result<BatteryReading, HardwareError>;
}

impl<F> BatteryReader for F
where
    F: Fn() -> Result<BatteryReading, HardwareError> + Send + Sync,
{
    fn read(&self) -> Result<BatteryReading, HardwareError> {
        self()
    }
}

/// An independently sampled capability, valid until rediscovery or shutdown.
#[derive(Clone)]
pub struct Battery {
    reader: Arc<Mutex<Option<Arc<dyn BatteryReader>>>>,
}

impl Battery {
    pub fn new(reader: impl BatteryReader + 'static) -> Self {
        Self {
            reader: Arc::new(Mutex::new(Some(Arc::new(reader)))),
        }
    }

    pub fn read(&self) -> Result<BatteryReading, HardwareError> {
        let reader = self.reader.lock().map_err(|_| {
            HardwareError::new(ErrorKind::Unavailable, "battery capability lock poisoned")
        })?;
        reader
            .as_ref()
            .ok_or_else(|| {
                HardwareError::new(ErrorKind::Unavailable, "battery capability has expired")
            })?
            .read()
    }

    pub fn close(&self) {
        if let Ok(mut reader) = self.reader.lock() {
            reader.take();
        }
    }
}

impl Capability for Battery {
    const ID: &'static str = "power.battery";
    const VERSION: u16 = 2;
    fn invalidate(&self) {
        self.close();
    }
}
