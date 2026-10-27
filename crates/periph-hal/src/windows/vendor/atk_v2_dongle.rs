//! CompX ATK/VXE receivers on FF02:0002. The same FF04 wire frames travel
//! over output/input reports here; the dongle's FF04 feature interface rejects
//! writes. Contributor-verified on 3554:F58E (VXE R1 SE+ 1K dongle).

use crate::{
    Connection, ErrorKind, HardwareError,
    dpi::{Dpi, DpiController},
    protocol,
    vendor::{Outcome, VendorAdapter},
    windows::transport::{drain, open_pair, open_pair_path, send},
};
use hidapi::{DeviceInfo, HidApi, HidDevice};
use std::{
    ffi::CString,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

// ATK V HUB 2.3.47 receiver entries; only F58E is live-verified.
const RECEIVER_PIDS: [u16; 9] = [
    0xf201, 0xf503, 0xf505, 0xf50b, 0xf528, 0xf58a, 0xf58e, 0xf590, 0xfb12,
];

pub struct AtkV2Dongle;

impl VendorAdapter for AtkV2Dongle {
    fn matches(&self, info: &DeviceInfo) -> bool {
        info.vendor_id() == 0x3554
            && info.usage_page() == 0xff02
            && info.usage() == 2
            && RECEIVER_PIDS.contains(&info.product_id())
    }

    fn kind(&self) -> &'static str {
        "mouse"
    }

    fn discover(&self, api: &HidApi, info: &DeviceInfo) -> Vec<Outcome> {
        vec![probe(api, info, false).unwrap_or_else(Outcome::failure)]
    }

    fn read(&self, api: &HidApi, info: &DeviceInfo) -> Outcome {
        probe(api, info, true).unwrap_or_else(Outcome::failure)
    }

    fn populate_capabilities(
        &self,
        info: &DeviceInfo,
        _slot: Option<u8>,
        lock: Arc<Mutex<()>>,
        capabilities: &mut crate::Capabilities,
    ) {
        let path = info.path().to_owned();
        let controller = DongleDpiController { path, lock };
        let _ = capabilities.insert("main", Dpi::new(vec![400, 800, 1200, 1600], controller));
    }
}

struct DongleDpiController {
    path: CString,
    lock: Arc<Mutex<()>>,
}

impl DpiController for DongleDpiController {
    fn read_stage(&self) -> Result<Option<u8>, HardwareError> {
        let _guard = self.lock.lock().map_err(|_| {
            HardwareError::new(ErrorKind::Unavailable, "receiver session lock poisoned")
        })?;
        let api = HidApi::new().map_err(HardwareError::transport)?;
        let (reader, writer) = open_pair_path(&api, &self.path)?;
        read_current_dpi(&reader, &writer)
    }

    fn set_stage(&self, stage: u8) -> Result<(), HardwareError> {
        let _guard = self.lock.lock().map_err(|_| {
            HardwareError::new(ErrorKind::Unavailable, "receiver session lock poisoned")
        })?;
        let api = HidApi::new().map_err(HardwareError::transport)?;
        let (reader, writer) = open_pair_path(&api, &self.path)?;
        set_current_dpi(&reader, &writer, stage)
    }
}

fn read_current_dpi(reader: &HidDevice, writer: &HidDevice) -> Result<Option<u8>, HardwareError> {
    let mut buffer = [0u8; 17];
    for _ in 0..3 {
        drain(reader);
        send(writer, &protocol::atk_v2_current_dpi_request())?;
        let end = Instant::now() + Duration::from_millis(600);
        while Instant::now() < end {
            let n = reader
                .read_timeout(&mut buffer, 120)
                .map_err(HardwareError::transport)?;
            if n > 0 && let Some(stage) = protocol::atk_v2_current_dpi(&buffer[..n]) {
                return Ok(Some(stage));
            }
        }
    }
    Ok(None)
}

fn set_current_dpi(reader: &HidDevice, writer: &HidDevice, stage: u8) -> Result<(), HardwareError> {
    let mut buffer = [0u8; 17];
    let mut written = false;
    for _ in 0..3 {
        drain(reader);
        send(writer, &protocol::atk_v2_set_current_dpi_request(stage))?;
        let end = Instant::now() + Duration::from_millis(600);
        while Instant::now() < end {
            let n = reader
                .read_timeout(&mut buffer, 120)
                .map_err(HardwareError::transport)?;
            if n > 0 && protocol::atk_v2_set_eeprom_ok(&buffer[..n], 4) {
                written = true;
                break;
            }
        }
        if written {
            break;
        }
    }
    if !written {
        return Err(HardwareError::new(ErrorKind::Timeout, "mouse set dpi did not respond"));
    }
    let active = read_current_dpi(reader, writer)?;
    if active == Some(stage) {
        Ok(())
    } else {
        Err(HardwareError::new(
            ErrorKind::InvalidResponse,
            "dpi readback verification failed",
        ))
    }
}

fn status(online: bool, rf: [u8; 3]) -> Outcome {
    Outcome {
        connection: if online {
            Connection::Connected
        } else {
            Connection::Disconnected
        },
        // An idle receiver may retain the last paired RF ID. Only attribute
        // identity to a peripheral that the receiver actually reports online.
        uid: if online && rf.iter().any(|&byte| byte != 0) {
            rf.iter().map(|byte| format!("{byte:02x}")).collect()
        } else {
            String::new()
        },
        ..Outcome::default()
    }
}

fn probe(api: &HidApi, info: &DeviceInfo, sample_battery: bool) -> Result<Outcome, HardwareError> {
    let (reader, writer) = open_pair(api, info)?;
    let (online, rf) = read_online(&reader, &writer)?;
    let mut outcome = status(online, rf);
    // Discovery establishes identity and connection only.
    if !sample_battery {
        return Ok(outcome);
    }
    if !online {
        outcome.issue = Some(HardwareError::new(
            ErrorKind::NoPairedDevice,
            "receiver online without a linked mouse",
        ));
    } else {
        match read_battery(&reader, &writer) {
            Ok(level) => outcome.battery = Some(level),
            Err(error) => outcome.issue = Some(error),
        }
    }
    // Missing/idle readings are temporary, never evidence of Unsupported.
    Ok(outcome)
}

fn read_online(reader: &HidDevice, writer: &HidDevice) -> Result<(bool, [u8; 3]), HardwareError> {
    let mut buffer = [0u8; 64];
    for _ in 0..2 {
        drain(reader);
        send(writer, &protocol::atk_ff04_request(0x03))?;
        let end = Instant::now() + Duration::from_millis(600);
        while Instant::now() < end {
            let n = reader
                .read_timeout(&mut buffer, 120)
                .map_err(HardwareError::transport)?;
            if let Some((online @ 0..=1, rf)) = protocol::atk_v2_ff02_online(&buffer[..n]) {
                return Ok((online == 1, rf));
            }
        }
    }
    Err(HardwareError::new(
        ErrorKind::Timeout,
        "receiver online query did not respond",
    ))
}

fn read_battery(reader: &HidDevice, writer: &HidDevice) -> Result<u8, HardwareError> {
    let mut buffer = [0u8; 64];
    for _ in 0..3 {
        drain(reader);
        send(writer, &protocol::atk_ff04_request(0x04))?;
        let end = Instant::now() + Duration::from_millis(900);
        while Instant::now() < end {
            let n = reader
                .read_timeout(&mut buffer, 120)
                .map_err(HardwareError::transport)?;
            if let Some((level, _)) = protocol::atk_v2_ff02_battery(&buffer[..n]) {
                return Ok(level);
            }
        }
    }
    Err(HardwareError::new(
        ErrorKind::Timeout,
        "linked mouse battery did not respond",
    ))
}

#[cfg(test)]
mod tests {
    use super::{RECEIVER_PIDS, status};
    use crate::Connection;

    #[test]
    fn receiver_pid_table_is_sorted_unique_and_receiver_only() {
        assert_eq!(RECEIVER_PIDS.len(), 9);
        assert!(RECEIVER_PIDS.is_sorted());
        assert!(RECEIVER_PIDS.windows(2).all(|pair| pair[0] != pair[1]));
        assert!(RECEIVER_PIDS.contains(&0xf58e));
        assert!(!RECEIVER_PIDS.contains(&0xfa09));
        for wired in [0xf200, 0xf502, 0xf506, 0xf50a, 0xf540, 0xf58c, 0xf58f] {
            assert!(!RECEIVER_PIDS.contains(&wired));
        }
    }

    #[test]
    fn idle_receiver_is_disconnected_without_a_stale_peripheral_identity() {
        let linked = status(true, [0xa5, 0x20, 0xcf]);
        assert_eq!(linked.connection, Connection::Connected);
        assert_eq!(linked.uid, "a520cf");
        assert!(linked.battery.is_none());
        let idle = status(false, [0xa5, 0x20, 0xcf]);
        assert_eq!(idle.connection, Connection::Disconnected);
        assert!(idle.uid.is_empty());
        assert!(!idle.unsupported);
        assert!(status(true, [0, 0, 0]).uid.is_empty());
    }
}
