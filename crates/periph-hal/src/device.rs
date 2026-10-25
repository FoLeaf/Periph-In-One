use crate::{Capabilities, HardwareError};

/// An access route, not a permanent identity or an application record ID.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DeviceKey {
    pub backend: String,
    pub local: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Connection {
    Connected,
    Disconnected,
    #[default]
    Unknown,
}

/// The namespace matters: USB vendor numbers and Bluetooth company numbers
/// are different registries even when the numeric values happen to match.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct HardwareIdentifier {
    pub namespace: String,
    pub vendor: u16,
    pub product: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum IdentityKind {
    UnitId,
    Serial,
    Address,
    Location,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Identity {
    pub namespace: String,
    pub kind: IdentityKind,
    /// E.g. "device" or "receiver". A receiver serial is not a peripheral UID.
    pub subject: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Link {
    pub transport: String,
    pub protocol: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DeviceDescriptor {
    pub key: DeviceKey,
    pub name: String,
    /// Extensible classification only; capabilities determine available APIs.
    pub roles: Vec<String>,
    pub identifiers: Vec<HardwareIdentifier>,
    pub identities: Vec<Identity>,
    pub links: Vec<Link>,
    /// Stable logical target within a shared endpoint (e.g. a receiver slot).
    /// Qualifies identities whose subject is an enclosing device/receiver.
    #[cfg_attr(feature = "serde", serde(default))]
    pub target: String,
}

#[derive(Clone, Debug)]
pub struct Device {
    pub descriptor: DeviceDescriptor,
    pub connection: Connection,
    pub capabilities: Capabilities,
    pub issue: Option<HardwareError>,
}
