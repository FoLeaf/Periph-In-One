use crate::{Device, DeviceDescriptor, ErrorKind, HardwareError};

/// A polling/discovery backend. Continuous domain data belongs to a typed
/// capability session, not this inventory/snapshot interface.
pub trait Backend {
    fn id(&self) -> &str;
    fn inventory(&mut self) -> Result<Vec<DeviceDescriptor>, HardwareError>;
    fn scan(&mut self) -> Result<Vec<Device>, HardwareError>;
}

#[derive(Clone, Debug)]
pub struct BackendFailure {
    pub backend: String,
    pub error: HardwareError,
}

#[derive(Default, Debug)]
pub struct Inventory {
    pub devices: Vec<DeviceDescriptor>,
    /// Only these sources completed an authoritative inventory this round.
    pub completed: Vec<String>,
    pub failures: Vec<BackendFailure>,
}

#[derive(Default, Debug)]
pub struct Snapshot {
    pub devices: Vec<Device>,
    pub completed: Vec<String>,
    pub failures: Vec<BackendFailure>,
}

/// Registration and failure isolation. Own this on the worker that owns its
/// backends; platform handles do not have to be Send or leave that worker.
#[derive(Default)]
pub struct Hardware {
    backends: Vec<Box<dyn Backend>>,
}

impl Hardware {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, backend: impl Backend + 'static) -> Result<(), HardwareError> {
        if backend.id().is_empty() || self.backends.iter().any(|item| item.id() == backend.id()) {
            return Err(HardwareError::new(
                ErrorKind::Conflict,
                "backend ID must be unique and nonempty",
            ));
        }
        self.backends.push(Box::new(backend));
        Ok(())
    }

    pub fn inventory(&mut self) -> Inventory {
        let mut result = Inventory::default();
        for backend in &mut self.backends {
            let id = backend.id().to_owned();
            match backend.inventory() {
                Ok(devices) => {
                    result.devices.extend(devices);
                    result.completed.push(id);
                }
                Err(error) => result.failures.push(BackendFailure { backend: id, error }),
            }
        }
        result
    }

    pub fn scan(&mut self) -> Snapshot {
        let mut result = Snapshot::default();
        for backend in &mut self.backends {
            let id = backend.id().to_owned();
            match backend.scan() {
                Ok(devices) => {
                    result.devices.extend(devices);
                    result.completed.push(id);
                }
                Err(error) => result.failures.push(BackendFailure { backend: id, error }),
            }
        }
        result
    }
}
