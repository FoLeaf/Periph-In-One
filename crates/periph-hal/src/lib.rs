//! Reusable peripheral access, independent of application state and UI.
//!
//! Backends discover devices and provide typed capabilities. The core does
//! not enumerate device categories or import any capability's payload type.

pub mod battery;
mod capability;
mod device;
mod error;
mod hardware;
pub mod protocol;

pub use capability::{
    BackendExecutor, BackendOperation, Capabilities, Capability, CapabilityDescriptor,
};
pub use device::{
    Connection, Device, DeviceDescriptor, DeviceFacts, DeviceKey, HardwareIdentifier, Identity,
    IdentityKind, Link,
};
pub use error::{ErrorKind, HardwareError};
pub use hardware::{
    Backend, BackendDiscoveryUpdate, BackendFailure, DiscoveryPoll, DiscoveryStream, Hardware,
    Inventory, Snapshot,
};

/// Default deadline for one backend or capability operation batch.
pub const DEFAULT_OPERATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

#[cfg(all(windows, feature = "windows-backend"))]
mod windows;
#[cfg(all(windows, feature = "windows-backend"))]
use windows::vendor;

impl Hardware {
    /// Construct backends for the current OS. Unsupported platforms return an
    /// error instead of pretending an empty hardware inventory is authoritative.
    pub fn system() -> Result<Self, HardwareError> {
        #[cfg(all(windows, feature = "windows-backend"))]
        {
            let mut hardware = Self::new();
            hardware.register_factory("windows.hid", || windows::HidBackend)?;
            hardware
                .register_factory("windows.bluetooth.audio", || windows::BluetoothAudioBackend)?;
            Ok(hardware)
        }
        #[cfg(not(all(windows, feature = "windows-backend")))]
        Err(HardwareError::new(
            ErrorKind::Unsupported,
            "no platform backend enabled",
        ))
    }
}

/// Explicit platform-specific probe tools, excluded from the portable API and
/// normal application builds. They never form a dependency of app business code.
#[cfg(all(windows, feature = "diagnostics"))]
pub mod diagnostics {
    pub use crate::windows::transport::{KzziLink, drain, send};
}
