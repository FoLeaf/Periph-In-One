//! Windows HID drivers. These types never cross the portable HAL boundary.
//! Wire codecs are shared with probes through `protocol`.
mod atk;
mod atk_v2;
mod atk_v2_dongle;
mod aula;
mod bigbigwon;
mod darkrim;
mod ikf;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetCardinality {
    Single,
    Multiple,
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
    /// Protocol topology, independent of names or discovery order.
    fn target_cardinality(&self, _info: &DeviceInfo) -> TargetCardinality {
        TargetCardinality::Single
    }
    /// Identify topology/identity only. Battery queries belong to read_target.
    fn discover(&self, _api: &HidApi, _info: &DeviceInfo) -> Vec<Outcome> {
        vec![Outcome::default()]
    }
    fn read_target(&self, api: &HidApi, info: &DeviceInfo, _slot: Option<u8>) -> Outcome {
        self.read(api, info)
    }
    fn read(&self, api: &HidApi, info: &DeviceInfo) -> Outcome;
    fn read_all(&self, api: &HidApi, info: &DeviceInfo) -> Vec<Outcome> {
        vec![self.read(api, info)]
    }
}

pub const ADAPTERS: &[&dyn VendorAdapter] = &[
    &atk::AtkMouse,
    &atk_v2::AtkV2Mouse,
    &atk_v2_dongle::AtkV2Dongle,
    &darkrim::DarkrimHeadset,
    &ikf::IkfHeadset,
    &kzzi::KzziKeyboard,
    &bigbigwon::BigbigwonGamepad,
    &logitech::LogitechReceiver,
    &aula::AulaCommandKeyboard,
];
