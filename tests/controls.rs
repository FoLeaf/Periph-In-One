use periph_hal::{
    Capabilities, Connection, CoverageScope, Device, DeviceDescriptor, DeviceKey, HardwareError,
    Snapshot,
    lighting::{Lighting, LightingController},
};
use periph_in_one::model;

struct Light;
impl LightingController for Light {
    fn read_state(&self) -> Result<bool, HardwareError> {
        Ok(true)
    }
    fn set_state(&self, _: bool) -> Result<(), HardwareError> {
        Ok(())
    }
}

#[test]
fn refresh_preserves_pending_controls_but_removes_missing_live_capabilities() {
    let mut capabilities = Capabilities::default();
    capabilities.insert("main", Lighting::new(Light)).unwrap();
    let mut snapshot = Snapshot {
        devices: vec![Device {
            descriptor: DeviceDescriptor {
                key: DeviceKey {
                    backend: "test.controls".into(),
                    local: "keyboard".into(),
                },
                ..DeviceDescriptor::default()
            },
            connection: Connection::Connected,
            capabilities,
            issue: None,
        }],
        coverage: vec![CoverageScope::Backend {
            backend: "test.controls".into(),
        }],
        ..Snapshot::default()
    };
    let mut first = model::merge_snapshot(&[], &snapshot).devices;
    first[0].lighting_pending = true;
    first[0].saved.last_lighting_on = Some(true);
    snapshot.devices[0].capabilities = Capabilities::default();
    let next = model::merge_snapshot(&first, &snapshot).devices;
    assert!(next[0].lighting_pending);
    assert!(next[0].lighting.is_none());
    assert_eq!(next[0].current_lighting_on, Some(true));
}
