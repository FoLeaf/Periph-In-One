//! Lighting control domain capability contract.
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

pub trait LightingController: Send + Sync {
    /// Read whether the lighting is currently active.
    fn read_state(&self) -> Result<bool, HardwareError>;
    /// Switch lighting on or off.
    fn set_state(&self, on: bool) -> Result<(), HardwareError>;
}

/// Typed capability client for lighting state inspection and toggle.
#[derive(Clone)]
pub struct Lighting {
    controller: Arc<Mutex<Option<Arc<dyn LightingController>>>>,
    valid: Arc<AtomicBool>,
    executor: Option<Arc<dyn BackendExecutor>>,
}

impl std::fmt::Debug for Lighting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lighting").finish()
    }
}

impl Lighting {
    pub fn new(controller: impl LightingController + 'static) -> Self {
        Self {
            controller: Arc::new(Mutex::new(Some(Arc::new(controller)))),
            valid: Arc::new(AtomicBool::new(true)),
            executor: None,
        }
    }

    pub fn read_state(&self) -> Result<bool, HardwareError> {
        self.read_state_timeout(DEFAULT_OPERATION_TIMEOUT)
    }

    pub fn read_state_timeout(&self, timeout: Duration) -> Result<bool, HardwareError> {
        if !self.valid.load(Ordering::Acquire) {
            return Err(HardwareError::new(
                ErrorKind::Unavailable,
                "lighting capability has expired",
            ));
        }
        let controller = self
            .controller
            .lock()
            .map_err(|_| {
                HardwareError::new(ErrorKind::Unavailable, "lighting controller lock poisoned")
            })?
            .clone()
            .ok_or_else(|| {
                HardwareError::new(ErrorKind::Unavailable, "lighting capability has expired")
            })?;

        if let Some(executor) = &self.executor {
            let (sender, receiver) = mpsc::sync_channel(1);
            let valid = self.valid.clone();
            let job_executor = executor.clone();
            let run: BackendOperation = Box::new(move |_backend| {
                let result = if !valid.load(Ordering::Acquire) || !job_executor.is_current() {
                    Err(HardwareError::new(
                        ErrorKind::Unavailable,
                        "lighting capability has expired",
                    ))
                } else {
                    controller.read_state()
                };
                let _ = sender.send(result);
            });
            executor.enqueue(run)?;
            receiver
                .recv_timeout(timeout)
                .map_err(|_| HardwareError::new(ErrorKind::Timeout, "lighting read timed out"))?
        } else {
            controller.read_state()
        }
    }

    pub fn set_state(&self, on: bool) -> Result<(), HardwareError> {
        self.set_state_timeout(on, DEFAULT_OPERATION_TIMEOUT)
    }

    pub fn set_state_timeout(&self, on: bool, timeout: Duration) -> Result<(), HardwareError> {
        if !self.valid.load(Ordering::Acquire) {
            return Err(HardwareError::new(
                ErrorKind::Unavailable,
                "lighting capability has expired",
            ));
        }
        let controller = self
            .controller
            .lock()
            .map_err(|_| {
                HardwareError::new(ErrorKind::Unavailable, "lighting controller lock poisoned")
            })?
            .clone()
            .ok_or_else(|| {
                HardwareError::new(ErrorKind::Unavailable, "lighting capability has expired")
            })?;

        if let Some(executor) = &self.executor {
            let (sender, receiver) = mpsc::sync_channel(1);
            let valid = self.valid.clone();
            let job_executor = executor.clone();
            let run: BackendOperation = Box::new(move |_backend| {
                let result = if !valid.load(Ordering::Acquire) || !job_executor.is_current() {
                    Err(HardwareError::new(
                        ErrorKind::Unavailable,
                        "lighting capability has expired",
                    ))
                } else {
                    controller.set_state(on)
                };
                let _ = sender.send(result);
            });
            executor.enqueue(run)?;
            receiver
                .recv_timeout(timeout)
                .map_err(|_| HardwareError::new(ErrorKind::Timeout, "lighting write timed out"))?
        } else {
            controller.set_state(on)
        }
    }

    pub(crate) fn routed(&self, executor: Arc<dyn BackendExecutor>) -> Self {
        Self {
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
        Some(Arc::new(self.routed(executor)))
    }
}
