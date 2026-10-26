use crate::{
    BackendExecutor, BackendOperation, Capabilities, DEFAULT_OPERATION_TIMEOUT, Device,
    DeviceDescriptor, DeviceKey, EndpointKey, ErrorKind, HardwareError,
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

/// A polling/discovery backend. Continuous domain data belongs to a typed
/// capability session, not this inventory/snapshot interface.
pub trait Backend: Any {
    fn id(&self) -> &str;
    fn inventory(&mut self) -> Result<Vec<DeviceDescriptor>, HardwareError>;
    /// Report authoritative inventory coverage. Existing backends retain the
    /// original contract and cover their full backend on success.
    fn inventory_report(&mut self) -> Result<InventoryReport, HardwareError> {
        let backend = self.id().to_owned();
        Ok(InventoryReport {
            devices: self.inventory()?,
            coverage: vec![CoverageScope::Backend { backend }],
            failures: Vec::new(),
        })
    }
    /// Return device facts and clients without sampling their capabilities.
    fn discover(&mut self) -> Result<Vec<Device>, HardwareError>;
    /// Report authoritative discovery coverage. Backends with partial endpoint
    /// results can override this while keeping `discover` as a compatibility
    /// entry point.
    fn discovery_report(&mut self) -> Result<DiscoveryReport, HardwareError> {
        let backend = self.id().to_owned();
        Ok(DiscoveryReport {
            devices: self.discover()?,
            coverage: vec![CoverageScope::Backend { backend }],
            failures: Vec::new(),
        })
    }
}

#[derive(Clone, Debug)]
pub struct BackendFailure {
    pub backend: String,
    pub error: HardwareError,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoverageScope {
    /// The backend completed its source scan. More specific failures withhold
    /// only their matching endpoint, device, or component from that coverage.
    Backend { backend: String },
    /// This endpoint produced authoritative results for the covered operation.
    Endpoint { endpoint: EndpointKey },
    /// This exact logical route was authoritatively checked.
    Device { device: DeviceKey },
    /// This component was authoritatively checked independently of the device.
    Component {
        device: DeviceKey,
        component: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FailureScope {
    Backend {
        backend: String,
    },
    Endpoint {
        endpoint: EndpointKey,
    },
    Device {
        device: DeviceKey,
    },
    Component {
        device: DeviceKey,
        component: String,
    },
    Capability {
        device: DeviceKey,
        component: String,
        id: String,
        version: u16,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationKind {
    Inventory,
    Discovery,
    Capability,
}

#[derive(Clone, Debug)]
pub struct ScopedFailure {
    /// A failed scope withholds matching coverage; capability failures never
    /// invalidate device presence or connection evidence.
    pub scope: FailureScope,
    pub operation: OperationKind,
    pub error: HardwareError,
}

#[derive(Default, Debug)]
pub struct InventoryReport {
    pub devices: Vec<DeviceDescriptor>,
    pub coverage: Vec<CoverageScope>,
    pub failures: Vec<ScopedFailure>,
}

#[derive(Default, Debug)]
pub struct DiscoveryReport {
    pub devices: Vec<Device>,
    pub coverage: Vec<CoverageScope>,
    pub failures: Vec<ScopedFailure>,
}

#[derive(Default, Debug)]
pub struct Inventory {
    pub devices: Vec<DeviceDescriptor>,
    /// Only these sources completed an authoritative inventory this round.
    pub completed: Vec<String>,
    pub failures: Vec<BackendFailure>,
    pub coverage: Vec<CoverageScope>,
    pub scoped_failures: Vec<ScopedFailure>,
}

#[derive(Clone, Default, Debug)]
pub struct Snapshot {
    pub devices: Vec<Device>,
    pub completed: Vec<String>,
    pub failures: Vec<BackendFailure>,
    pub coverage: Vec<CoverageScope>,
    pub scoped_failures: Vec<ScopedFailure>,
}

pub struct BackendDiscoveryUpdate {
    pub backend: String,
    pub result: Result<DiscoveryReport, HardwareError>,
}

fn has_backend_coverage(coverage: &[CoverageScope], backend: &str) -> bool {
    coverage.iter().any(
        |scope| matches!(scope, CoverageScope::Backend { backend: covered } if covered == backend),
    )
}

fn push_backend_failure(
    failures: &mut Vec<BackendFailure>,
    scoped_failures: &mut Vec<ScopedFailure>,
    backend: String,
    operation: OperationKind,
    error: HardwareError,
) {
    failures.push(BackendFailure {
        backend: backend.clone(),
        error: error.clone(),
    });
    scoped_failures.push(ScopedFailure {
        scope: FailureScope::Backend { backend },
        operation,
        error,
    });
}

pub enum DiscoveryPoll {
    Update(BackendDiscoveryUpdate),
    Pending,
    Complete,
}

/// Incremental discovery across registered backends. Polling checks every
/// completed channel before waiting, so a slow earlier backend cannot hold up
/// a later backend's result.
pub struct DiscoveryStream<'a> {
    hardware: &'a mut Hardware,
    pending: Vec<(String, Receiver<Reply>)>,
    deadline: Instant,
    cancelled: Option<&'a AtomicBool>,
    complete: bool,
}

impl DiscoveryStream<'_> {
    pub fn poll(&mut self) -> DiscoveryPoll {
        if self.complete {
            return DiscoveryPoll::Complete;
        }
        if self
            .cancelled
            .is_some_and(|cancelled| cancelled.load(Ordering::Acquire))
        {
            self.pending.clear();
            self.complete = true;
            return DiscoveryPoll::Complete;
        }

        for index in 0..self.pending.len() {
            match self.pending[index].1.try_recv() {
                Ok(reply) => {
                    let (backend, _) = self.pending.remove(index);
                    let result = match reply {
                        Reply::Discover(Ok(report)) => {
                            self.hardware.live.insert(
                                backend.clone(),
                                report
                                    .devices
                                    .iter()
                                    .map(|device| device.capabilities.clone())
                                    .collect(),
                            );
                            Ok(report)
                        }
                        Reply::Discover(Err(error)) | Reply::Inventory(Err(error)) => Err(error),
                        _ => Err(HardwareError::new(
                            ErrorKind::InvalidResponse,
                            "backend worker returned the wrong operation result",
                        )),
                    };
                    return DiscoveryPoll::Update(BackendDiscoveryUpdate { backend, result });
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    let (backend, _) = self.pending.remove(index);
                    return DiscoveryPoll::Update(BackendDiscoveryUpdate {
                        backend,
                        result: Err(HardwareError::new(
                            ErrorKind::Unavailable,
                            "backend worker stopped before returning a result",
                        )),
                    });
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }

        if self.pending.is_empty() {
            self.complete = true;
            return DiscoveryPoll::Complete;
        }
        if Instant::now() >= self.deadline {
            let (backend, _) = self.pending.remove(0);
            return DiscoveryPoll::Update(BackendDiscoveryUpdate {
                backend,
                result: Err(HardwareError::new(
                    ErrorKind::Timeout,
                    "backend operation exceeded its deadline",
                )),
            });
        }
        DiscoveryPoll::Pending
    }

    pub fn is_complete(&self) -> bool {
        self.complete
    }
}

#[derive(Clone, Copy)]
enum QueryKind {
    Inventory,
    Discover,
}

enum Reply {
    Inventory(Result<InventoryReport, HardwareError>),
    Discover(Result<DiscoveryReport, HardwareError>),
}

enum Task {
    Query {
        generation: u64,
        kind: QueryKind,
        reply: SyncSender<Reply>,
    },
    Capability {
        generation: u64,
        run: BackendOperation,
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

    fn query(
        &self,
        generation: u64,
        kind: QueryKind,
        supersede: bool,
    ) -> Result<Receiver<Reply>, HardwareError> {
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
        if supersede {
            self.generation.store(generation, Ordering::Release);
            // Rediscovery supersedes queued work. An already-running native
            // call cannot be safely interrupted, but its result is checked.
            state.tasks.clear();
        } else if self.generation.load(Ordering::Acquire) != generation {
            return Err(HardwareError::new(
                ErrorKind::Unavailable,
                "inventory generation is stale",
            ));
        } else if state.tasks.len() >= MAX_PENDING_OPERATIONS {
            return Err(HardwareError::new(
                ErrorKind::Unavailable,
                "backend operation queue is full",
            ));
        }
        state.tasks.push_back(Task::Query {
            generation,
            kind,
            reply,
        });
        self.changed.notify_one();
        Ok(receiver)
    }

    fn capability(&self, generation: u64, run: BackendOperation) -> Result<(), HardwareError> {
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

struct WorkerExecutor {
    shared: Weak<WorkerShared>,
    generation: u64,
}

impl BackendExecutor for WorkerExecutor {
    fn enqueue(&self, operation: BackendOperation) -> Result<(), HardwareError> {
        let Some(shared) = self.shared.upgrade() else {
            return Err(HardwareError::new(
                ErrorKind::Unavailable,
                "backend worker has stopped",
            ));
        };
        shared.capability(self.generation, operation)
    }

    fn is_current(&self) -> bool {
        self.shared
            .upgrade()
            .is_some_and(|shared| shared.generation.load(Ordering::Acquire) == self.generation)
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
                                    QueryKind::Inventory => catch_unwind(AssertUnwindSafe(|| {
                                        backend.inventory_report()
                                    }))
                                    .unwrap_or_else(|_| {
                                        Err(HardwareError::new(
                                            ErrorKind::Unavailable,
                                            "backend inventory panicked",
                                        ))
                                    })
                                    .map(|report| Reply::Inventory(Ok(report))),
                                    QueryKind::Discover => catch_unwind(AssertUnwindSafe(|| {
                                        backend.discovery_report()
                                    }))
                                    .unwrap_or_else(|_| {
                                        Err(HardwareError::new(
                                            ErrorKind::Unavailable,
                                            "backend discovery panicked",
                                        ))
                                    })
                                    .map(|mut report| {
                                        let executor: Arc<dyn BackendExecutor> =
                                            Arc::new(WorkerExecutor {
                                                shared: Arc::downgrade(&thread_shared),
                                                generation,
                                            });
                                        for device in &mut report.devices {
                                            device.capabilities.bind_executor(executor.clone());
                                        }
                                        Reply::Discover(Ok(report))
                                    }),
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
                                if let Ok(Reply::Discover(Ok(report))) = &result {
                                    for device in &report.devices {
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

    fn query(
        &self,
        generation: u64,
        kind: QueryKind,
        supersede: bool,
    ) -> Result<Receiver<Reply>, HardwareError> {
        self.shared.query(generation, kind, supersede)
    }

    fn close(&self) {
        self.shared.close();
    }
}

fn invalidate_reply(reply: Reply) {
    if let Reply::Discover(Ok(report)) = reply {
        for device in report.devices {
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
        let generation = self.generation;
        let deadline = Instant::now() + self.operation_timeout;
        let mut pending = Vec::<(String, Result<Receiver<Reply>, Reply>)>::new();
        for registration in &mut self.backends {
            match &mut registration.runner {
                Runner::Worker(worker) => pending.push((
                    registration.id.clone(),
                    worker
                        .query(generation, QueryKind::Inventory, false)
                        .map_err(|error| Reply::Inventory(Err(error))),
                )),
                Runner::Inline(backend) => pending.push((
                    registration.id.clone(),
                    Ok(ready_receiver(Reply::Inventory(backend.inventory_report()))),
                )),
            }
        }
        let mut result = Inventory::default();
        for (id, response) in pending {
            match receive_reply(response, deadline, None) {
                Reply::Inventory(Ok(report)) => {
                    result.devices.extend(report.devices);
                    result.coverage.extend(report.coverage);
                    result.scoped_failures.extend(report.failures);
                    if has_backend_coverage(&result.coverage, &id) {
                        result.completed.push(id);
                    }
                }
                Reply::Inventory(Err(error)) | Reply::Discover(Err(error)) => push_backend_failure(
                    &mut result.failures,
                    &mut result.scoped_failures,
                    id,
                    OperationKind::Inventory,
                    error,
                ),
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
        let stream = self.discovery_stream_inner(None);
        collect_discovery(stream)
    }

    /// Discover while allowing a caller to stop waiting without joining or
    /// interrupting a native operation already running on a backend worker.
    pub fn discover_cancellable(&mut self, cancelled: &AtomicBool) -> Snapshot {
        let stream = self.discovery_stream_inner(Some(cancelled));
        collect_discovery(stream)
    }

    pub fn discovery_stream(&mut self) -> DiscoveryStream<'_> {
        self.discovery_stream_inner(None)
    }

    pub fn discovery_stream_cancellable<'a>(
        &'a mut self,
        cancelled: &'a AtomicBool,
    ) -> DiscoveryStream<'a> {
        self.discovery_stream_inner(Some(cancelled))
    }

    fn discovery_stream_inner<'a>(
        &'a mut self,
        cancelled: Option<&'a AtomicBool>,
    ) -> DiscoveryStream<'a> {
        let generation = self.next_generation();
        let deadline = Instant::now() + self.operation_timeout;
        for capabilities in self.live.values().flatten() {
            capabilities.invalidate();
        }
        self.live.clear();

        let mut pending = Vec::<(String, Receiver<Reply>)>::new();
        for registration in &mut self.backends {
            match &mut registration.runner {
                Runner::Worker(worker) => {
                    let receiver = worker
                        .query(generation, QueryKind::Discover, true)
                        .unwrap_or_else(|error| ready_receiver(Reply::Discover(Err(error))));
                    pending.push((registration.id.clone(), receiver));
                }
                Runner::Inline(backend) => pending.push((
                    registration.id.clone(),
                    ready_receiver(Reply::Discover(backend.discovery_report())),
                )),
            }
        }
        DiscoveryStream {
            hardware: self,
            pending,
            deadline,
            cancelled,
            complete: false,
        }
    }
}

fn collect_discovery(mut stream: DiscoveryStream<'_>) -> Snapshot {
    let mut snapshot = Snapshot::default();
    loop {
        match stream.poll() {
            DiscoveryPoll::Update(update) => match update.result {
                Ok(report) => {
                    snapshot.devices.extend(report.devices);
                    snapshot.coverage.extend(report.coverage);
                    snapshot.scoped_failures.extend(report.failures);
                    if has_backend_coverage(&snapshot.coverage, &update.backend) {
                        snapshot.completed.push(update.backend);
                    }
                }
                Err(error) => push_backend_failure(
                    &mut snapshot.failures,
                    &mut snapshot.scoped_failures,
                    update.backend,
                    OperationKind::Discovery,
                    error,
                ),
            },
            DiscoveryPoll::Pending => thread::sleep(Duration::from_millis(5)),
            DiscoveryPoll::Complete => return snapshot,
        }
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
