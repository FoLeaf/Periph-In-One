mod bluetooth;
pub(crate) mod hid;
mod topology;
pub(crate) mod transport;
pub(crate) mod vendor;

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

use crate::{Backend, Device, DeviceDescriptor, DiscoveryReport, HardwareError};

pub struct HidBackend;
impl Backend for HidBackend {
    fn id(&self) -> &str {
        "windows.hid"
    }
    fn inventory(&mut self) -> Result<Vec<DeviceDescriptor>, HardwareError> {
        hid::inventory()
    }
    fn discover(&mut self) -> Result<Vec<Device>, HardwareError> {
        hid::discover()
    }
    fn discovery_report(&mut self) -> Result<DiscoveryReport, HardwareError> {
        hid::discovery_report()
    }
}

pub struct BluetoothAudioBackend;
impl Backend for BluetoothAudioBackend {
    fn id(&self) -> &str {
        "windows.bluetooth.audio"
    }
    fn inventory(&mut self) -> Result<Vec<DeviceDescriptor>, HardwareError> {
        bluetooth::inventory()
    }
    fn discover(&mut self) -> Result<Vec<Device>, HardwareError> {
        bluetooth::discover()
    }
}
