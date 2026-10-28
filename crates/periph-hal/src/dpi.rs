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
    can_write: bool,
    client: ControllerClient<dyn DpiController>,
}

impl std::fmt::Debug for Dpi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dpi")
            .field("stages", &self.stages)
            .field("can_write", &self.can_write)
            .finish()
    }
}

impl Dpi {
    pub fn new(stages: Vec<u32>, controller: impl DpiController + 'static) -> Self {
        Self::with_writable(stages, true, controller)
    }

    pub fn with_writable(
        stages: Vec<u32>,
        can_write: bool,
        controller: impl DpiController + 'static,
    ) -> Self {
        Self {
            stages,
            can_write,
            client: ControllerClient::new(Arc::new(controller)),
        }
    }

    pub fn stages(&self) -> &[u32] {
        &self.stages
    }

    pub fn can_write(&self) -> bool {
        self.can_write
    }

    /// Map an absolute DPI value to a 1-based stage if it matches one of the preset stages.
    pub fn stage_for_value(&self, value: u32) -> Option<u8> {
        self.stages
            .iter()
            .position(|&s| s == value)
            .map(|idx| (idx + 1) as u8)
    }

    /// Get the preset DPI value for a 1-based stage.
    pub fn value_for_stage(&self, stage: u8) -> Option<u32> {
        if stage == 0 {
            return None;
        }
        self.stages.get(usize::from(stage - 1)).copied()
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
        if !self.can_write {
            return Err(HardwareError::new(
                ErrorKind::Unsupported,
                "DPI writing is not supported on this device",
            ));
        }
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
            can_write: self.can_write,
            client: self.client.routed(executor),
        }))
    }
}
