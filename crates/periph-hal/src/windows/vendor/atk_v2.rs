//! ATK V2 mice over the FF04/2 vendor interface: the 0x373B line plus the
//! CompX-VID 0x3554 Dragonfly line. 16-byte feature-report frames with the
//! 0x55-minus-sum checksum; battery via GetBatteryLevel (command 4). The
//! frames follow the reviewed ATK HUB JavaScript and are not yet verified on
//! live hardware.

use crate::{
    Connection, ErrorKind, HardwareError,
    dpi::{Dpi, DpiController},
    protocol,
    vendor::{Outcome, VendorAdapter},
    windows::transport::{drain, open_pair, open_pair_path},
};
use hidapi::{DeviceInfo, HidApi, HidDevice};
use std::{
    ffi::CString,
    sync::{Arc, Mutex},
};

/// V2 mice on the ATK VID (filter-3fyXcGGg.js, review-verified 2026-10-25).
const PIDS_373B: [u16; 179] = [
    0x1014, 0x1015, 0x1016, 0x1017, 0x1018, 0x1019, 0x101a, 0x101b, 0x102e, 0x102f, 0x1030, 0x1031,
    0x103c, 0x103d, 0x103e, 0x103f, 0x1040, 0x1045, 0x104a, 0x104b, 0x104c, 0x104d, 0x105b, 0x1065,
    0x1066, 0x1067, 0x1068, 0x1069, 0x106a, 0x1084, 0x1085, 0x1086, 0x1087, 0x10ac, 0x10c7, 0x10c8,
    0x10c9, 0x10ca, 0x10cb, 0x10cc, 0x10e7, 0x10e8, 0x10e9, 0x10fd, 0x1106, 0x1107, 0x110d, 0x110e,
    0x1115, 0x1116, 0x1117, 0x1119, 0x111a, 0x111b, 0x1135, 0x113e, 0x113f, 0x1149, 0x114a, 0x114b,
    0x114c, 0x1157, 0x1158, 0x1159, 0x115b, 0x115c, 0x115d, 0x115e, 0x11a0, 0x11a6, 0x11a7, 0x11a9,
    0x11aa, 0x11ab, 0x11b3, 0x11b6, 0x11c8, 0x11cc, 0x11cd, 0x11ce, 0x11cf, 0x11d2, 0x11d8, 0x11d9,
    0x11dd, 0x11e3, 0x11e4, 0x11e5, 0x11e7, 0x11ec, 0x11ed, 0x11f0, 0x11f2, 0x11f3, 0x11f4, 0x11f5,
    0x11f6, 0x11f7, 0x11f9, 0x11fd, 0x11fe, 0x1201, 0x120a, 0x120b, 0x120d, 0x120e, 0x121f, 0x1230,
    0x1245, 0x1246, 0x1248, 0x1249, 0x124a, 0x124e, 0x1256, 0x125a, 0x1266, 0x1269, 0x126c, 0x126d,
    0x126e, 0x126f, 0x1270, 0x1271, 0x1272, 0x1273, 0x1274, 0x1278, 0x1279, 0x127a, 0x127c, 0x127e,
    0x127f, 0x1285, 0x1286, 0x1289, 0x128a, 0x128b, 0x128c, 0x128d, 0x128e, 0x1292, 0x1293, 0x1296,
    0x129b, 0x129c, 0x129d, 0x12a2, 0x12a7, 0x12a8, 0x12a9, 0x12ab, 0x12af, 0x12b0, 0x12b3, 0x12b4,
    0x12bf, 0x12c6, 0x12cc, 0x12d7, 0x12da, 0x12e9, 0x12f7, 0x12f9, 0x12fa, 0x12fb, 0x12ff, 0x1303,
    0x1316, 0x131a, 0x132a, 0x132b, 0x133b, 0x1340, 0x1341, 0x1342, 0x1358, 0x1359, 0x1363,
];

/// The CompX-VID Dragonfly line: same FF04 protocol under a different VID.
/// Kept in this adapter because the ATK HUB lists them as ATK products;
/// identity records stay separate because the identity chain keys on VID.
const PIDS_3554: [u16; 12] = [
    0xf502, 0xf503, 0xf505, 0xf506, 0xf50a, 0xf50b, 0xf58a, 0xf58c, 0xf58d, 0xf58e, 0xf58f, 0xfb12,
];

pub struct AtkV2Mouse;

impl VendorAdapter for AtkV2Mouse {
    fn matches(&self, info: &DeviceInfo) -> bool {
        info.usage_page() == 0xff04
            && info.usage() == 2
            && ((info.vendor_id() == 0x373b && PIDS_373B.contains(&info.product_id()))
                || (info.vendor_id() == 0x3554 && PIDS_3554.contains(&info.product_id())))
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

    fn populate_capabilities(
        &self,
        info: &DeviceInfo,
        _slot: Option<u8>,
        lock: Arc<Mutex<()>>,
        capabilities: &mut crate::Capabilities,
    ) {
        let path = info.path().to_owned();
        let controller = V2MouseDpiController { path, lock };
        let _ = capabilities.insert("main", Dpi::new(vec![400, 800, 1200, 1600], controller));
    }
}

struct V2MouseDpiController {
    path: CString,
    lock: Arc<Mutex<()>>,
}

impl DpiController for V2MouseDpiController {
    fn read_stage(&self) -> Result<Option<u8>, HardwareError> {
        let _guard = self.lock.lock().map_err(|_| {
            HardwareError::new(ErrorKind::Unavailable, "mouse session lock poisoned")
        })?;
        let api = HidApi::new().map_err(HardwareError::transport)?;
        let (reader, writer) = open_pair_path(&api, &self.path)?;
        read_current_dpi(&reader, &writer)
    }

    fn set_stage(&self, stage: u8) -> Result<(), HardwareError> {
        let _guard = self.lock.lock().map_err(|_| {
            HardwareError::new(ErrorKind::Unavailable, "mouse session lock poisoned")
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
        writer
            .send_feature_report(&protocol::atk_v2_current_dpi_request())
            .map_err(HardwareError::transport)?;
        buffer[0] = 8;
        if let Ok(n) = reader.get_feature_report(&mut buffer)
            && let Some(stage) = protocol::atk_v2_current_dpi(&buffer[..n])
        {
            return Ok(Some(stage));
        }
    }
    Ok(None)
}

fn set_current_dpi(reader: &HidDevice, writer: &HidDevice, stage: u8) -> Result<(), HardwareError> {
    let mut buffer = [0u8; 17];
    let mut written = false;
    for _ in 0..3 {
        drain(reader);
        writer
            .send_feature_report(&protocol::atk_v2_set_current_dpi_request(stage))
            .map_err(HardwareError::transport)?;
        buffer[0] = 8;
        if let Ok(n) = reader.get_feature_report(&mut buffer)
            && protocol::atk_v2_set_eeprom_ok(&buffer[..n], 4)
        {
            written = true;
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

fn read_battery(api: &HidApi, info: &DeviceInfo) -> Result<u8, HardwareError> {
    let (reader, writer) = open_pair(api, info)?;
    let mut buffer = [0u8; 17];
    for _ in 0..3 {
        drain(&reader);
        writer
            .send_feature_report(&protocol::atk_ff04_battery_request())
            .map_err(HardwareError::transport)?;
        // The reply is a feature report on the control endpoint: a failed
        // send surfaces as an error, a silent device is bounded by the OS
        // control-transfer timeout. Not yet verified on live hardware.
        // hidapi 2.6 takes the report id in buffer[0], not as an argument.
        buffer[0] = 8;
        if let Ok(n) = reader.get_feature_report(&mut buffer)
            && let Some((level, _)) = protocol::atk_ff04_battery(&buffer[..n])
        {
            return Ok(level);
        }
    }
    Err(HardwareError::new(
        ErrorKind::Timeout,
        "mouse did not respond",
    ))
}

#[cfg(test)]
mod tests {
    use super::{PIDS_373B, PIDS_3554};

    #[test]
    fn ff04_pid_tables_are_sorted_unique_and_disjoint() {
        assert_eq!(PIDS_373B.len(), 179);
        assert_eq!(PIDS_3554.len(), 12);
        for list in [PIDS_373B.as_slice(), PIDS_3554.as_slice()] {
            assert!(list.is_sorted());
            assert!(list.windows(2).all(|pair| pair[0] != pair[1]));
        }
        assert!(PIDS_3554.contains(&0xf502));
        assert!(!PIDS_373B.iter().any(|pid| PIDS_3554.contains(pid)));
        // The early-batch FF02 segment on 0x3554 stays out: same-protocol
        // there is only an inference, not interface evidence.
        for unproven in [0xf2ff, 0xf400, 0xf410] {
            assert!(!PIDS_3554.contains(&unproven));
        }
    }
}
