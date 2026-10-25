//! KZZI keyboards behind a Compx 2.4G receiver (3554:FA09). The receiver
//! answers identity itself; battery queries are relayed to the keyboard and
//! only models whose firmware implements them reply. Models verified to keep
//! their battery private report Unsupported, so a linked keyboard without a
//! reading is told apart from one that answered nothing. The interface only
//! answers requests written through the control endpoint (hidapi fails there
//! with error 1784), so the exchange runs on `windows::transport::KzziLink` raw handles.

use crate::{Connection, ErrorKind, HardwareError};
use crate::{
    protocol,
    vendor::{Outcome, VendorAdapter},
    windows::transport::KzziLink,
};
use hidapi::{DeviceInfo, HidApi};
use std::time::{Duration, Instant};

pub struct KzziKeyboard;

impl VendorAdapter for KzziKeyboard {
    fn matches(&self, info: &DeviceInfo) -> bool {
        info.vendor_id() == 0x3554
            && info.product_id() == 0xfa09
            && info.usage_page() == 0xff02
            && info.usage() == 2
    }

    fn kind(&self) -> &'static str {
        "keyboard"
    }

    fn read(&self, _api: &HidApi, info: &DeviceInfo) -> Outcome {
        match probe(info) {
            Ok((battery, name, unsupported)) => Outcome {
                connection: if battery.is_some() || unsupported {
                    Connection::Connected
                } else {
                    Connection::Unknown
                },
                issue: (battery.is_none() && !unsupported).then(|| {
                    HardwareError::new(ErrorKind::Timeout, "keyboard battery did not respond")
                }),
                battery,
                unsupported,
                hardware_name: (!name.is_empty()).then_some(name),
                ..Outcome::default()
            },
            Err(error) => Outcome::failure(error),
        }
    }
}

fn probe(info: &DeviceInfo) -> Result<(Option<u8>, String, bool), HardwareError> {
    let link = KzziLink::open(&info.path().to_string_lossy())?;
    let mut buffer = [0u8; 64];
    let mut name = String::new();
    let mut private = false;
    for _ in 0..3 {
        link.drain();
        link.send(&protocol::kzzi_identity_request())?;
        let end = Instant::now() + Duration::from_millis(600);
        while name.is_empty() && Instant::now() < end {
            if let Some(n) = link.read(&mut buffer, Duration::from_millis(120))
                && let Some(psd) = protocol::kzzi_identity(&buffer[..n])
            {
                private = protocol::kzzi_battery_private(&psd);
                name = protocol::kzzi_model_name(&psd).unwrap_or("").to_string();
            }
        }
        if !name.is_empty() {
            break;
        }
    }
    for _ in 0..3 {
        link.drain();
        link.send(&protocol::kzzi_request(0x4a))?;
        let end = Instant::now() + Duration::from_millis(900);
        while Instant::now() < end {
            if let Some(n) = link.read(&mut buffer, Duration::from_millis(120))
                && let Some((level, _)) = protocol::kzzi_battery(&buffer[..n])
            {
                return Ok((Some(level), name, private));
            }
        }
    }
    Ok((None, name, private))
}
