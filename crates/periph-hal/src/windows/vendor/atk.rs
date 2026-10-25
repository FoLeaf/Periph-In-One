//! ATK / VXE mice over the FF05/1 vendor interface: the live-verified
//! 373B:1216 nano dongle (with its FF02 fallback interface) plus the wider
//! FF05 mouse/receiver line from the reviewed ATK HUB device table.

use crate::{Connection, ErrorKind, HardwareError};
use crate::{
    protocol,
    vendor::{Outcome, VendorAdapter},
    windows::transport::{drain, open_pair, send},
};
use hidapi::{DeviceInfo, HidApi};
use std::time::{Duration, Instant};

/// FF05/usage-1 mouse and receiver PIDs from the ATK HUB device table
/// (filter-hsnHUoY9.js, review-cleaned 2026-10-25). Peripherals sharing the
/// same usage page - monitors, tablets, handhelds, glasses, speakers,
/// webcams, docks and microphones - are deliberately absent.
const FF05_PIDS: [u16; 89] = [
    0x1154, 0x1155, 0x11db, 0x11dc, 0x1211, 0x1213, 0x1214, 0x1215, 0x1216, 0x1222, 0x1223, 0x1224,
    0x123a, 0x124c, 0x124d, 0x124f, 0x1251, 0x1263, 0x1264, 0x1268, 0x127b, 0x1287, 0x1288, 0x128f,
    0x1290, 0x1291, 0x1299, 0x129a, 0x129e, 0x12af, 0x12b1, 0x12cd, 0x12ce, 0x12d0, 0x12d1, 0x12d2,
    0x12d8, 0x12d9, 0x12dc, 0x12de, 0x12e1, 0x12e3, 0x12ea, 0x12ec, 0x12f1, 0x12f5, 0x1300, 0x1301,
    0x1302, 0x1304, 0x1305, 0x1306, 0x1307, 0x1308, 0x130b, 0x130e, 0x130f, 0x1310, 0x1311, 0x1312,
    0x1314, 0x1315, 0x1324, 0x1331, 0x1332, 0x1337, 0x1338, 0x133c, 0x133e, 0x133f, 0x1343, 0x1344,
    0x1346, 0x1347, 0x1348, 0x134b, 0x134c, 0x134d, 0x134e, 0x134f, 0x1350, 0x135b, 0x135c, 0x135d,
    0x135e, 0x1366, 0x1367, 0x1368, 0x1369,
];

pub struct AtkMouse;

impl VendorAdapter for AtkMouse {
    fn matches(&self, info: &DeviceInfo) -> bool {
        // The 1216 dongle is verified live and exposes both the FF05 protocol
        // interface and an FF02 fallback interface.
        (info.vendor_id() == 0x373b
            && info.product_id() == 0x1216
            && matches!(info.usage_page(), 0xff05 | 0xff02))
            || (info.vendor_id() == 0x373b
                && info.usage_page() == 0xff05
                && info.usage() == 1
                && FF05_PIDS.contains(&info.product_id()))
    }

    fn kind(&self) -> &'static str {
        "mouse"
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
    let mut buffer = [0; 256];
    for _ in 0..3 {
        drain(&reader);
        if info.usage_page() == 0xff05 {
            send(&writer, &protocol::atk_request())?;
        } else {
            send(&writer, &protocol::atk_ff02_request())?;
        }
        let end = Instant::now() + Duration::from_millis(650);
        while Instant::now() < end {
            let n = reader
                .read_timeout(&mut buffer, 120)
                .map_err(HardwareError::transport)?;
            if n == 0 {
                continue;
            }
            let battery = if info.usage_page() == 0xff05 {
                protocol::atk_battery(&buffer[..n])
            } else {
                protocol::atk_ff02_battery(&buffer[..n])
            };
            if let Some(level) = battery {
                return Ok(level);
            }
        }
    }
    Err(HardwareError::new(
        ErrorKind::Timeout,
        "mouse did not respond",
    ))
}

#[cfg(test)]
mod tests {
    use super::FF05_PIDS;

    #[test]
    fn ff05_pid_table_is_sorted_unique_and_scoped_to_mice() {
        assert_eq!(FF05_PIDS.len(), 89);
        assert!(FF05_PIDS.is_sorted());
        assert!(FF05_PIDS.windows(2).all(|pair| pair[0] != pair[1]));
        assert!(FF05_PIDS.contains(&0x1216));
        // FF05 peripherals from the same device table must stay out.
        for peripheral in [
            0x0029, 0x3010, 0x4010, 0x5010, 0x6010, 0x7010, 0x8010, 0x9010, 0xA010,
        ] {
            assert!(!FF05_PIDS.contains(&peripheral));
        }
    }
}
