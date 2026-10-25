//! DARKRIM SHADOW headset (3654:6901): CRC16-guarded TLV queries.

use crate::{Connection, ErrorKind, HardwareError};
use crate::{
    protocol,
    vendor::{Outcome, VendorAdapter},
    windows::transport::{drain, open_pair, send},
};
use hidapi::{DeviceInfo, HidApi};
use std::time::{Duration, Instant};

pub struct DarkrimHeadset;

impl VendorAdapter for DarkrimHeadset {
    fn matches(&self, info: &DeviceInfo) -> bool {
        info.vendor_id() == 0x3654
            && info.product_id() == 0x6901
            && info.usage_page() == 0xff02
            && info.usage() == 2
    }

    fn kind(&self) -> &'static str {
        "headset"
    }

    fn read(&self, api: &HidApi, info: &DeviceInfo) -> Outcome {
        match query(api, info) {
            Ok((battery, name)) => Outcome {
                connection: if battery.is_some() || !name.is_empty() {
                    Connection::Connected
                } else {
                    Connection::Unknown
                },
                issue: battery.is_none().then(|| {
                    HardwareError::new(ErrorKind::Timeout, "headset battery did not respond")
                }),
                battery,
                hardware_name: (!name.is_empty()).then_some(name),
                ..Outcome::default()
            },
            Err(error) => Outcome::failure(error),
        }
    }
}

fn query(api: &HidApi, info: &DeviceInfo) -> Result<(Option<u8>, String), HardwareError> {
    let (reader, writer) = open_pair(api, info)?;
    let mut battery = None;
    let mut name = String::new();
    let mut buffer = [0; 256];
    for (seq, kind) in [(0, 1), (1, 2)] {
        drain(&reader);
        send(&writer, &protocol::darkrim_query(seq, kind))?;
        let end = Instant::now() + Duration::from_millis(450);
        while Instant::now() < end {
            let n = reader
                .read_timeout(&mut buffer, 100)
                .map_err(HardwareError::transport)?;
            let report = protocol::restore_darkrim_report_id(&buffer[..n]);
            if let Some(tlvs) = protocol::darkrim_tlvs(&report, seq) {
                for (key, value) in tlvs {
                    if key == 1 && !value.is_empty() && value[0] <= 100 {
                        battery = Some(value[0]);
                    }
                    if key == 2 {
                        name = String::from_utf8_lossy(value)
                            .trim_matches(char::from(0))
                            .trim()
                            .to_string();
                    }
                }
            }
        }
    }
    Ok((battery, name))
}
