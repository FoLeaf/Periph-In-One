//! Windows HID drivers. These types never cross the portable HAL boundary.
//! Wire codecs are shared with probes through `protocol`.
mod atk;
mod atk_v2;
mod aula;
mod bigbigwon;
mod darkrim;
mod kzzi;
mod logitech;

use crate::{Connection, HardwareError};
use hidapi::{DeviceInfo, HidApi};

#[derive(Debug, Default)]
pub struct Outcome {
    pub battery: Option<u8>,
    pub unsupported: bool,
    pub connection: Connection,
    pub hardware_name: Option<String>,
    pub uid: String,
    pub slot: Option<u8>,
    pub issue: Option<HardwareError>,
}

impl Outcome {
    pub fn failure(error: HardwareError) -> Self {
        Self {
            issue: Some(error),
            ..Self::default()
        }
    }
}

pub trait VendorAdapter {
    fn matches(&self, info: &DeviceInfo) -> bool;
    fn kind(&self) -> &'static str;
    fn read(&self, api: &HidApi, info: &DeviceInfo) -> Outcome;
    fn read_all(&self, api: &HidApi, info: &DeviceInfo) -> Vec<Outcome> {
        vec![self.read(api, info)]
    }
}

pub const ADAPTERS: &[&dyn VendorAdapter] = &[
    &atk::AtkMouse,
    &atk_v2::AtkV2Mouse,
    &darkrim::DarkrimHeadset,
    &kzzi::KzziKeyboard,
    &bigbigwon::BigbigwonGamepad,
    &logitech::LogitechReceiver,
    &aula::AulaCommandKeyboard,
];
