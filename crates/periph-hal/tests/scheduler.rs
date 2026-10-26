use periph_hal::{
    Backend, BackendExecutor, BackendOperation, Capabilities, Capability, Connection, Device,
    DeviceDescriptor, DeviceFacts, DeviceKey, ErrorKind, Hardware, HardwareError,
    battery::{Battery, BatteryReading, BatteryState, Percent, Sample, SampleOrigin},
};
use std::{
    any::Any,
    cell::Cell,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, SystemTime},
};

fn descriptor(backend: &str, local: &str, name: &str) -> DeviceDescriptor {
    DeviceDescriptor {
        key: DeviceKey {
            backend: backend.into(),
            local: local.into(),
        },
        name: name.into(),
        ..DeviceDescriptor::default()
    }
}

fn device(descriptor: DeviceDescriptor, capabilities: Capabilities) -> Device {
    Device {
        descriptor,
        connection: Connection::Connected,
        capabilities,
        issue: None,
    }
}

fn reading() -> BatteryReading {
    reading_level(42)
}

fn reading_level(level: u8) -> BatteryReading {
    BatteryReading {
        state: BatteryState::Value(Sample {
            level: Percent::new(level).unwrap(),
            origin: SampleOrigin::DeviceResponse,
            observed_at: SystemTime::now(),
            sampled_at: None,
        }),
        facts: DeviceFacts::default(),
    }
}

struct LocalBackend {
    created_on: thread::ThreadId,
    state: Rc<Cell<u8>>,
}

#[derive(Clone)]
struct LocalCounter {
    executor: Option<Arc<dyn BackendExecutor>>,
}

impl Capability for LocalCounter {
    const ID: &'static str = "test.local-counter";

    fn bind_executor(
        &self,
        executor: Arc<dyn BackendExecutor>,
    ) -> Option<Arc<dyn Any + Send + Sync>> {
        Some(Arc::new(Self {
            executor: Some(executor),
        }))
    }
}

impl LocalCounter {
    fn read(&self) -> Result<u8, HardwareError> {
        let executor = self
            .executor
            .as_ref()
            .ok_or_else(|| HardwareError::new(ErrorKind::Unavailable, "counter is not bound"))?;
        let (reply, receiver) = mpsc::sync_channel(1);
        let operation: BackendOperation = Box::new(move |backend: &mut dyn Any| {
            let result = backend
                .downcast_mut::<LocalBackend>()
                .map(|backend| {
                    let value = backend.state.get();
                    backend.state.set(value + 1);
                    value
                })
                .ok_or_else(|| {
                    HardwareError::new(ErrorKind::Conflict, "counter backend type mismatch")
                });
            let _ = reply.send(result);
        });
        executor.enqueue(operation)?;
        receiver
            .recv_timeout(Duration::from_secs(1))
            .map_err(|_| HardwareError::new(ErrorKind::Timeout, "counter operation timed out"))?
    }
}

impl Backend for LocalBackend {
    fn id(&self) -> &str {
        "test.local"
    }

    fn inventory(&mut self) -> Result<Vec<DeviceDescriptor>, HardwareError> {
        assert_eq!(thread::current().id(), self.created_on);
        Ok(vec![descriptor("test.local", "camera", "camera")])
    }

    fn discover(&mut self) -> Result<Vec<Device>, HardwareError> {
        assert_eq!(thread::current().id(), self.created_on);
        self.state.set(self.state.get() + 1);
        let mut capabilities = Capabilities::default();
        capabilities.insert(
            "main",
            Battery::new_local::<Self, _>(|backend| {
                assert_eq!(thread::current().id(), backend.created_on);
                let next = backend.state.get() + 1;
                backend.state.set(next);
                Ok(reading_level(next))
            }),
        )?;
        capabilities.insert("count", LocalCounter { executor: None })?;
        Ok(vec![device(
            descriptor("test.local", "camera", "camera"),
            capabilities,
        )])
    }
}

#[test]
fn factory_builds_a_non_send_backend_on_its_worker() {
    let state = Arc::new(Mutex::new(None));
    let worker_id = state.clone();
    let mut hardware = Hardware::new();
    hardware
        .register_factory("test.local", move || {
            *worker_id.lock().unwrap() = Some(thread::current().id());
            LocalBackend {
                created_on: thread::current().id(),
                state: Rc::new(Cell::new(0)),
            }
        })
        .unwrap();

    let snapshot = hardware.discover();
    assert_eq!(snapshot.completed, ["test.local"]);
    assert_eq!(snapshot.devices[0].descriptor.name, "camera");
    assert!(state.lock().unwrap().is_some());
    let battery = snapshot.devices[0]
        .capabilities
        .get::<Battery>("main")
        .unwrap();
    for expected in [2, 3] {
        let reading = battery.read().unwrap();
        assert!(
            matches!(reading.state, BatteryState::Value(sample) if sample.level.get() == expected)
        );
    }
    assert_eq!(
        snapshot.devices[0]
            .capabilities
            .get::<LocalCounter>("count")
            .unwrap()
            .read()
            .unwrap(),
        3
    );
    let inventory = hardware.inventory();
    assert_eq!(inventory.completed, ["test.local"]);
    let reading = battery.read().unwrap();
    assert!(matches!(reading.state, BatteryState::Value(sample) if sample.level.get() == 5));
}

struct MismatchedLocalBackend;

impl Backend for MismatchedLocalBackend {
    fn id(&self) -> &str {
        "test.mismatch"
    }

    fn inventory(&mut self) -> Result<Vec<DeviceDescriptor>, HardwareError> {
        Ok(vec![])
    }

    fn discover(&mut self) -> Result<Vec<Device>, HardwareError> {
        let mut capabilities = Capabilities::default();
        capabilities.insert(
            "main",
            Battery::new_local::<LocalBackend, _>(|_| Ok(reading())),
        )?;
        Ok(vec![device(
            descriptor("test.mismatch", "device", "device"),
            capabilities,
        )])
    }
}

#[test]
fn local_battery_backend_type_mismatch_is_a_structured_conflict() {
    let mut hardware = Hardware::new();
    hardware
        .register_factory("test.mismatch", || MismatchedLocalBackend)
        .unwrap();
    let snapshot = hardware.discover();
    let battery = snapshot.devices[0]
        .capabilities
        .get::<Battery>("main")
        .unwrap();
    assert_eq!(battery.read().unwrap_err().kind, ErrorKind::Conflict);
}

struct SlowBackend {
    release: Receiver<()>,
    started: Sender<()>,
    finished: Sender<()>,
    calls: u8,
}

impl Backend for SlowBackend {
    fn id(&self) -> &str {
        "test.slow"
    }

    fn inventory(&mut self) -> Result<Vec<DeviceDescriptor>, HardwareError> {
        Ok(vec![])
    }

    fn discover(&mut self) -> Result<Vec<Device>, HardwareError> {
        self.calls += 1;
        if self.calls == 1 {
            let _ = self.started.send(());
            let _ = self.release.recv();
            let _ = self.finished.send(());
            Ok(vec![device(
                descriptor("test.slow", "old", "late result"),
                Capabilities::default(),
            )])
        } else {
            Ok(vec![device(
                descriptor("test.slow", "fresh", "current result"),
                Capabilities::default(),
            )])
        }
    }
}

struct FastBackend;

impl Backend for FastBackend {
    fn id(&self) -> &str {
        "test.fast"
    }

    fn inventory(&mut self) -> Result<Vec<DeviceDescriptor>, HardwareError> {
        Ok(vec![])
    }

    fn discover(&mut self) -> Result<Vec<Device>, HardwareError> {
        Ok(vec![device(
            descriptor("test.fast", "fast", "fast result"),
            Capabilities::default(),
        )])
    }
}

#[test]
fn slow_backend_times_out_without_hiding_fast_or_late_results() {
    let (release_tx, release_rx) = mpsc::channel();
    let (started_tx, started_rx) = mpsc::channel();
    let (finished_tx, finished_rx) = mpsc::channel();
    let mut hardware = Hardware::new();
    hardware.set_operation_timeout(Duration::from_millis(40));
    hardware
        .register_factory("test.slow", move || SlowBackend {
            release: release_rx,
            started: started_tx,
            finished: finished_tx,
            calls: 0,
        })
        .unwrap();
    hardware
        .register_factory("test.fast", || FastBackend)
        .unwrap();

    let first = hardware.discover();
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(first.completed, ["test.fast"]);
    assert_eq!(first.devices.len(), 1);
    assert_eq!(first.devices[0].descriptor.name, "fast result");
    assert!(first.failures.iter().any(|failure| {
        failure.backend == "test.slow" && failure.error.kind == ErrorKind::Timeout
    }));

    hardware.set_operation_timeout(Duration::from_secs(2));
    release_tx.send(()).unwrap();
    finished_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let second = hardware.discover();
    assert!(second.failures.is_empty());
    assert!(second.devices.iter().any(|device| {
        device.descriptor.key.backend == "test.slow" && device.descriptor.name == "current result"
    }));
    assert!(
        !second
            .devices
            .iter()
            .any(|device| device.descriptor.name == "late result")
    );
}

#[test]
fn discovery_stream_reports_fast_backend_before_blocked_peer() {
    let (release_tx, release_rx) = mpsc::channel();
    let (started_tx, started_rx) = mpsc::channel();
    let (finished_tx, _finished_rx) = mpsc::channel();
    let mut hardware = Hardware::new();
    hardware.set_operation_timeout(Duration::from_secs(2));
    hardware
        .register_factory("test.slow", move || SlowBackend {
            release: release_rx,
            started: started_tx,
            finished: finished_tx,
            calls: 0,
        })
        .unwrap();
    hardware
        .register_factory("test.fast", || FastBackend)
        .unwrap();

    let mut stream = hardware.discovery_stream();
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let mut fast_name = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while fast_name.is_none() && std::time::Instant::now() < deadline {
        match stream.poll() {
            periph_hal::DiscoveryPoll::Update(update) if update.backend == "test.fast" => {
                fast_name = update.result.ok().and_then(|devices| {
                    devices
                        .devices
                        .first()
                        .map(|device| device.descriptor.name.clone())
                });
            }
            periph_hal::DiscoveryPoll::Pending => thread::sleep(Duration::from_millis(2)),
            periph_hal::DiscoveryPoll::Update(_) | periph_hal::DiscoveryPoll::Complete => {}
        }
    }
    assert_eq!(fast_name.as_deref(), Some("fast result"));
    drop(stream);
    drop(hardware);
    release_tx.send(()).unwrap();
}

#[test]
fn caller_cancellation_releases_a_blocked_discovery_wait() {
    let (release_tx, release_rx) = mpsc::channel();
    let (started_tx, started_rx) = mpsc::channel();
    let (finished_tx, _finished_rx) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let signal = cancelled.clone();
    let cancel_thread = thread::spawn(move || {
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        signal.store(true, Ordering::Release);
    });

    let mut hardware = Hardware::new();
    hardware.set_operation_timeout(Duration::from_secs(20));
    hardware
        .register_factory("test.slow", move || SlowBackend {
            release: release_rx,
            started: started_tx,
            finished: finished_tx,
            calls: 0,
        })
        .unwrap();

    let started_at = std::time::Instant::now();
    let snapshot = hardware.discover_cancellable(&cancelled);
    cancel_thread.join().unwrap();
    assert!(started_at.elapsed() < Duration::from_secs(1));
    assert!(snapshot.failures.is_empty());
    assert!(snapshot.devices.is_empty());

    // Hardware drop shuts down its mailbox without joining the blocked worker.
    let drop_started_at = std::time::Instant::now();
    drop(hardware);
    assert!(drop_started_at.elapsed() < Duration::from_secs(1));
    // The native call itself is not interrupted; release it for test cleanup.
    release_tx.send(()).unwrap();
}

struct BatteryBackend {
    first_started: Sender<()>,
    release_first: Arc<Mutex<Receiver<()>>>,
    second_reads: Arc<AtomicUsize>,
}

impl Backend for BatteryBackend {
    fn id(&self) -> &str {
        "test.battery"
    }

    fn inventory(&mut self) -> Result<Vec<DeviceDescriptor>, HardwareError> {
        Ok(vec![])
    }

    fn discover(&mut self) -> Result<Vec<Device>, HardwareError> {
        let mut first = Capabilities::default();
        let first_started = self.first_started.clone();
        let release_first = self.release_first.clone();
        first.insert(
            "main",
            Battery::new(move || {
                let _ = first_started.send(());
                let _ = release_first.lock().unwrap().recv();
                Ok(reading())
            }),
        )?;
        let mut second = Capabilities::default();
        let second_reads = self.second_reads.clone();
        second.insert(
            "main",
            Battery::new(move || {
                second_reads.fetch_add(1, Ordering::SeqCst);
                Ok(reading())
            }),
        )?;
        Ok(vec![
            device(descriptor("test.battery", "first", "first"), first),
            device(descriptor("test.battery", "second", "second"), second),
        ])
    }
}

#[test]
fn queued_battery_request_checks_invalidation_before_io() {
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let second_reads = Arc::new(AtomicUsize::new(0));
    let mut hardware = Hardware::new();
    hardware
        .register_factory("test.battery", {
            let second_reads = second_reads.clone();
            move || BatteryBackend {
                first_started: started_tx,
                release_first: release_rx,
                second_reads,
            }
        })
        .unwrap();
    let snapshot = hardware.discover();
    assert_eq!(snapshot.devices.len(), 2);
    let first = snapshot.devices[0]
        .capabilities
        .get::<Battery>("main")
        .unwrap()
        .clone();
    let second = snapshot.devices[1]
        .capabilities
        .get::<Battery>("main")
        .unwrap()
        .clone();
    let first_request = first.request_read();
    let second_request = second.request_read();
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    second.close();
    release_tx.send(()).unwrap();

    assert!(first_request.wait(Duration::from_secs(1)).is_ok());
    assert_eq!(
        second_request
            .wait(Duration::from_secs(1))
            .unwrap_err()
            .kind,
        ErrorKind::Unavailable
    );
    assert_eq!(second_reads.load(Ordering::SeqCst), 0);
}
