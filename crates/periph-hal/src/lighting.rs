//! Lighting control domain capability contract.
use crate::{
    BackendExecutor, Capability, DEFAULT_OPERATION_TIMEOUT, HardwareError,
    operation::{ControllerClient, OperationRequest},
};
use std::{any::Any, sync::Arc, time::Duration};

pub trait LightingController: Send + Sync {
    fn read_state(&self) -> Result<bool, HardwareError>;
    fn set_state(&self, on: bool) -> Result<(), HardwareError>;
}

/// Typed capability client for lighting state inspection and toggle.
#[derive(Clone)]
pub struct Lighting {
    client: ControllerClient<dyn LightingController>,
}

impl std::fmt::Debug for Lighting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lighting").finish()
    }
}

impl Lighting {
    pub fn new(controller: impl LightingController + 'static) -> Self {
        Self {
            client: ControllerClient::new(Arc::new(controller)),
        }
    }
    /// Enqueue without waiting on worker-bound clients; direct clients run
    /// synchronously for compatibility with standalone controllers.
    pub fn request_read(&self) -> OperationRequest<bool> {
        self.client.request(|controller| controller.read_state())
    }
    pub fn read_state(&self) -> Result<bool, HardwareError> {
        self.read_state_timeout(DEFAULT_OPERATION_TIMEOUT)
    }
    pub fn read_state_timeout(&self, timeout: Duration) -> Result<bool, HardwareError> {
        self.request_read().wait(timeout)
    }
    pub fn set_state(&self, on: bool) -> Result<(), HardwareError> {
        self.set_state_timeout(on, DEFAULT_OPERATION_TIMEOUT)
    }
    pub fn set_state_timeout(&self, on: bool, timeout: Duration) -> Result<(), HardwareError> {
        self.client
            .request(move |controller| controller.set_state(on))
            .wait(timeout)
    }
    pub fn close(&self) {
        self.client.close();
    }
}

impl Capability for Lighting {
    const ID: &'static str = "control.lighting";
    const VERSION: u16 = 1;
    fn invalidate(&self) {
        self.close();
    }
    fn bind_executor(
        &self,
        executor: Arc<dyn BackendExecutor>,
    ) -> Option<Arc<dyn Any + Send + Sync>> {
        Some(Arc::new(Self {
            client: self.client.routed(executor),
        }))
    }
}
