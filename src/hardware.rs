//! Coordinate device-core facts and optional battery-domain observations.
mod controls;
use controls::{PendingControl, poll as poll_controls, queue as queue_controls};

use crate::{
    battery::{Batteries, Observation},
    model::{self, Device},
};
use periph_hal::{
    BackendDiscoveryUpdate, Capability, ComponentKey, CoverageScope, DiscoveryPoll,
    DiscoveryStream, FailureScope, Hardware, OperationKind, ScopedFailure, Snapshot,
    battery::{Battery, BatteryReadRequest, BatteryState},
};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Default)]
pub struct ScanReport {
    pub snapshot: Snapshot,
    pub batteries: Vec<Observation>,
    pub dpi_stages: Vec<(periph_hal::DeviceKey, Option<u8>)>,
    pub lighting_states: Vec<(periph_hal::DeviceKey, bool)>,
}

fn record_capability_failure(
    snapshot: &mut Snapshot,
    key: &periph_hal::DeviceKey,
    component: &ComponentKey,
    error: &periph_hal::HardwareError,
) {
    record_failure(
        snapshot,
        key,
        component,
        Battery::ID,
        Battery::VERSION,
        error,
    );
}

fn record_failure(
    snapshot: &mut Snapshot,
    key: &periph_hal::DeviceKey,
    component: &ComponentKey,
    id: &str,
    version: u16,
    error: &periph_hal::HardwareError,
) {
    snapshot.scoped_failures.push(ScopedFailure {
        scope: FailureScope::Capability {
            device: key.clone(),
            component: serde_json::to_string(component).unwrap_or_default(),
            id: id.into(),
            version,
        },
        operation: OperationKind::Capability,
        error: error.clone(),
    });
}

struct PendingBattery {
    device_index: usize,
    key: periph_hal::DeviceKey,
    component: ComponentKey,
    request: BatteryReadRequest,
}

/// Progressive scan state shared by the UI worker and HAL's per-backend
/// discovery stream. `poll` never waits for a particular backend or battery.
pub struct RefreshSession<'a> {
    discovery: DiscoveryStream<'a>,
    snapshot: Snapshot,
    observations: Vec<Observation>,
    pending: Vec<PendingBattery>,
    pending_controls: Vec<PendingControl>,
    dpi_stages: Vec<(periph_hal::DeviceKey, Option<u8>)>,
    lighting_states: Vec<(periph_hal::DeviceKey, bool)>,
    deadline: Instant,
    cancelled: &'a AtomicBool,
    done: bool,
}

impl<'a> RefreshSession<'a> {
    pub fn new(hardware: &'a mut Hardware, cancelled: &'a AtomicBool) -> Self {
        let timeout = hardware.operation_timeout();
        Self {
            discovery: hardware.discovery_stream_cancellable(cancelled),
            snapshot: Snapshot::default(),
            observations: Vec::new(),
            pending: Vec::new(),
            pending_controls: Vec::new(),
            dpi_stages: Vec::new(),
            lighting_states: Vec::new(),
            deadline: Instant::now() + timeout,
            cancelled,
            done: false,
        }
    }

    pub fn poll(&mut self) -> Option<ScanReport> {
        if self.done {
            return None;
        }
        if self.cancelled.load(Ordering::Acquire) {
            self.done = true;
            self.pending.clear();
            self.pending_controls.clear();
            return None;
        }

        let mut changed = false;
        while let DiscoveryPoll::Update(update) = self.discovery.poll() {
            self.accept_backend(update);
            changed = true;
        }

        let mut index = 0;
        while index < self.pending.len() {
            let result = self.pending[index].request.try_take().or_else(|| {
                (Instant::now() >= self.deadline).then(|| {
                    Err(periph_hal::HardwareError::new(
                        periph_hal::ErrorKind::Timeout,
                        "battery operation exceeded its deadline",
                    ))
                })
            });
            let Some(result) = result else {
                index += 1;
                continue;
            };
            let pending = self.pending.remove(index);
            let state = match result {
                Ok(reading) => {
                    if let BatteryState::Unavailable(error) = &reading.state {
                        record_capability_failure(
                            &mut self.snapshot,
                            &pending.key,
                            &pending.component,
                            error,
                        );
                    }
                    reading
                        .facts
                        .apply_to(&mut self.snapshot.devices[pending.device_index]);
                    reading.state
                }
                Err(error) => {
                    self.snapshot.devices[pending.device_index].issue = Some(error.clone());
                    record_capability_failure(
                        &mut self.snapshot,
                        &pending.key,
                        &pending.component,
                        &error,
                    );
                    BatteryState::Unavailable(error)
                }
            };
            self.observations.push(Observation {
                key: pending.key,
                component: pending.component,
                state,
            });
            changed = true;
        }

        changed |= poll_controls(
            &mut self.pending_controls,
            &mut self.snapshot,
            &mut self.dpi_stages,
            &mut self.lighting_states,
            self.deadline,
        );
        if self.discovery.is_complete()
            && self.pending.is_empty()
            && self.pending_controls.is_empty()
        {
            self.done = true;
        }
        changed.then(|| ScanReport {
            snapshot: self.snapshot.clone(),
            batteries: self.observations.clone(),
            dpi_stages: self.dpi_stages.clone(),
            lighting_states: self.lighting_states.clone(),
        })
    }

    fn accept_backend(&mut self, update: BackendDiscoveryUpdate) {
        match update.result {
            Ok(report) => {
                let first = self.snapshot.devices.len();
                self.snapshot.devices.extend(report.devices);
                self.snapshot.coverage.extend(report.coverage);
                self.snapshot.scoped_failures.extend(report.failures);
                if self.snapshot.coverage.iter().any(|scope| {
                    matches!(scope, CoverageScope::Backend { backend } if backend == &update.backend)
                }) {
                    self.snapshot.completed.push(update.backend.clone());
                }
                for device_index in first..self.snapshot.devices.len() {
                    self.queue_batteries(device_index);
                }
                self.pending_controls
                    .extend(queue_controls(&self.snapshot.devices[first..]));
            }
            Err(error) => {
                self.snapshot.failures.push(periph_hal::BackendFailure {
                    backend: update.backend.clone(),
                    error: error.clone(),
                });
                self.snapshot.scoped_failures.push(ScopedFailure {
                    scope: FailureScope::Backend {
                        backend: update.backend,
                    },
                    operation: OperationKind::Discovery,
                    error,
                });
            }
        }
    }

    fn queue_batteries(&mut self, device_index: usize) {
        let device = &self.snapshot.devices[device_index];
        let key = device.descriptor.key.clone();
        let components: Vec<_> = device
            .capabilities
            .descriptors()
            .filter(|capability| {
                capability.id == Battery::ID && capability.version == Battery::VERSION
            })
            .map(|capability| capability.component.clone())
            .collect();
        for component in components {
            if let Some(battery) = device.capabilities.get_component::<Battery>(&component) {
                self.pending.push(PendingBattery {
                    device_index,
                    key: key.clone(),
                    component,
                    request: battery.request_read(),
                });
            }
        }
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    pub fn has_failures(&self) -> bool {
        !self.snapshot.failures.is_empty() || !self.snapshot.scoped_failures.is_empty()
    }
}

/// Blocking convenience for one-shot commands and tests. UI refreshes use
/// `RefreshSession` so completed backends and capabilities appear at once.
pub fn from_snapshot(snapshot: Snapshot) -> ScanReport {
    from_snapshot_with_timeout(snapshot, periph_hal::DEFAULT_OPERATION_TIMEOUT)
}

pub fn from_snapshot_with_timeout(snapshot: Snapshot, timeout: Duration) -> ScanReport {
    from_snapshot_inner(snapshot, timeout, None)
}

pub fn from_snapshot_cancellable(
    snapshot: Snapshot,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> ScanReport {
    from_snapshot_inner(snapshot, timeout, Some(cancelled))
}

fn from_snapshot_inner(
    mut snapshot: Snapshot,
    timeout: Duration,
    cancelled: Option<&AtomicBool>,
) -> ScanReport {
    let deadline = Instant::now() + timeout;
    let mut pending_controls = queue_controls(&snapshot.devices);
    let mut pending = Vec::<PendingBattery>::new();
    for (device_index, device) in snapshot.devices.iter().enumerate() {
        let key = device.descriptor.key.clone();
        let components: Vec<_> = device
            .capabilities
            .descriptors()
            .filter(|capability| {
                capability.id == Battery::ID && capability.version == Battery::VERSION
            })
            .map(|capability| capability.component.clone())
            .collect();
        for component in components {
            if let Some(battery) = device.capabilities.get_component::<Battery>(&component) {
                pending.push(PendingBattery {
                    device_index,
                    key: key.clone(),
                    component,
                    request: battery.request_read(),
                });
            }
        }
    }

    let mut batteries = Vec::new();
    let mut index = 0;
    while index < pending.len() {
        if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            break;
        }
        let result = pending[index].request.try_take().or_else(|| {
            (Instant::now() >= deadline).then(|| {
                Err(periph_hal::HardwareError::new(
                    periph_hal::ErrorKind::Timeout,
                    "battery operation exceeded its deadline",
                ))
            })
        });
        let Some(result) = result else {
            index += 1;
            if index == pending.len() {
                index = 0;
                thread::sleep(Duration::from_millis(5));
            }
            continue;
        };
        let operation = pending.remove(index);
        let state = match result {
            Ok(reading) => {
                if let BatteryState::Unavailable(error) = &reading.state {
                    record_capability_failure(
                        &mut snapshot,
                        &operation.key,
                        &operation.component,
                        error,
                    );
                }
                reading
                    .facts
                    .apply_to(&mut snapshot.devices[operation.device_index]);
                reading.state
            }
            Err(error) => {
                snapshot.devices[operation.device_index].issue = Some(error.clone());
                record_capability_failure(
                    &mut snapshot,
                    &operation.key,
                    &operation.component,
                    &error,
                );
                BatteryState::Unavailable(error)
            }
        };
        batteries.push(Observation {
            key: operation.key,
            component: operation.component,
            state,
        });
        index = 0;
    }
    let mut dpi_stages = Vec::new();
    let mut lighting_states = Vec::new();
    while !pending_controls.is_empty()
        && !cancelled.is_some_and(|flag| flag.load(Ordering::Acquire))
    {
        if !poll_controls(
            &mut pending_controls,
            &mut snapshot,
            &mut dpi_stages,
            &mut lighting_states,
            deadline,
        ) {
            thread::sleep(Duration::from_millis(5));
        }
    }
    ScanReport {
        snapshot,
        batteries,
        dpi_stages,
        lighting_states,
    }
}

pub fn merge_report(
    previous: &[Device],
    batteries: &mut Batteries,
    report: ScanReport,
    now: u64,
) -> Vec<Device> {
    let mut merged = model::merge_snapshot(previous, &report.snapshot);
    batteries.apply(&report.batteries, &merged, now);
    for (key, stage) in &report.dpi_stages {
        if let Some(device_id) = merged.bindings.get(key)
            && let Some(device) = merged.devices.iter_mut().find(|d| &d.saved.id == device_id)
        {
            if stage.is_some() {
                device.current_dpi_stage = *stage;
                device.saved.last_dpi_stage = *stage;
            } else if device.current_dpi_stage.is_none() {
                device.current_dpi_stage = device.saved.last_dpi_stage;
            }
        }
    }
    for (key, on) in &report.lighting_states {
        if let Some(device_id) = merged.bindings.get(key)
            && let Some(device) = merged.devices.iter_mut().find(|d| &d.saved.id == device_id)
        {
            device.current_lighting_on = Some(*on);
            device.saved.last_lighting_on = Some(*on);
        }
    }
    merged.devices
}

#[cfg(test)]
mod tests {
    use super::*;
    use periph_hal::{
        Backend, Capabilities, Connection, Device, DeviceDescriptor, DeviceFacts, DeviceKey,
        ErrorKind, HardwareError,
        battery::{Battery, BatteryReading, BatteryState, Percent, Sample, SampleOrigin},
    };
    use std::{
        sync::{
            Arc, Mutex,
            atomic::AtomicBool,
            mpsc::{self, Receiver, Sender},
        },
        time::SystemTime,
    };

    fn make_device(backend: &str, local: &str, name: &str, battery: Battery) -> Device {
        let mut capabilities = Capabilities::default();
        capabilities.insert("main", battery).unwrap();
        Device {
            descriptor: DeviceDescriptor {
                key: DeviceKey {
                    backend: backend.into(),
                    local: local.into(),
                },
                name: name.into(),
                ..DeviceDescriptor::default()
            },
            connection: Connection::Unknown,
            capabilities,
            issue: None,
        }
    }

    fn reading(level: u8) -> BatteryReading {
        BatteryReading {
            state: BatteryState::Value(Sample {
                level: Percent::new(level).unwrap(),
                origin: SampleOrigin::DeviceResponse,
                observed_at: SystemTime::now(),
                sampled_at: None,
            }),
            facts: DeviceFacts {
                connection: Some(Connection::Connected),
                ..DeviceFacts::default()
            },
        }
    }

    struct HidBackend {
        started: Sender<()>,
        release: Arc<Mutex<Receiver<()>>>,
    }

    impl Backend for HidBackend {
        fn id(&self) -> &str {
            "test.hid"
        }

        fn inventory(&mut self) -> Result<Vec<DeviceDescriptor>, HardwareError> {
            Ok(Vec::new())
        }

        fn discover(&mut self) -> Result<Vec<Device>, HardwareError> {
            let started = self.started.clone();
            let release = self.release.clone();
            Ok(vec![make_device(
                "test.hid",
                "receiver/slot-1",
                "slow HID",
                Battery::new(move || {
                    let _ = started.send(());
                    release.lock().unwrap().recv().map_err(|error| {
                        HardwareError::new(ErrorKind::Unavailable, error.to_string())
                    })?;
                    Ok(reading(10))
                }),
            )])
        }
    }

    struct BluetoothBackend {
        hid_battery_started: Receiver<()>,
    }

    impl Backend for BluetoothBackend {
        fn id(&self) -> &str {
            "test.bluetooth"
        }

        fn inventory(&mut self) -> Result<Vec<DeviceDescriptor>, HardwareError> {
            Ok(Vec::new())
        }

        fn discover(&mut self) -> Result<Vec<Device>, HardwareError> {
            // Make the slow HID battery request enter its backend queue first.
            self.hid_battery_started
                .recv()
                .map_err(|error| HardwareError::new(ErrorKind::Unavailable, error.to_string()))?;
            Ok(vec![make_device(
                "test.bluetooth",
                "earbuds",
                "fast Bluetooth",
                Battery::new(|| Ok(reading(77))),
            )])
        }
    }

    #[test]
    fn fast_bluetooth_state_updates_while_hid_battery_read_is_blocked() {
        let (hid_started_tx, hid_started_rx) = mpsc::channel();
        let (release_hid_tx, release_hid_rx) = mpsc::channel();
        let mut hardware = Hardware::new();
        hardware.set_operation_timeout(Duration::from_secs(2));
        hardware
            .register_factory("test.hid", move || HidBackend {
                started: hid_started_tx,
                release: Arc::new(Mutex::new(release_hid_rx)),
            })
            .unwrap();
        hardware
            .register_factory("test.bluetooth", move || BluetoothBackend {
                hid_battery_started: hid_started_rx,
            })
            .unwrap();

        let cancelled = AtomicBool::new(false);
        let mut refresh = RefreshSession::new(&mut hardware, &cancelled);
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut app_devices = Vec::new();
        let mut batteries = Batteries::default();
        let mut saw_fast_bluetooth_battery = false;
        while !saw_fast_bluetooth_battery && Instant::now() < deadline {
            if let Some(report) = refresh.poll() {
                let merge = model::merge_snapshot(&app_devices, &report.snapshot);
                batteries.apply(&report.batteries, &merge, 100);
                app_devices = merge.devices;
                saw_fast_bluetooth_battery = report.batteries.iter().any(|observation| {
                    observation.key.backend == "test.bluetooth"
                        && matches!(observation.state, BatteryState::Value(ref sample) if sample.level.get() == 77)
                });
            }
            if !saw_fast_bluetooth_battery {
                thread::sleep(Duration::from_millis(2));
            }
        }

        assert!(
            saw_fast_bluetooth_battery,
            "fast Bluetooth reading was head-of-line blocked"
        );
        let fast = app_devices
            .iter()
            .find(|device| device.saved.descriptor.name == "fast Bluetooth")
            .expect("Bluetooth device should already be in application state");
        let view = batteries.view(&fast.saved.id, "main");
        assert_eq!(view.level, Some(77));

        drop(refresh);
        release_hid_tx.send(()).unwrap();
    }

    #[test]
    fn battery_errors_are_capability_scoped_and_keep_confirmed_connection() {
        let unavailable = HardwareError::new(ErrorKind::Timeout, "no device reply");
        let unavailable_reading = BatteryReading {
            state: BatteryState::Unavailable(unavailable.clone()),
            facts: DeviceFacts {
                connection: Some(Connection::Connected),
                ..DeviceFacts::default()
            },
        };
        let unavailable_battery = Battery::new(move || Ok(unavailable_reading.clone()));
        let failed_battery =
            Battery::new(move || Err(HardwareError::new(ErrorKind::Transport, "read failed")));
        let mut first = make_device(
            "test.power",
            "state-unavailable",
            "unavailable",
            unavailable_battery,
        );
        let mut second = make_device(
            "test.power",
            "request-error",
            "request error",
            failed_battery,
        );
        first.connection = Connection::Connected;
        second.connection = Connection::Connected;

        let report = from_snapshot_with_timeout(
            Snapshot {
                devices: vec![first, second],
                ..Snapshot::default()
            },
            Duration::from_secs(1),
        );
        assert_eq!(report.snapshot.scoped_failures.len(), 2);
        assert!(report.snapshot.scoped_failures.iter().all(|failure| {
            failure.operation == OperationKind::Capability
                && matches!(failure.scope, FailureScope::Capability { ref id, version: 2, .. } if id == "power.battery")
        }));
        assert!(
            report
                .snapshot
                .devices
                .iter()
                .all(|device| { device.connection == Connection::Connected })
        );
    }

    use periph_hal::lighting::Lighting;

    struct SlowLight {
        started: Sender<()>,
        release: Arc<Mutex<Receiver<()>>>,
    }
    impl periph_hal::lighting::LightingController for SlowLight {
        fn read_state(&self) -> Result<bool, HardwareError> {
            self.started.send(()).unwrap();
            let _ = self.release.lock().unwrap().recv();
            Ok(true)
        }
        fn set_state(&self, _: bool) -> Result<(), HardwareError> {
            Ok(())
        }
    }
    struct LightingBackend {
        started: Sender<()>,
        release: Arc<Mutex<Receiver<()>>>,
    }
    impl Backend for LightingBackend {
        fn id(&self) -> &str {
            "test.lighting"
        }
        fn inventory(&mut self) -> Result<Vec<DeviceDescriptor>, HardwareError> {
            Ok(Vec::new())
        }
        fn discover(&mut self) -> Result<Vec<Device>, HardwareError> {
            let mut device = make_device(
                "test.lighting",
                "keyboard",
                "slow keyboard",
                Battery::new(|| Ok(reading(50))),
            );
            device.connection = Connection::Connected;
            device
                .capabilities
                .insert(
                    "main",
                    Lighting::new(SlowLight {
                        started: self.started.clone(),
                        release: self.release.clone(),
                    }),
                )
                .unwrap();
            Ok(vec![device])
        }
    }

    #[test]
    fn slow_lighting_does_not_block_fast_battery_or_refresh_cancellation() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut hardware = Hardware::new();
        hardware.set_operation_timeout(Duration::from_secs(2));
        hardware
            .register_factory("test.lighting", move || LightingBackend {
                started: started_tx,
                release: Arc::new(Mutex::new(release_rx)),
            })
            .unwrap();
        hardware
            .register_factory("test.bluetooth", move || BluetoothBackend {
                hid_battery_started: started_rx,
            })
            .unwrap();
        let cancelled = AtomicBool::new(false);
        let mut refresh = RefreshSession::new(&mut hardware, &cancelled);
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut saw_fast = false;
        while Instant::now() < deadline && !saw_fast {
            if let Some(report) = refresh.poll() {
                saw_fast = report
                    .batteries
                    .iter()
                    .any(|item| item.key.backend == "test.bluetooth");
                assert!(report.lighting_states.is_empty());
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert!(saw_fast, "lighting read blocked the refresh coordinator");
        cancelled.store(true, Ordering::Release);
        assert!(refresh.poll().is_none());
        assert!(refresh.is_done());
        assert!(refresh.pending_controls.is_empty());
        release_tx.send(()).unwrap();
    }

    #[test]
    fn lighting_failure_keeps_connection_and_uses_its_own_failure_scope() {
        struct Unavailable;
        impl periph_hal::lighting::LightingController for Unavailable {
            fn read_state(&self) -> Result<bool, HardwareError> {
                Err(HardwareError::new(ErrorKind::Timeout, "test timeout"))
            }
            fn set_state(&self, _: bool) -> Result<(), HardwareError> {
                Ok(())
            }
        }
        let mut device = make_device(
            "test.controls",
            "keyboard",
            "keyboard",
            Battery::new(|| Ok(reading(50))),
        );
        device.connection = Connection::Connected;
        device
            .capabilities
            .insert("main", Lighting::new(Unavailable))
            .unwrap();
        let report = from_snapshot(Snapshot {
            devices: vec![device],
            ..Snapshot::default()
        });
        assert!(report.lighting_states.is_empty());
        assert_eq!(report.snapshot.devices[0].connection, Connection::Connected);
        assert!(
            matches!(&report.snapshot.scoped_failures[0].scope, FailureScope::Capability { id, .. } if id == Lighting::ID)
        );
    }
}
