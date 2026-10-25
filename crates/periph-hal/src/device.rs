use crate::{Capabilities, HardwareError};

/// An access route, not a permanent identity or an application record ID.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DeviceKey {
    pub backend: String,
    pub local: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connection {
    Connected,
    Disconnected,
    Unknown,
}

/// The namespace matters: USB vendor numbers and Bluetooth company numbers
/// are different registries even when the numeric values happen to match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HardwareIdentifier {
    pub namespace: String,
    pub vendor: u16,
    pub product: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum IdentityKind {
    UnitId,
    Serial,
    Address,
    Location,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub namespace: String,
    pub kind: IdentityKind,
    /// E.g. "device" or "receiver". A receiver serial is not a peripheral UID.
    pub subject: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub transport: String,
    pub protocol: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceDescriptor {
    pub key: DeviceKey,
    pub name: String,
    /// Extensible classification only; capabilities determine available APIs.
    pub roles: Vec<String>,
    pub identifiers: Vec<HardwareIdentifier>,
    pub identities: Vec<Identity>,
    pub links: Vec<Link>,
}

#[derive(Clone, Debug)]
pub struct Device {
    pub descriptor: DeviceDescriptor,
    pub connection: Connection,
    pub capabilities: Capabilities,
    pub issue: Option<HardwareError>,
}
