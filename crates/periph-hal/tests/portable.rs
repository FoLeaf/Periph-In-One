use periph_hal::{
    Backend, Capabilities, Capability, Connection, Device, DeviceDescriptor, DeviceKey, ErrorKind,
    Hardware, HardwareError,
    battery::{Battery, BatteryState, Percent},
};

// Deliberately defined downstream: adding a domain must not edit HAL enums.
#[derive(Debug, PartialEq)]
struct CameraControl {
    exposure: u32,
}
impl Capability for CameraControl {
    const ID: &'static str = "camera.exposure";
}

fn descriptor() -> DeviceDescriptor {
    DeviceDescriptor {
        key: DeviceKey {
            backend: "fake.camera".into(),
            local: "camera-1".into(),
        },
        name: "Camera".into(),
        roles: vec!["video.capture".into()],
        identifiers: vec![],
        identities: vec![],
        links: vec![],
        target: String::new(),
    }
}

struct Camera;
impl Backend for Camera {
    fn id(&self) -> &str {
        "fake.camera"
    }
    fn inventory(&mut self) -> Result<Vec<DeviceDescriptor>, HardwareError> {
        Ok(vec![descriptor()])
    }
    fn discover(&mut self) -> Result<Vec<Device>, HardwareError> {
        let mut capabilities = Capabilities::default();
        capabilities.insert("sensor", CameraControl { exposure: 12 })?;
        Ok(vec![Device {
            descriptor: descriptor(),
            connection: Connection::Connected,
            capabilities,
            issue: None,
        }])
    }
}

struct Broken;
impl Backend for Broken {
    fn id(&self) -> &str {
        "fake.broken"
    }
    fn inventory(&mut self) -> Result<Vec<DeviceDescriptor>, HardwareError> {
        Err(HardwareError::new(ErrorKind::PermissionDenied, "denied"))
    }
    fn discover(&mut self) -> Result<Vec<Device>, HardwareError> {
        Err(HardwareError::new(
            ErrorKind::Transport,
            "disconnected source",
        ))
    }
}

#[test]
fn downstream_domain_needs_neither_battery_nor_core_changes() {
    let mut hardware = Hardware::new();
    hardware.register(Camera).unwrap();
    let snapshot = hardware.discover();
    let device = &snapshot.devices[0];
    assert_eq!(
        device
            .capabilities
            .get::<CameraControl>("sensor")
            .unwrap()
            .exposure,
        12
    );
    assert!(device.capabilities.get::<Battery>("main").is_none());
    assert_eq!(device.descriptor.roles, ["video.capture"]);
}

#[test]
fn failed_backend_does_not_hide_success_or_claim_complete_inventory() {
    let mut hardware = Hardware::new();
    hardware.register(Broken).unwrap();
    hardware.register(Camera).unwrap();
    let snapshot = hardware.discover();
    assert_eq!(snapshot.devices.len(), 1);
    assert_eq!(snapshot.completed, ["fake.camera"]);
    assert_eq!(snapshot.failures[0].backend, "fake.broken");
    let inventory = hardware.inventory();
    assert_eq!(inventory.devices.len(), 1);
    assert_eq!(
        inventory.failures[0].error.kind,
        ErrorKind::PermissionDenied
    );
    assert_eq!(inventory.completed, ["fake.camera"]);
}

#[test]
fn components_and_capability_ownership_are_independent() {
    let mut capabilities = Capabilities::default();
    capabilities
        .insert("left", CameraControl { exposure: 1 })
        .unwrap();
    capabilities
        .insert("right", CameraControl { exposure: 2 })
        .unwrap();
    assert_eq!(
        capabilities.get::<CameraControl>("left").unwrap().exposure,
        1
    );
    assert_eq!(
        capabilities.get::<CameraControl>("right").unwrap().exposure,
        2
    );
    assert_eq!(
        capabilities
            .insert("left", CameraControl { exposure: 3 })
            .unwrap_err()
            .kind,
        ErrorKind::Conflict
    );
}

#[test]
fn battery_zero_is_valid_and_unknown_is_not_zero() {
    assert_eq!(Percent::new(0).unwrap().get(), 0);
    assert!(Percent::new(101).is_err());
    let unknown = BatteryState::Unavailable(HardwareError::new(ErrorKind::Timeout, "no reply"));
    assert_ne!(unknown, BatteryState::Unsupported);
}

#[test]
fn duplicate_backend_registration_is_rejected() {
    let mut hardware = Hardware::new();
    hardware.register(Camera).unwrap();
    assert_eq!(
        hardware.register(Camera).unwrap_err().kind,
        ErrorKind::Conflict
    );
}
