//! Aula keyboards on the command-protocol dongles (VID 0x0C45): the wireless
//! SDK channel of Aula Hub 2.2.3 — F87ProV2/F108ProV2 series dongles (FEFC)
//! plus the FEFE/FEF9 siblings — enumerated as usage page FF60/FF68/FF80
//! collections with usage 0x01/0x61/0x71. Battery is a read-only 32-byte
//! command exchange (cmd 0x11, fallback 0x51, mode 0x16) built by
//! `protocol::aula_command_request`; the reply carries level and charging
//! flag. Frames and match rules come from the deobfuscated renderer device
//! table and SDK (see docs/ADAPTATION.md §7.5); no unit has been probed on
//! hardware, so this stays one tier below measured.

use crate::{Connection, ErrorKind, HardwareError};
use crate::{
    protocol,
    vendor::{Outcome, VendorAdapter},
    windows::transport::{drain, open_pair, send},
};
use hidapi::{DeviceInfo, HidApi};
use std::time::{Duration, Instant};

pub struct AulaCommandKeyboard;

impl VendorAdapter for AulaCommandKeyboard {
    fn matches(&self, info: &DeviceInfo) -> bool {
        // The dongle PIDs and SDK-channel usage pages recorded in the vendor
        // device table; the alias pages (FF67/FF70) cover firmware
        // personalities that enumerate one step off the primary page.
        info.vendor_id() == 0x0c45
            && matches!(info.product_id(), 0xfef9 | 0xfefc | 0xfefe)
            && matches!(
                info.usage_page(),
                0xff60 | 0xff67 | 0xff68 | 0xff70 | 0xff80
            )
            && matches!(info.usage(), 0x01 | 0x61 | 0x71)
    }

    fn kind(&self) -> &'static str {
        "keyboard"
    }

    fn read(&self, api: &HidApi, info: &DeviceInfo) -> Outcome {
        match probe(api, info) {
            Ok(Some((level, _charging))) => Outcome {
                battery: Some(level),
                connection: Connection::Connected,
                ..Outcome::default()
            },
            Ok(None) => Outcome::failure(HardwareError::new(
                ErrorKind::Timeout,
                "keyboard did not respond",
            )),
            Err(error) => Outcome::failure(error),
        }
    }
}

/// One battery exchange per command byte; the hub tries 0x11 first and
/// re-asks some models with 0x51. A keyboard that is off stays silent and
/// the dongle answers nothing on its behalf, so silence means "check the
/// keyboard" here. The exchange needs no reply CRC: the vendor client also
/// trusts only the echoed cmd/mode and the status byte.
fn probe(api: &HidApi, info: &DeviceInfo) -> Result<Option<(u8, u8)>, HardwareError> {
    let (reader, writer) = open_pair(api, info)?;
    let mut buffer = [0u8; 64];
    for cmd in [protocol::AULA_CMD_BATTERY, protocol::AULA_CMD_BATTERY_ALT] {
        let mut wired = Vec::with_capacity(33);
        wired.push(0);
        wired.extend_from_slice(&protocol::aula_command_request(
            cmd,
            protocol::AULA_MODE_BATTERY,
        ));
        for _ in 0..3 {
            drain(&reader);
            send(&writer, &wired)?;
            let end = Instant::now() + Duration::from_millis(600);
            while Instant::now() < end {
                if let Ok(n) = reader.read_timeout(&mut buffer, 120)
                    && n > 0
                    && let Some(battery) = protocol::aula_command_battery(&buffer[..n], cmd)
                {
                    return Ok(Some(battery));
                }
            }
        }
    }
    Ok(None)
}
