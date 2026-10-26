use crate::{
    Capabilities, DEFAULT_OPERATION_TIMEOUT, Device, DeviceDescriptor, ErrorKind, HardwareError,
    battery::{BatteryExecutor, BatteryOperation, BatteryReadRequest},
};
use std::{
    any::Any,
    collections::{BTreeMap, VecDeque},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Condvar, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

const MAX_PENDING_OPERATIONS: usize = 128;
type WorkerOperation = Box<dyn FnOnce(&mut dyn Any) + Send>;

/// A polling/discovery backend. Continuous domain data belongs to a typed
/// capability session, not this inventory/snapshot interface.
pub trait Backend: Any {
    fn id(&self) -> &str;
    fn inventory(&mut self) -> Result<Vec<DeviceDescriptor>, HardwareError>;
    /// Return device facts and clients without sampling their capabilities.
    fn discover(&mut self) -> Result<Vec<Device>, HardwareError>;
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

#[derive(Clone, Copy)]
enum QueryKind {
    Inventory,
    Discover,
}

enum Reply {
    Inventory(Result<Vec<DeviceDescriptor>, HardwareError>),
    Discover(Result<Vec<Device>, HardwareError>),
}

enum Task {
    Query {
        generation: u64,
        kind: QueryKind,
        reply: SyncSender<Reply>,
    },
    Capability {
        generation: u64,
        run: WorkerOperation,
    },
}

#[derive(Default)]
struct QueueState {
    tasks: VecDeque<Task>,
    closing: bool,
}

struct WorkerShared {
    state: Mutex<QueueState>,
    changed: Condvar,
    generation: AtomicU64,
}

impl WorkerShared {
    fn new() -> Self {
        Self {
            state: Mutex::new(QueueState::default()),
            changed: Condvar::new(),
            generation: AtomicU64::new(0),
        }
    }

    fn query(&self, generation: u64, kind: QueryKind) -> Result<Receiver<Reply>, HardwareError> {
        let (reply, receiver) = mpsc::sync_channel(1);
        let mut state = self.state.lock().map_err(|_| {
            HardwareError::new(ErrorKind::Unavailable, "backend operation queue poisoned")
        })?;
        if state.closing {
            return Err(HardwareError::new(
                ErrorKind::Unavailable,
                "backend worker is shutting down",
            ));
        }
        self.generation.store(generation, Ordering::Release);
        // A new round supersedes queued work. An already-running native call
        // cannot be safely interrupted, but its result is generation-checked.
        state.tasks.clear();
        state.tasks.push_back(Task::Query {
            generation,
            kind,
            reply,
        });
        self.changed.notify_one();
        Ok(receiver)
    }

    fn capability(&self, generation: u64, run: WorkerOperation) -> Result<(), HardwareError> {
        let mut state = self.state.lock().map_err(|_| {
            HardwareError::new(ErrorKind::Unavailable, "backend operation queue poisoned")
        })?;
        if state.closing || self.generation.load(Ordering::Acquire) != generation {
            return Err(HardwareError::new(
                ErrorKind::Unavailable,
                "capability belongs to an expired discovery round",
            ));
        }
        if state.tasks.len() >= MAX_PENDING_OPERATIONS {
            return Err(HardwareError::new(
                ErrorKind::Unavailable,
                "backend operation queue is full",
            ));
        }
        state.tasks.push_back(Task::Capability { generation, run });
        self.changed.notify_one();
        Ok(())
    }

    fn next(&self) -> Option<Task> {
        let mut state = self.state.lock().ok()?;
        loop {
            if state.closing {
                return None;
            }
            if let Some(task) = state.tasks.pop_front() {
                return Some(task);
            }
            state = self.changed.wait(state).ok()?;
        }
    }

    fn close(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.closing = true;
            state.tasks.clear();
            self.changed.notify_all();
        }
    }
}

struct BackendWorker {
    shared: Arc<WorkerShared>,
}

struct WorkerBatteryExecutor {
    shared: Weak<WorkerShared>,
    generation: u64,
}

impl BatteryExecutor for WorkerBatteryExecutor {
    fn request(
        &self,
        operation: Arc<BatteryOperation>,
        valid: Arc<std::sync::atomic::AtomicBool>,
    ) -> BatteryReadRequest {
        let Some(shared) = self.shared.upgrade() else {
            return BatteryReadRequest::ready(Err(HardwareError::new(
                ErrorKind::Unavailable,
                "backend worker has stopped",
            )));
        };
        let (reply, receiver) = mpsc::sync_channel(1);
        let job_shared = shared.clone();
        let generation = self.generation;
        let run = Box::new(move |backend: &mut dyn Any| {
            let result = if !valid.load(Ordering::Acquire)
                || job_shared.generation.load(Ordering::Acquire) != generation
            {
                Err(HardwareError::new(
                    ErrorKind::Unavailable,
                    "battery capability has expired",
                ))
            } else {
                catch_unwind(AssertUnwindSafe(|| operation.read(backend))).unwrap_or_else(|_| {
                    Err(HardwareError::new(
                        ErrorKind::Unavailable,
                        "battery capability panicked",
                    ))
                })
            };
            let result = if valid.load(Ordering::Acquire)
                && job_shared.generation.load(Ordering::Acquire) == generation
            {
                result
            } else {
                Err(HardwareError::new(
                    ErrorKind::Unavailable,
                    "battery result belongs to an expired discovery round",
                ))
            };
            let _ = reply.send(result);
        });
        if let Err(error) = shared.capability(self.generation, run) {
            return BatteryReadRequest::ready(Err(error));
        }
        BatteryReadRequest::from_receiver(receiver)
    }
}

impl BackendWorker {
    fn spawn<B, F>(id: String, factory: F) -> Result<Self, HardwareError>
    where
        B: Backend + 'static,
        F: FnOnce() -> B + Send + 'static,
    {
        let shared = Arc::new(WorkerShared::new());
        let thread_shared = shared.clone();
        let expected_id = id.clone();
        thread::Builder::new()
            .name(format!("periph-{}", id.replace(['.', ':', '/'], "-")))
            .spawn(move || {
                let initialized = catch_unwind(AssertUnwindSafe(factory)).map_err(|_| {
                    HardwareError::new(ErrorKind::Unavailable, "backend factory panicked")
                });
                let (mut backend, startup_error): (
                    Option<Box<dyn Backend>>,
                    Option<HardwareError>,
                ) = match initialized {
                    Err(error) => (None, Some(error)),
                    Ok(backend) if backend.id() != expected_id => (
                        None,
                        Some(HardwareError::new(
                            ErrorKind::Conflict,
                            "backend factory returned a mismatched ID",
                        )),
                    ),
                    Ok(backend) => (Some(Box::new(backend)), None),
                };

                while let Some(task) = thread_shared.next() {
                    match task {
                        Task::Query {
                            generation,
                            kind,
                            reply,
                        } => {
                            if thread_shared.generation.load(Ordering::Acquire) != generation {
                                continue;
                            }
                            let result = if let Some(error) = &startup_error {
                                Err(error.clone())
                            } else if let Some(backend) = backend.as_mut() {
                                match kind {
                                    QueryKind::Inventory => {
                                        catch_unwind(AssertUnwindSafe(|| backend.inventory()))
                                            .unwrap_or_else(|_| {
                                                Err(HardwareError::new(
                                                    ErrorKind::Unavailable,
                                                    "backend inventory panicked",
                                                ))
                                            })
                                            .map(|devices| Reply::Inventory(Ok(devices)))
                                    }
                                    QueryKind::Discover => {
                                        catch_unwind(AssertUnwindSafe(|| backend.discover()))
                                            .unwrap_or_else(|_| {
                                                Err(HardwareError::new(
                                                    ErrorKind::Unavailable,
                                                    "backend discovery panicked",
                                                ))
                                            })
                                            .map(|mut devices| {
                                                let executor: Arc<dyn BatteryExecutor> =
                                                    Arc::new(WorkerBatteryExecutor {
                                                        shared: Arc::downgrade(&thread_shared),
                                                        generation,
                                                    });
                                                for device in &mut devices {
                                                    device
                                                        .capabilities
                                                        .route_batteries(executor.clone());
                                                }
                                                Reply::Discover(Ok(devices))
                                            })
                                    }
                                }
                            } else {
                                Err(HardwareError::new(
                                    ErrorKind::Unavailable,
                                    "backend worker is unavailable",
                                ))
                            };

                            let is_current =
                                thread_shared.generation.load(Ordering::Acquire) == generation;
                            let result = if is_current {
                                result
                            } else {
                                if let Ok(Reply::Discover(Ok(devices))) = &result {
                                    for device in devices {
                                        device.capabilities.invalidate();
                                    }
                                }
                                Err(HardwareError::new(
                                    ErrorKind::Unavailable,
                                    "backend result belongs to an expired discovery round",
                                ))
                            };
                            let reply_value = match (kind, result) {
                                (QueryKind::Inventory, Ok(Reply::Inventory(result))) => {
                                    Reply::Inventory(result)
                                }
                                (QueryKind::Discover, Ok(Reply::Discover(result))) => {
                                    Reply::Discover(result)
                                }
                                (QueryKind::Inventory, Err(error)) => Reply::Inventory(Err(error)),
                                (QueryKind::Discover, Err(error)) => Reply::Discover(Err(error)),
                                _ => Reply::Inventory(Err(HardwareError::new(
                                    ErrorKind::InvalidResponse,
                                    "backend worker returned the wrong operation result",
                                ))),
                            };
                            if let Err(disconnected) = reply.send(reply_value) {
                                invalidate_reply(disconnected.0);
                            }
                        }
                        Task::Capability { generation, run } => {
                            if thread_shared.generation.load(Ordering::Acquire) == generation
                                && let Some(backend) = backend.as_mut()
                            {
                                run(backend.as_mut() as &mut dyn Any);
                            }
                        }
                    }
                }
            })
            .map_err(HardwareError::transport)?;
        Ok(Self { shared })
    }

    fn query(&self, generation: u64, kind: QueryKind) -> Result<Receiver<Reply>, HardwareError> {
        self.shared.query(generation, kind)
    }

    fn close(&self) {
        self.shared.close();
    }
}

fn invalidate_reply(reply: Reply) {
    if let Reply::Discover(Ok(devices)) = reply {
        for device in devices {
            device.capabilities.invalidate();
        }
    }
}

enum Runner {
    Inline(Box<dyn Backend>),
    Worker(BackendWorker),
}

struct Registration {
    id: String,
    runner: Runner,
}

/// Registration and failure isolation. Prefer `register_factory`: it creates
/// each backend on its own worker, so native handles never need to be `Send`.
/// `register` remains as a synchronous compatibility path for existing callers.
pub struct Hardware {
    backends: Vec<Registration>,
    live: BTreeMap<String, Vec<Capabilities>>,
    generation: u64,
    operation_timeout: Duration,
}

impl Default for Hardware {
    fn default() -> Self {
        Self {
            backends: Vec::new(),
            live: BTreeMap::new(),
            generation: 0,
            operation_timeout: DEFAULT_OPERATION_TIMEOUT,
        }
    }
}

impl Hardware {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set one shared deadline for the concurrently scheduled backend
    /// discovery/inventory calls. This also controls app capability waits.
    pub fn set_operation_timeout(&mut self, timeout: Duration) {
        self.operation_timeout = timeout.max(Duration::from_millis(1));
    }

    pub fn operation_timeout(&self) -> Duration {
        self.operation_timeout
    }

    fn has_id(&self, id: &str) -> bool {
        self.backends.iter().any(|item| item.id == id)
    }

    /// Register a legacy backend instance for synchronous calls on the caller.
    /// Use `register_factory` for isolation, local operations, and deadlines.
    pub fn register(&mut self, backend: impl Backend + 'static) -> Result<(), HardwareError> {
        let id = backend.id().to_owned();
        if id.is_empty() || self.has_id(&id) {
            return Err(HardwareError::new(
                ErrorKind::Conflict,
                "backend ID must be unique and nonempty",
            ));
        }
        self.backends.push(Registration {
            id,
            runner: Runner::Inline(Box::new(backend)),
        });
        Ok(())
    }

    /// Register a backend factory. The factory runs once on a dedicated
    /// worker; its return value may contain thread-affine, non-`Send` state.
    pub fn register_factory<B, F>(
        &mut self,
        id: impl Into<String>,
        factory: F,
    ) -> Result<(), HardwareError>
    where
        B: Backend + 'static,
        F: FnOnce() -> B + Send + 'static,
    {
        let id = id.into();
        if id.is_empty() || self.has_id(&id) {
            return Err(HardwareError::new(
                ErrorKind::Conflict,
                "backend ID must be unique and nonempty",
            ));
        }
        let worker = BackendWorker::spawn(id.clone(), factory)?;
        self.backends.push(Registration {
            id,
            runner: Runner::Worker(worker),
        });
        Ok(())
    }

    fn next_generation(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1).max(1);
        self.generation
    }

    pub fn inventory(&mut self) -> Inventory {
        let generation = self.next_generation();
        let deadline = Instant::now() + self.operation_timeout;
        let mut pending = Vec::<(String, Result<Receiver<Reply>, Reply>)>::new();
        for registration in &mut self.backends {
            match &mut registration.runner {
                Runner::Worker(worker) => pending.push((
                    registration.id.clone(),
                    worker
                        .query(generation, QueryKind::Inventory)
                        .map_err(|error| Reply::Inventory(Err(error))),
                )),
                Runner::Inline(backend) => pending.push((
                    registration.id.clone(),
                    Ok(ready_receiver(Reply::Inventory(backend.inventory()))),
                )),
            }
        }
        let mut result = Inventory::default();
        for (id, response) in pending {
            match receive_reply(response, deadline, None) {
                Reply::Inventory(Ok(devices)) => {
                    result.devices.extend(devices);
                    result.completed.push(id);
                }
                Reply::Inventory(Err(error)) | Reply::Discover(Err(error)) => {
                    result.failures.push(BackendFailure { backend: id, error })
                }
                _ => result.failures.push(BackendFailure {
                    backend: id,
                    error: HardwareError::new(
                        ErrorKind::InvalidResponse,
                        "backend worker returned the wrong operation result",
                    ),
                }),
            }
        }
        result
    }

    pub fn discover(&mut self) -> Snapshot {
        self.discover_inner(None)
    }

    /// Discover while allowing a caller to stop waiting without joining or
    /// interrupting a native operation already running on a backend worker.
    pub fn discover_cancellable(&mut self, cancelled: &AtomicBool) -> Snapshot {
        self.discover_inner(Some(cancelled))
    }

    fn discover_inner(&mut self, cancelled: Option<&AtomicBool>) -> Snapshot {
        let generation = self.next_generation();
        let deadline = Instant::now() + self.operation_timeout;
        for capabilities in self.live.values().flatten() {
            capabilities.invalidate();
        }
        self.live.clear();

        let mut pending = Vec::<(String, Result<Receiver<Reply>, Reply>)>::new();
        for registration in &mut self.backends {
            match &mut registration.runner {
                Runner::Worker(worker) => pending.push((
                    registration.id.clone(),
                    worker
                        .query(generation, QueryKind::Discover)
                        .map_err(|error| Reply::Discover(Err(error))),
                )),
                Runner::Inline(backend) => pending.push((
                    registration.id.clone(),
                    Ok(ready_receiver(Reply::Discover(backend.discover()))),
                )),
            }
        }
        let mut result = Snapshot::default();
        for (id, response) in pending {
            match receive_reply(response, deadline, cancelled) {
                Reply::Discover(Ok(devices)) => {
                    self.live.insert(
                        id.clone(),
                        devices
                            .iter()
                            .map(|device| device.capabilities.clone())
                            .collect(),
                    );
                    result.devices.extend(devices);
                    result.completed.push(id);
                }
                Reply::Discover(Err(error)) | Reply::Inventory(Err(error)) => {
                    result.failures.push(BackendFailure { backend: id, error })
                }
                _ => result.failures.push(BackendFailure {
                    backend: id,
                    error: HardwareError::new(
                        ErrorKind::InvalidResponse,
                        "backend worker returned the wrong operation result",
                    ),
                }),
            }
        }
        result
    }
}

fn ready_receiver(reply: Reply) -> Receiver<Reply> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let _ = sender.send(reply);
    receiver
}

fn receive_reply(
    response: Result<Receiver<Reply>, Reply>,
    deadline: Instant,
    cancelled: Option<&AtomicBool>,
) -> Reply {
    match response {
        Err(reply) => reply,
        Ok(receiver) => loop {
            if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                break Reply::Inventory(Err(HardwareError::new(
                    ErrorKind::Unavailable,
                    "backend result wait cancelled",
                )));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let slice = if cancelled.is_some() {
                remaining.min(Duration::from_millis(25))
            } else {
                remaining
            };
            match receiver.recv_timeout(slice) {
                Ok(reply) => break reply,
                Err(RecvTimeoutError::Disconnected) => {
                    break Reply::Inventory(Err(HardwareError::new(
                        ErrorKind::Unavailable,
                        "backend worker stopped before returning a result",
                    )));
                }
                Err(RecvTimeoutError::Timeout) if Instant::now() >= deadline => {
                    break Reply::Inventory(Err(HardwareError::new(
                        ErrorKind::Timeout,
                        "backend operation exceeded its deadline",
                    )));
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        },
    }
}

impl Drop for Hardware {
    fn drop(&mut self) {
        for capabilities in self.live.values().flatten() {
            capabilities.invalidate();
        }
        for registration in &self.backends {
            if let Runner::Worker(worker) = &registration.runner {
                worker.close();
            }
        }
    }
}
