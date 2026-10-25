//! Reusable peripheral access, independent of application state and UI.
//!
//! Backends discover devices and provide typed capabilities. The core does
//! not enumerate device categories or import any capability's payload type.

pub mod battery;
mod capability;
mod device;
mod error;
mod hardware;

pub use capability::{Capabilities, Capability, CapabilityDescriptor};
pub use device::{
    Connection, Device, DeviceDescriptor, DeviceKey, HardwareIdentifier, Identity, IdentityKind,
    Link,
};
pub use error::{ErrorKind, HardwareError};
pub use hardware::{Backend, BackendFailure, Hardware, Inventory, Snapshot};
