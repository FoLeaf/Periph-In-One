//! DPI control domain capability contract.
use crate::{
    BackendExecutor, BackendOperation, Capability, DEFAULT_OPERATION_TIMEOUT, ErrorKind,
    HardwareError,
};
use std::{
    any::Any,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

pub trait DpiController: Send + Sync {
    /// Read the current active DPI stage (1-based index).
    /// Returns `Ok(None)` if the active stage is unverified or outside preset range.
    fn read_stage(&self) -> Result<Option<u8>, HardwareError>;
    /// Switch to the specified DPI stage (1-based index).
    fn set_stage(&self, stage: u8) -> Result<(), HardwareError>;
}

/// Typed capability client for DPI inspection and switching.
#[derive(Clone)]
pub struct Dpi {
    stages: Vec<u32>,
    controller: Arc<Mutex<Option<Arc<dyn DpiController>>>>,
    valid: Arc<AtomicBool>,
    executor: Option<Arc<dyn BackendExecutor>>,
}

impl std::fmt::Debug for Dpi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dpi")
            .field("stages", &self.stages)
            .finish()
    }
}

impl Dpi {
    pub fn new(stages: Vec<u32>, controller: impl DpiController + 'static) -> Self {
        Self {
            stages,
            controller: Arc::new(Mutex::new(Some(Arc::new(controller)))),
            valid: Arc::new(AtomicBool::new(true)),
            executor: None,
        }
    }

    pub fn stages(&self) -> &[u32] {
        &self.stages
    }

    pub fn read_stage(&self) -> Result<Option<u8>, HardwareError> {
        self.read_stage_timeout(DEFAULT_OPERATION_TIMEOUT)
    }

    pub fn read_stage_timeout(&self, timeout: Duration) -> Result<Option<u8>, HardwareError> {
        if !self.valid.load(Ordering::Acquire) {
            return Err(HardwareError::new(
                ErrorKind::Unavailable,
                "dpi capability has expired",
            ));
        }
        let controller = self
            .controller
            .lock()
            .map_err(|_| HardwareError::new(ErrorKind::Unavailable, "dpi controller lock poisoned"))?
            .clone()
            .ok_or_else(|| {
                HardwareError::new(ErrorKind::Unavailable, "dpi capability has expired")
            })?;

        if let Some(executor) = &self.executor {
            let (sender, receiver) = mpsc::sync_channel(1);
            let valid = self.valid.clone();
            let job_executor = executor.clone();
            let run: BackendOperation = Box::new(move |_backend| {
                let result = if !valid.load(Ordering::Acquire) || !job_executor.is_current() {
                    Err(HardwareError::new(
                        ErrorKind::Unavailable,
                        "dpi capability has expired",
                    ))
                } else {
                    controller.read_stage()
                };
                let _ = sender.send(result);
            });
            executor.enqueue(run)?;
            receiver
                .recv_timeout(timeout)
                .map_err(|_| HardwareError::new(ErrorKind::Timeout, "dpi read timed out"))?
        } else {
            controller.read_stage()
        }
    }

    pub fn set_stage(&self, stage: u8) -> Result<(), HardwareError> {
        self.set_stage_timeout(stage, DEFAULT_OPERATION_TIMEOUT)
    }

    pub fn set_stage_timeout(&self, stage: u8, timeout: Duration) -> Result<(), HardwareError> {
        if !self.valid.load(Ordering::Acquire) {
            return Err(HardwareError::new(
                ErrorKind::Unavailable,
                "dpi capability has expired",
            ));
        }
        let controller = self
            .controller
            .lock()
            .map_err(|_| HardwareError::new(ErrorKind::Unavailable, "dpi controller lock poisoned"))?
            .clone()
            .ok_or_else(|| {
                HardwareError::new(ErrorKind::Unavailable, "dpi capability has expired")
            })?;

        if let Some(executor) = &self.executor {
            let (sender, receiver) = mpsc::sync_channel(1);
            let valid = self.valid.clone();
            let job_executor = executor.clone();
            let run: BackendOperation = Box::new(move |_backend| {
                let result = if !valid.load(Ordering::Acquire) || !job_executor.is_current() {
                    Err(HardwareError::new(
                        ErrorKind::Unavailable,
                        "dpi capability has expired",
                    ))
                } else {
                    controller.set_stage(stage)
                };
                let _ = sender.send(result);
            });
            executor.enqueue(run)?;
            receiver
                .recv_timeout(timeout)
                .map_err(|_| HardwareError::new(ErrorKind::Timeout, "dpi write timed out"))?
        } else {
            controller.set_stage(stage)
        }
    }

    pub(crate) fn routed(&self, executor: Arc<dyn BackendExecutor>) -> Self {
        Self {
            stages: self.stages.clone(),
            controller: self.controller.clone(),
            valid: self.valid.clone(),
            executor: Some(executor),
        }
    }

    pub fn close(&self) {
        self.valid.store(false, Ordering::Release);
        if let Ok(mut controller) = self.controller.lock() {
            controller.take();
        }
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
        Some(Arc::new(self.routed(executor)))
    }
}
