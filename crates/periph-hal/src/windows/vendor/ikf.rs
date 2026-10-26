//! iKF/Cm USB headsets on FF00:0001. iKF Audio queries property 0C in a
//! 64-byte output report (ID 2); input report ID 3 carries the answer.
//! Contributor-verified on 38B1:3419 V11 Pro; other PIDs remain unverified.

use crate::{
    Connection, ErrorKind, HardwareError, protocol,
    vendor::{Outcome, VendorAdapter},
    windows::transport::{drain, open_pair, send},
};
use hidapi::{DeviceInfo, HidApi};
use std::time::{Duration, Instant};

pub struct IkfHeadset;

impl VendorAdapter for IkfHeadset {
    fn matches(&self, info: &DeviceInfo) -> bool {
        info.vendor_id() == 0x38b1 && info.usage_page() == 0xff00 && info.usage() == 1
    }

    fn kind(&self) -> &'static str {
        "headset"
    }

    fn read(&self, api: &HidApi, info: &DeviceInfo) -> Outcome {
        match read_battery(api, info) {
            Ok(level) => Outcome {
                battery: Some(level),
                connection: Connection::Connected,
                ..Outcome::default()
            },
            Err(error) => Outcome::failure(error),
        }
    }
}

fn read_battery(api: &HidApi, info: &DeviceInfo) -> Result<u8, HardwareError> {
    let (reader, writer) = open_pair(api, info)?;
    let mut buffer = [0u8; 64];
    for _ in 0..3 {
        drain(&reader);
        send(&writer, &protocol::ikf_battery_query(0))?;
        let end = Instant::now() + Duration::from_millis(900);
        while Instant::now() < end {
            let n = reader
                .read_timeout(&mut buffer, 120)
                .map_err(HardwareError::transport)?;
            if let Some(level) = protocol::ikf_battery(&buffer[..n])
                && buffer[7] == 0
            {
                return Ok(level);
            }
        }
    }
    Err(HardwareError::new(
        ErrorKind::Timeout,
        "headset battery query did not respond",
    ))
}
