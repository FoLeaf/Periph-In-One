use crate::{Capabilities, HardwareError};

/// An access route, not a permanent identity or an application record ID.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DeviceKey {
    pub backend: String,
    pub local: String,
}

/// A local access endpoint groups one or more logical devices reached through
/// the same physical or protocol endpoint. It is a route scope, not identity.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct EndpointKey {
    pub backend: String,
    pub local: String,
}

impl EndpointKey {
    pub fn for_device(key: &DeviceKey) -> Self {
        Self {
            backend: key.backend.clone(),
            local: key.local.clone(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.backend.is_empty() || self.local.is_empty()
    }
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
    /// Endpoint shared by logical devices, such as slots behind one receiver.
    /// Empty on old serialized descriptors; callers use the route as fallback.
    #[cfg_attr(feature = "serde", serde(default))]
    pub endpoint: EndpointKey,
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

/// Facts established during a capability operation, independent of its domain.
#[derive(Clone, Debug, Default)]
pub struct DeviceFacts {
    pub connection: Option<Connection>,
    pub name: Option<String>,
    pub identities: Vec<Identity>,
}

impl DeviceFacts {
    pub fn apply_to(&self, device: &mut Device) {
        if let Some(connection) = self.connection {
            device.connection = connection;
        }
        if let Some(name) = &self.name {
            device.descriptor.name = name.clone();
        }
        for identity in &self.identities {
            device.descriptor.identities.retain(|old| {
                !(old.namespace == identity.namespace
                    && old.kind == identity.kind
                    && old.subject == identity.subject)
            });
            device.descriptor.identities.push(identity.clone());
        }
    }
}

impl DeviceDescriptor {
    pub fn access_endpoint(&self) -> EndpointKey {
        if self.endpoint.is_empty() {
            EndpointKey::for_device(&self.key)
        } else {
            self.endpoint.clone()
        }
    }
}
