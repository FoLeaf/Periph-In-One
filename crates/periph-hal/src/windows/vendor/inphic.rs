//! Inphic 2.4G mice over vendor collections (1D57:FA60 usage_page 0x0A/0x0B and
//! 1D57:FA65 FF00:0001). Notifications ride on input report 0x03. Zero writes
//! on the discovery/battery path.

use crate::{Connection, ErrorKind, HardwareError};
use crate::{
    protocol,
    vendor::{Outcome, TargetCardinality, VendorAdapter},
    windows::{
        topology::container_id,
        transport::{drain, open_pair},
    },
};
use hidapi::{DeviceInfo, HidApi};
use std::time::{Duration, Instant};

const VID: u16 = 0x1d57;
const PID_FA60: u16 = 0xfa60;
const PID_FA65: u16 = 0xfa65;

pub struct InphicMouse;

impl InphicMouse {
    pub fn is_match(info: &DeviceInfo) -> bool {
        if info.vendor_id() != VID {
            return false;
        }
        match info.product_id() {
            PID_FA60 => (info.usage_page() == 0x000a || info.usage_page() == 0x000b) && info.usage() == 0,
            PID_FA65 => info.usage_page() == 0xff00 && info.usage() == 0x0001,
            _ => false,
        }
    }
}

impl VendorAdapter for InphicMouse {
    fn matches(&self, info: &DeviceInfo) -> bool {
        Self::is_match(info)
    }

    fn kind(&self) -> &'static str {
        "mouse"
    }

    fn target_cardinality(&self, _info: &DeviceInfo) -> TargetCardinality {
        TargetCardinality::Single
    }

    fn read(&self, api: &HidApi, info: &DeviceInfo) -> Outcome {
        match probe_notification(api, info) {
            Ok(outcome) => outcome,
            Err(error) => Outcome::failure(error),
        }
    }

    fn discover(&self, api: &HidApi, info: &DeviceInfo) -> Vec<Outcome> {
        vec![self.read(api, info)]
    }
}

fn probe_notification(api: &HidApi, info: &DeviceInfo) -> Result<Outcome, HardwareError> {
    let container = container_id(info);
    let interfaces: Vec<DeviceInfo> = api
        .device_list()
        .filter(|d| InphicMouse::is_match(d) && container_id(d) == container)
        .cloned()
        .collect();

    // Prefer notification endpoint (usage_page 0x000A on FA60, or FF00:0001 on FA65)
    let notif_info = interfaces
        .iter()
        .find(|i| {
            (i.product_id() == PID_FA60 && i.usage_page() == 0x000a)
                || (i.product_id() == PID_FA65 && i.usage_page() == 0xff00)
        })
        .or_else(|| interfaces.first())
        .ok_or_else(|| {
            HardwareError::new(
                ErrorKind::Unavailable,
                "inphic notification interface missing",
            )
        })?;

    let (reader, _writer) = open_pair(api, notif_info)?;
    drain(&reader);

    let mut buffer = [0u8; 128];
    let end = Instant::now() + Duration::from_millis(150);
    while Instant::now() < end {
        let n = reader
            .read_timeout(&mut buffer, 50)
            .map_err(HardwareError::transport)?;
        if n == 0 {
            continue;
        }
        if let Some(battery) = protocol::inphic_parse_battery(&buffer[..n]) {
            return Ok(Outcome {
                battery: Some(battery.level),
                connection: Connection::Connected,
                hardware_name: Some(battery.model.display_name().into()),
                ..Outcome::default()
            });
        }
    }

    // When no fresh report arrives or the mouse is asleep, report Unavailable
    // without resetting historical readings or claiming connection.
    Ok(Outcome {
        battery: None,
        connection: Connection::Unknown,
        hardware_name: Some("英菲克无线鼠标".into()),
        issue: Some(HardwareError::new(
            ErrorKind::Unavailable,
            "鼠标未上报有效电量（可能关机或休眠）",
        )),
        ..Outcome::default()
    })
}
