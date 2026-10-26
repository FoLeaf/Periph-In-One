//! Battery observations are one optional domain contract, not device-core data.
use crate::{
    Backend, Capability, DEFAULT_OPERATION_TIMEOUT, DeviceFacts, ErrorKind, HardwareError,
};
use std::{
    any::Any,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    time::{Duration, Instant, SystemTime},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Percent(u8);

impl Percent {
    pub fn new(value: u8) -> Result<Self, HardwareError> {
        if value <= 100 {
            Ok(Self(value))
        } else {
            Err(HardwareError::new(
                ErrorKind::InvalidResponse,
                "battery percentage exceeds 100",
            ))
        }
    }

    pub const fn get(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleOrigin {
    DeviceResponse,
    SystemCache,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sample {
    pub level: Percent,
    pub origin: SampleOrigin,
    pub observed_at: SystemTime,
    /// None when the backend cannot establish the device's sampling time.
    pub sampled_at: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatteryState {
    Value(Sample),
    /// A verified device/model lacks this capability, not a failed read.
    Unsupported,
    Unavailable(HardwareError),
}

#[derive(Clone, Debug)]
pub struct BatteryReading {
    pub state: BatteryState,
    pub facts: DeviceFacts,
}

pub trait BatteryReader: Send + Sync {
    fn read(&self) -> Result<BatteryReading, HardwareError>;
}

impl<F> BatteryReader for F
where
    F: Fn() -> Result<BatteryReading, HardwareError> + Send + Sync,
{
    fn read(&self) -> Result<BatteryReading, HardwareError> {
        self()
    }
}

/// An independently sampled capability, valid until rediscovery or shutdown.
#[derive(Clone)]
pub struct Battery {
    operation: Arc<Mutex<Option<Arc<BatteryOperation>>>>,
    valid: Arc<AtomicBool>,
    executor: Option<Arc<dyn BatteryExecutor>>,
}

type LocalBatteryReader =
    dyn Fn(&mut dyn Any) -> Result<BatteryReading, HardwareError> + Send + Sync;

pub(crate) enum BatteryOperation {
    Direct(Arc<dyn BatteryReader>),
    Local(Arc<LocalBatteryReader>),
}

impl BatteryOperation {
    pub(crate) fn read(&self, backend: &mut dyn Any) -> Result<BatteryReading, HardwareError> {
        match self {
            Self::Direct(reader) => reader.read(),
            Self::Local(reader) => reader(backend),
        }
    }

    fn read_without_backend(&self) -> Result<BatteryReading, HardwareError> {
        match self {
            Self::Direct(reader) => reader.read(),
            Self::Local(_) => Err(HardwareError::new(
                ErrorKind::Unavailable,
                "local battery capability requires its backend worker",
            )),
        }
    }
}

pub(crate) trait BatteryExecutor: Send + Sync {
    fn request(
        &self,
        operation: Arc<BatteryOperation>,
        valid: Arc<AtomicBool>,
    ) -> BatteryReadRequest;
}

/// A queued battery operation. Scheduling is nonblocking; callers choose a
/// bounded wait appropriate to their refresh policy.
pub struct BatteryReadRequest {
    receiver: Receiver<Result<BatteryReading, HardwareError>>,
}

impl BatteryReadRequest {
    pub fn wait(self, timeout: Duration) -> Result<BatteryReading, HardwareError> {
        self.wait_inner(timeout, None)
    }

    pub fn wait_cancellable(
        self,
        timeout: Duration,
        cancelled: &AtomicBool,
    ) -> Result<BatteryReading, HardwareError> {
        self.wait_inner(timeout, Some(cancelled))
    }

    fn wait_inner(
        self,
        timeout: Duration,
        cancelled: Option<&AtomicBool>,
    ) -> Result<BatteryReading, HardwareError> {
        let deadline = Instant::now() + timeout;
        loop {
            if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                return Err(HardwareError::new(
                    ErrorKind::Unavailable,
                    "battery operation cancelled",
                ));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let slice = if cancelled.is_some() {
                remaining.min(Duration::from_millis(25))
            } else {
                remaining
            };
            match self.receiver.recv_timeout(slice) {
                Ok(result) => return result,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(HardwareError::new(
                        ErrorKind::Unavailable,
                        "battery operation was cancelled",
                    ));
                }
                Err(RecvTimeoutError::Timeout) if Instant::now() >= deadline => {
                    return Err(HardwareError::new(
                        ErrorKind::Timeout,
                        "battery operation exceeded its deadline",
                    ));
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    pub(crate) fn ready(result: Result<BatteryReading, HardwareError>) -> Self {
        let (sender, receiver) = mpsc::sync_channel(1);
        let _ = sender.send(result);
        Self { receiver }
    }

    pub(crate) fn from_receiver(receiver: Receiver<Result<BatteryReading, HardwareError>>) -> Self {
        Self { receiver }
    }
}

impl Battery {
    pub fn new(reader: impl BatteryReader + 'static) -> Self {
        Self {
            operation: Arc::new(Mutex::new(Some(Arc::new(BatteryOperation::Direct(
                Arc::new(reader),
            ))))),
            valid: Arc::new(AtomicBool::new(true)),
            executor: None,
        }
    }

    /// Define an operation that needs the backend's own thread-local state.
    /// The closure is sent to the worker, while `backend` itself never leaves
    /// the thread where the factory created it. This requires registering the
    /// backend with `Hardware::register_factory`.
    pub fn new_local<B, F>(reader: F) -> Self
    where
        B: Backend + 'static,
        F: Fn(&mut B) -> Result<BatteryReading, HardwareError> + Send + Sync + 'static,
    {
        let reader = Arc::new(move |backend: &mut dyn Any| {
            let backend = backend.downcast_mut::<B>().ok_or_else(|| {
                HardwareError::new(
                    ErrorKind::Conflict,
                    "local battery operation received a different backend type",
                )
            })?;
            reader(backend)
        });
        Self {
            operation: Arc::new(Mutex::new(Some(Arc::new(BatteryOperation::Local(reader))))),
            valid: Arc::new(AtomicBool::new(true)),
            executor: None,
        }
    }

    pub fn read(&self) -> Result<BatteryReading, HardwareError> {
        if self.executor.is_some() {
            return self.request_read().wait(DEFAULT_OPERATION_TIMEOUT);
        }
        self.read_local()
    }

    /// Dispatch a read. Worker-bound capabilities enqueue it without waiting;
    /// the synchronous compatibility path runs it before returning.
    pub fn request_read(&self) -> BatteryReadRequest {
        if !self.valid.load(Ordering::Acquire) {
            return BatteryReadRequest::ready(Err(HardwareError::new(
                ErrorKind::Unavailable,
                "battery capability has expired",
            )));
        }
        let operation = match self.operation.lock() {
            Ok(operation) => operation.clone(),
            Err(_) => {
                return BatteryReadRequest::ready(Err(HardwareError::new(
                    ErrorKind::Unavailable,
                    "battery capability lock poisoned",
                )));
            }
        };
        let Some(operation) = operation else {
            return BatteryReadRequest::ready(Err(HardwareError::new(
                ErrorKind::Unavailable,
                "battery capability has expired",
            )));
        };
        if let Some(executor) = &self.executor {
            executor.request(operation, self.valid.clone())
        } else {
            BatteryReadRequest::ready(operation.read_without_backend())
        }
    }

    fn read_local(&self) -> Result<BatteryReading, HardwareError> {
        if !self.valid.load(Ordering::Acquire) {
            return Err(HardwareError::new(
                ErrorKind::Unavailable,
                "battery capability has expired",
            ));
        }
        let operation = self
            .operation
            .lock()
            .map_err(|_| {
                HardwareError::new(ErrorKind::Unavailable, "battery capability lock poisoned")
            })?
            .clone()
            .ok_or_else(|| {
                HardwareError::new(ErrorKind::Unavailable, "battery capability has expired")
            })?;
        if !self.valid.load(Ordering::Acquire) {
            return Err(HardwareError::new(
                ErrorKind::Unavailable,
                "battery capability has expired",
            ));
        }
        operation.read_without_backend()
    }

    pub(crate) fn routed(&self, executor: Arc<dyn BatteryExecutor>) -> Self {
        Self {
            operation: self.operation.clone(),
            valid: self.valid.clone(),
            executor: Some(executor),
        }
    }

    pub fn close(&self) {
        self.valid.store(false, Ordering::Release);
        if let Ok(mut operation) = self.operation.lock() {
            operation.take();
        }
    }
}

impl Capability for Battery {
    const ID: &'static str = "power.battery";
    const VERSION: u16 = 2;
    fn invalidate(&self) {
        self.close();
    }
}
