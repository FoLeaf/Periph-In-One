//! BIGBIG WON controllers (GALE family and other Zikway models) over the
//! FF7A vendor interface. GetDevicePowerInfo answers only while the controller
//! is linked and running its composite gamepad personality; while linked,
//! GetUUID reports the 8-byte per-unit identity that anchors the record
//! across personalities and ports.

use crate::{Connection, ErrorKind, HardwareError};
use crate::{
    protocol,
    vendor::{Outcome, VendorAdapter},
    windows::transport::{drain, open_pair, send},
};
use hidapi::{DeviceInfo, HidApi, HidDevice};
use std::time::{Duration, Instant};

const VID: u16 = 0x413d;
const PID_ACTIVE: u16 = 0x2104;
const PID_IDLE: u16 = 0x2106;
const PID_WIRED: u16 = 0x2114;

// The idle endpoint stays enumerated without proving a controller is linked
// or charging. Keep this path passive: the verified firmware does not answer.
fn idle_endpoint() -> Outcome {
    Outcome {
        hardware_name: Some("BIGBIG WON 手柄".into()),
        ..Outcome::default()
    }
}

pub struct BigbigwonGamepad;

impl VendorAdapter for BigbigwonGamepad {
    fn matches(&self, info: &DeviceInfo) -> bool {
        // Restrict the shared FF7A interface to known firmware personalities.
        info.vendor_id() == VID
            && info.usage_page() == 0xff7a
            && info.usage() == 1
            && matches!(info.product_id(), PID_ACTIVE | PID_IDLE | PID_WIRED)
    }

    fn kind(&self) -> &'static str {
        "gamepad"
    }

    fn read(&self, api: &HidApi, info: &DeviceInfo) -> Outcome {
        if info.product_id() == PID_IDLE {
            return Outcome {
                issue: Some(HardwareError::new(ErrorKind::Unavailable, "电量暂不可用")),
                ..idle_endpoint()
            };
        }
        match probe(api, info) {
            Ok((battery, uid)) => Outcome {
                battery,
                uid,
                connection: Connection::Connected,
                ..Outcome::default()
            },
            Err(error) => Outcome::failure(error),
        }
    }

    fn discover(&self, api: &HidApi, info: &DeviceInfo) -> Vec<Outcome> {
        if info.product_id() == PID_IDLE {
            return vec![idle_endpoint()];
        }
        match open_pair(api, info) {
            Ok((reader, writer)) => {
                let uid = gamepad_uid(&reader, &writer);
                vec![Outcome {
                    connection: if uid.is_empty() {
                        Connection::Unknown
                    } else {
                        Connection::Connected
                    },
                    uid,
                    ..Outcome::default()
                }]
            }
            Err(error) => vec![Outcome::failure(error)],
        }
    }
}

fn probe(api: &HidApi, info: &DeviceInfo) -> Result<(Option<u8>, String), HardwareError> {
    let (reader, writer) = open_pair(api, info)?;
    let mut buffer = [0; 256];
    for _ in 0..3 {
        drain(&reader);
        send(&writer, &protocol::bigbigwon_request())?;
        let end = Instant::now() + Duration::from_millis(650);
        while Instant::now() < end {
            let n = reader
                .read_timeout(&mut buffer, 120)
                .map_err(HardwareError::transport)?;
            if n == 0 {
                continue;
            }
            if let Some(level) = protocol::bigbigwon_battery(&buffer[..n]) {
                return Ok((Some(level), gamepad_uid(&reader, &writer)));
            }
        }
    }
    Err(HardwareError::new(
        ErrorKind::Timeout,
        "gamepad did not respond",
    ))
}

fn gamepad_uid(reader: &HidDevice, writer: &HidDevice) -> String {
    let mut buffer = [0u8; 256];
    for _ in 0..2 {
        drain(reader);
        if send(writer, &protocol::bigbigwon_uuid_request()).is_err() {
            return String::new();
        }
        let end = Instant::now() + Duration::from_millis(400);
        while Instant::now() < end {
            let Ok(n) = reader.read_timeout(&mut buffer, 80) else {
                return String::new();
            };
            if n == 0 {
                continue;
            }
            if let Some(uuid) = protocol::bigbigwon_uuid(&buffer[..n]) {
                return uuid.iter().map(|b| format!("{b:02x}")).collect();
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an attached BIGBIG WON endpoint in PID 2106 idle mode"]
    fn live_idle_endpoint_does_not_claim_connection_or_charging() {
        let api = HidApi::new().unwrap();
        let adapter = BigbigwonGamepad;
        let info = api
            .device_list()
            .find(|info| adapter.matches(info) && info.product_id() == PID_IDLE)
            .expect("connect an idle BIGBIG WON endpoint (413D:2106)");
        let started = Instant::now();
        let discovered = adapter.discover(&api, info);
        assert_eq!(discovered.len(), 1);
        let read = adapter.read(&api, info);
        for outcome in [&discovered[0], &read] {
            assert_eq!(outcome.connection, Connection::Unknown);
            assert!(outcome.uid.is_empty());
            assert!(outcome.battery.is_none());
            assert!(!outcome.unsupported);
        }
        assert!(discovered[0].issue.is_none());
        let error = read.issue.as_ref().unwrap();
        assert_eq!(error.kind, ErrorKind::Unavailable);
        assert_eq!(error.detail, "电量暂不可用");
        println!(
            "idle discovery/read: {:?}, elapsed={:?}",
            read,
            started.elapsed()
        );
    }
}
