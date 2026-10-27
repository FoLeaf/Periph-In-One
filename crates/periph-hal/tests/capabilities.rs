use periph_hal::{
    Backend, Capabilities, Connection, Device, DeviceDescriptor, DeviceFacts, DeviceKey, ErrorKind,
    Hardware, HardwareError,
    battery::{Battery, BatteryReading, BatteryState, Percent, Sample, SampleOrigin},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct CounterBackend {
    reads: Arc<AtomicUsize>,
    fail: Arc<AtomicBool>,
}
impl Backend for CounterBackend {
    fn id(&self) -> &str {
        "test.counter"
    }
    fn inventory(&mut self) -> Result<Vec<DeviceDescriptor>, HardwareError> {
        Ok(vec![descriptor()])
    }
    fn discover(&mut self) -> Result<Vec<Device>, HardwareError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(HardwareError::new(ErrorKind::Transport, "discovery failed"));
        }
        let reads = self.reads.clone();
        let mut capabilities = Capabilities::default();
        capabilities.insert(
            "main",
            Battery::new(move || {
                let level = reads.fetch_add(1, Ordering::SeqCst) as u8;
                Ok(BatteryReading {
                    state: BatteryState::Value(Sample {
                        level: Percent::new(level)?,
                        origin: SampleOrigin::DeviceResponse,
                        observed_at: std::time::SystemTime::now(),
                        sampled_at: None,
                    }),
                    facts: DeviceFacts {
                        connection: Some(Connection::Connected),
                        ..DeviceFacts::default()
                    },
                })
            }),
        )?;
        Ok(vec![Device {
            descriptor: descriptor(),
            connection: Connection::Unknown,
            capabilities,
            issue: None,
        }])
    }
}

fn descriptor() -> DeviceDescriptor {
    DeviceDescriptor {
        key: DeviceKey {
            backend: "test.counter".into(),
            local: "target".into(),
        },
        ..DeviceDescriptor::default()
    }
}

fn setup() -> (Hardware, Arc<AtomicUsize>, Arc<AtomicBool>) {
    let reads = Arc::new(AtomicUsize::new(0));
    let fail = Arc::new(AtomicBool::new(false));
    let mut hardware = Hardware::new();
    hardware
        .register(CounterBackend {
            reads: reads.clone(),
            fail: fail.clone(),
        })
        .unwrap();
    (hardware, reads, fail)
}

#[test]
fn discovery_and_inventory_do_not_sample_and_reads_are_independent() {
    let (mut hardware, reads, _) = setup();
    assert_eq!(hardware.inventory().devices.len(), 1);
    let snapshot = hardware.discover();
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    let port = snapshot.devices[0]
        .capabilities
        .get::<Battery>("main")
        .unwrap();
    let first = port.read().unwrap();
    let second = port.read().unwrap();
    assert_eq!(reads.load(Ordering::SeqCst), 2);
    assert!(matches!(first.state, BatteryState::Value(sample) if sample.level.get() == 0));
    assert!(matches!(second.state, BatteryState::Value(sample) if sample.level.get() == 1));
    assert_eq!(snapshot.devices[0].connection, Connection::Unknown);
}

#[test]
fn rediscovery_revokes_old_ports_and_their_clones_before_io() {
    let (mut hardware, reads, _) = setup();
    let first = hardware.discover();
    let old = first.devices[0]
        .capabilities
        .get::<Battery>("main")
        .unwrap()
        .clone();
    let clone = old.clone();
    let second = hardware.discover();
    assert_eq!(old.read().unwrap_err().kind, ErrorKind::Unavailable);
    assert_eq!(clone.read().unwrap_err().kind, ErrorKind::Unavailable);
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    second.devices[0]
        .capabilities
        .get::<Battery>("main")
        .unwrap()
        .read()
        .unwrap();
    assert_eq!(reads.load(Ordering::SeqCst), 1);
}

#[test]
fn dropping_owner_or_failed_rediscovery_revokes_ports() {
    let (mut hardware, reads, fail) = setup();
    let snapshot = hardware.discover();
    let old = snapshot.devices[0]
        .capabilities
        .get::<Battery>("main")
        .unwrap()
        .clone();
    fail.store(true, Ordering::SeqCst);
    assert_eq!(hardware.discover().failures.len(), 1);
    assert!(old.read().is_err());
    fail.store(false, Ordering::SeqCst);
    let fresh = hardware.discover();
    let port = fresh.devices[0]
        .capabilities
        .get::<Battery>("main")
        .unwrap()
        .clone();
    drop(hardware);
    assert!(port.read().is_err());
    assert_eq!(reads.load(Ordering::SeqCst), 0);
}

struct TestDpiController {
    stage: Arc<AtomicUsize>,
}

impl periph_hal::dpi::DpiController for TestDpiController {
    fn read_stage(&self) -> Result<Option<u8>, HardwareError> {
        Ok(Some(self.stage.load(Ordering::SeqCst) as u8))
    }
    fn set_stage(&self, stage: u8) -> Result<(), HardwareError> {
        self.stage.store(stage as usize, Ordering::SeqCst);
        Ok(())
    }
}

struct TestLightingController {
    on: Arc<AtomicBool>,
}

impl periph_hal::lighting::LightingController for TestLightingController {
    fn read_state(&self) -> Result<bool, HardwareError> {
        Ok(self.on.load(Ordering::SeqCst))
    }
    fn set_state(&self, on: bool) -> Result<(), HardwareError> {
        self.on.store(on, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn dpi_and_lighting_capabilities_support_state_manipulation_and_invalidation() {
    let dpi_stage = Arc::new(AtomicUsize::new(2));
    let dpi = periph_hal::dpi::Dpi::new(
        vec![400, 800, 1200, 1600],
        TestDpiController {
            stage: dpi_stage.clone(),
        },
    );
    assert_eq!(dpi.stages(), &[400, 800, 1200, 1600]);
    assert_eq!(dpi.read_stage().unwrap(), Some(2));
    dpi.set_stage(4).unwrap();
    assert_eq!(dpi_stage.load(Ordering::SeqCst), 4);
    assert_eq!(dpi.read_stage().unwrap(), Some(4));

    dpi.close();
    assert!(dpi.read_stage().is_err());
    assert!(dpi.set_stage(1).is_err());

    let light_state = Arc::new(AtomicBool::new(false));
    let light = periph_hal::lighting::Lighting::new(TestLightingController {
        on: light_state.clone(),
    });
    assert!(!light.read_state().unwrap());
    light.set_state(true).unwrap();
    assert!(light_state.load(Ordering::SeqCst));
    assert!(light.read_state().unwrap());

    light.close();
    assert!(light.read_state().is_err());
    assert!(light.set_state(false).is_err());
}

#[test]
fn explicit_close_revokes_all_clones_without_destroying_device_metadata() {
    let (mut hardware, reads, _) = setup();
    let snapshot = hardware.discover();
    let port = snapshot.devices[0]
        .capabilities
        .get::<Battery>("main")
        .unwrap();
    let clone = port.clone();
    port.close();
    assert!(clone.read().is_err());
    assert_eq!(snapshot.devices[0].descriptor.key.local, "target");
    assert_eq!(reads.load(Ordering::SeqCst), 0);
}
