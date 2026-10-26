use crate::{ErrorKind, HardwareError};
use std::{any::Any, collections::BTreeMap, fmt, sync::Arc};

type CapabilityValue = dyn Any + Send + Sync;
type ExecutorBinder =
    fn(&CapabilityValue, Arc<dyn BackendExecutor>) -> Option<Arc<CapabilityValue>>;

/// A typed domain task sent to the backend's own worker. The type-erased
/// boundary stays inside the HAL and capability implementation.
pub type BackendOperation = Box<dyn FnOnce(&mut dyn Any) + Send>;

pub trait BackendExecutor: Send + Sync {
    fn enqueue(&self, operation: BackendOperation) -> Result<(), HardwareError>;
    fn is_current(&self) -> bool;
}

/// Implement in a capability module or downstream crate, without changing core.
/// Values may be immutable observations or typed clients for separate sessions.
pub trait Capability: Any + Send + Sync {
    const ID: &'static str;
    const VERSION: u16 = 1;
    /// Revoke live clients before rediscovery or backend shutdown.
    fn invalidate(&self) {}
    /// Bind a live client to its owner thread. Immutable capabilities keep the
    /// default implementation.
    fn bind_executor(
        &self,
        _executor: Arc<dyn BackendExecutor>,
    ) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }
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
    value: Arc<CapabilityValue>,
    invalidate: fn(&CapabilityValue),
    bind_executor: ExecutorBinder,
}

fn bind<C: Capability>(
    value: &(dyn Any + Send + Sync),
    executor: Arc<dyn BackendExecutor>,
) -> Option<Arc<dyn Any + Send + Sync>> {
    value.downcast_ref::<C>()?.bind_executor(executor)
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
                bind_executor: bind::<C>,
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

    pub(crate) fn bind_executor(&mut self, executor: Arc<dyn BackendExecutor>) {
        for entry in self.entries.values_mut() {
            if let Some(bound) = (entry.bind_executor)(entry.value.as_ref(), executor.clone()) {
                entry.value = bound;
            }
        }
    }
}

impl fmt::Debug for Capabilities {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.descriptors()).finish()
    }
}
