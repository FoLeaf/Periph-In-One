use crate::{
    ErrorKind, HardwareError,
    battery::{Battery, BatteryExecutor},
};
use std::{any::Any, collections::BTreeMap, fmt, sync::Arc};

/// Implement in a capability module or downstream crate, without changing core.
/// Values may be immutable observations or typed clients for separate sessions.
pub trait Capability: Any + Send + Sync {
    const ID: &'static str;
    const VERSION: u16 = 1;
    /// Revoke live clients before rediscovery or backend shutdown.
    fn invalidate(&self) {}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityDescriptor {
    pub id: &'static str,
    pub version: u16,
    /// Source-local component key. An empty key addresses the device itself.
    pub component: String,
}

#[derive(Clone)]
struct Entry {
    descriptor: CapabilityDescriptor,
    value: Arc<dyn Any + Send + Sync>,
    invalidate: fn(&(dyn Any + Send + Sync)),
}

/// Type erasure is confined here. Consumers only register/get concrete contracts;
/// neither raw Any values nor string-dispatched commands cross this API.
#[derive(Clone, Default)]
pub struct Capabilities {
    entries: BTreeMap<(&'static str, u16, String), Entry>,
}

impl Capabilities {
    pub fn insert<C: Capability>(
        &mut self,
        component: impl Into<String>,
        value: C,
    ) -> Result<(), HardwareError> {
        let component = component.into();
        let key = (C::ID, C::VERSION, component.clone());
        if C::ID.is_empty() || C::VERSION == 0 {
            return Err(HardwareError::new(
                ErrorKind::InvalidResponse,
                "invalid capability contract",
            ));
        }
        if self.entries.contains_key(&key) {
            return Err(HardwareError::new(
                ErrorKind::Conflict,
                "duplicate capability instance",
            ));
        }
        self.entries.insert(
            key,
            Entry {
                descriptor: CapabilityDescriptor {
                    id: C::ID,
                    version: C::VERSION,
                    component,
                },
                value: Arc::new(value),
                invalidate: |value| {
                    if let Some(value) = value.downcast_ref::<C>() {
                        value.invalidate();
                    }
                },
            },
        );
        Ok(())
    }

    pub fn get<C: Capability>(&self, component: &str) -> Option<&C> {
        self.entries
            .get(&(C::ID, C::VERSION, component.to_owned()))?
            .value
            .downcast_ref()
    }

    pub fn descriptors(&self) -> impl Iterator<Item = &CapabilityDescriptor> {
        self.entries.values().map(|entry| &entry.descriptor)
    }

    pub(crate) fn invalidate(&self) {
        for entry in self.entries.values() {
            (entry.invalidate)(entry.value.as_ref());
        }
    }

    pub(crate) fn route_batteries(&mut self, executor: Arc<dyn BatteryExecutor>) {
        for entry in self.entries.values_mut() {
            let routed = entry.value.downcast_ref::<Battery>().map(|battery| {
                Arc::new(battery.routed(executor.clone())) as Arc<dyn Any + Send + Sync>
            });
            if let Some(routed) = routed {
                entry.value = routed;
            }
        }
    }
}

impl fmt::Debug for Capabilities {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.descriptors()).finish()
    }
}
