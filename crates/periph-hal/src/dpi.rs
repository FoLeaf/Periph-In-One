//! DPI control domain capability contract.
use crate::{
    BackendExecutor, Capability, DEFAULT_OPERATION_TIMEOUT, ErrorKind, HardwareError,
    operation::{ControllerClient, OperationRequest},
};
use std::{any::Any, sync::Arc, time::Duration};

pub trait DpiController: Send + Sync {
    /// Read the active stage (1-based); None when it is unverified.
    fn read_stage(&self) -> Result<Option<u8>, HardwareError>;
    fn set_stage(&self, stage: u8) -> Result<(), HardwareError>;
}

#[derive(Clone)]
pub struct Dpi {
    stages: Vec<u32>,
    client: ControllerClient<dyn DpiController>,
}

impl std::fmt::Debug for Dpi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dpi").field("stages", &self.stages).finish()
    }
}

impl Dpi {
    pub fn new(stages: Vec<u32>, controller: impl DpiController + 'static) -> Self {
        Self {
            stages,
            client: ControllerClient::new(Arc::new(controller)),
        }
    }
    pub fn stages(&self) -> &[u32] {
        &self.stages
    }
    /// Enqueue without waiting on worker-bound clients; direct clients run
    /// synchronously for compatibility with standalone controllers.
    pub fn request_read(&self) -> OperationRequest<Option<u8>> {
        self.client.request(|controller| controller.read_stage())
    }
    pub fn read_stage(&self) -> Result<Option<u8>, HardwareError> {
        self.read_stage_timeout(DEFAULT_OPERATION_TIMEOUT)
    }
    pub fn read_stage_timeout(&self, timeout: Duration) -> Result<Option<u8>, HardwareError> {
        self.request_read().wait(timeout)
    }
    pub fn set_stage(&self, stage: u8) -> Result<(), HardwareError> {
        self.set_stage_timeout(stage, DEFAULT_OPERATION_TIMEOUT)
    }
    pub fn set_stage_timeout(&self, stage: u8, timeout: Duration) -> Result<(), HardwareError> {
        if stage == 0 || usize::from(stage) > self.stages.len() {
            return Err(HardwareError::new(
                ErrorKind::Unsupported,
                "DPI stage outside preset range",
            ));
        }
        self.client
            .request(move |controller| controller.set_stage(stage))
            .wait(timeout)
    }
    pub fn close(&self) {
        self.client.close();
    }
}

impl Capability for Dpi {
    const ID: &'static str = "control.dpi";
    const VERSION: u16 = 1;
    fn invalidate(&self) {
        self.close();
    }
    fn bind_executor(
        &self,
        executor: Arc<dyn BackendExecutor>,
    ) -> Option<Arc<dyn Any + Send + Sync>> {
        Some(Arc::new(Self {
            stages: self.stages.clone(),
            client: self.client.routed(executor),
        }))
    }
}
