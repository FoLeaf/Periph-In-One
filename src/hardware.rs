//! Coordinate device-core facts and optional battery-domain observations.
use crate::{
    battery::{Batteries, Observation},
    model::{self, Device},
};
use periph_hal::{
    BackendDiscoveryUpdate, BackendFailure, Capability, DiscoveryPoll, DiscoveryStream, Hardware,
    Snapshot,
    battery::{Battery, BatteryReadRequest, BatteryState},
};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct ScanReport {
    pub snapshot: Snapshot,
    pub batteries: Vec<Observation>,
}

struct PendingBattery {
    device_index: usize,
    key: periph_hal::DeviceKey,
    component: String,
    request: BatteryReadRequest,
}

/// Progressive scan state shared by the UI worker and HAL's per-backend
/// discovery stream. `poll` never waits for a particular backend or battery.
pub struct RefreshSession<'a> {
    discovery: DiscoveryStream<'a>,
    snapshot: Snapshot,
    observations: Vec<Observation>,
    pending: Vec<PendingBattery>,
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
                    reading
                        .facts
                        .apply_to(&mut self.snapshot.devices[pending.device_index]);
                    reading.state
                }
                Err(error) => {
                    self.snapshot.devices[pending.device_index].issue = Some(error.clone());
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

        if self.discovery.is_complete() && self.pending.is_empty() {
            self.done = true;
        }
        changed.then(|| ScanReport {
            snapshot: self.snapshot.clone(),
            batteries: self.observations.clone(),
        })
    }

    fn accept_backend(&mut self, update: BackendDiscoveryUpdate) {
        match update.result {
            Ok(devices) => {
                let first = self.snapshot.devices.len();
                self.snapshot.devices.extend(devices);
                self.snapshot.completed.push(update.backend);
                for device_index in first..self.snapshot.devices.len() {
                    self.queue_batteries(device_index);
                }
            }
            Err(error) => self.snapshot.failures.push(BackendFailure {
                backend: update.backend,
                error,
            }),
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
            if let Some(battery) = device.capabilities.get::<Battery>(&component) {
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
        !self.snapshot.failures.is_empty()
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
            if let Some(battery) = device.capabilities.get::<Battery>(&component) {
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
                reading
                    .facts
                    .apply_to(&mut snapshot.devices[operation.device_index]);
                reading.state
            }
            Err(error) => {
                snapshot.devices[operation.device_index].issue = Some(error.clone());
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
    ScanReport {
        snapshot,
        batteries,
    }
}

pub fn merge_report(
    previous: &[Device],
    batteries: &mut Batteries,
    report: ScanReport,
    now: u64,
) -> Vec<Device> {
    let merged = model::merge_snapshot(previous, &report.snapshot);
    batteries.apply(&report.batteries, &merged, now);
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
}
