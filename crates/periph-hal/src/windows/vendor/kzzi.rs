//! KZZI keyboards behind a Compx 2.4G receiver (3554:FA09). The receiver
//! answers identity itself; battery queries are relayed to the keyboard and
//! only models whose firmware implements them reply. Models verified to keep
//! their battery private report Unsupported, so a linked keyboard without a
//! reading is told apart from one that answered nothing. The interface only
//! answers requests written through the control endpoint (hidapi fails there
//! with error 1784), so the exchange runs on `windows::transport::KzziLink` raw handles.

use crate::{Connection, ErrorKind, HardwareError};
use crate::{
    lighting::{Lighting, LightingController},
    protocol,
    vendor::{Outcome, VendorAdapter},
    windows::transport::KzziLink,
};
use hidapi::{DeviceInfo, HidApi};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

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

    fn discover(&self, _api: &HidApi, info: &DeviceInfo) -> Vec<Outcome> {
        match KzziLink::open(&info.path().to_string_lossy()).and_then(|link| identify(&link)) {
            Ok((name, unsupported)) => vec![Outcome {
                hardware_name: (!name.is_empty()).then_some(name),
                unsupported,
                connection: if unsupported {
                    Connection::Connected
                } else {
                    Connection::Unknown
                },
                ..Outcome::default()
            }],
            Err(error) => vec![Outcome::failure(error)],
        }
    }

    fn populate_capabilities(
        &self,
        info: &DeviceInfo,
        _slot: Option<u8>,
        lock: Arc<Mutex<()>>,
        capabilities: &mut crate::Capabilities,
    ) {
        let path = info.path().to_string_lossy().to_string();
        let controller = KzziLightingController { path, lock };
        let _ = capabilities.insert("main", Lighting::new(controller));
    }
}

struct KzziLightingController {
    path: String,
    lock: Arc<Mutex<()>>,
}

impl LightingController for KzziLightingController {
    fn read_state(&self) -> Result<bool, HardwareError> {
        let _guard = self.lock.lock().map_err(|_| {
            HardwareError::new(ErrorKind::Unavailable, "keyboard session lock poisoned")
        })?;
        let link = KzziLink::open(&self.path)?;
        read_lighting_state(&link)
    }

    fn set_state(&self, on: bool) -> Result<(), HardwareError> {
        let _guard = self.lock.lock().map_err(|_| {
            HardwareError::new(ErrorKind::Unavailable, "keyboard session lock poisoned")
        })?;
        let link = KzziLink::open(&self.path)?;
        set_lighting_state(&link, on)
    }
}

fn read_lighting_state(link: &KzziLink) -> Result<bool, HardwareError> {
    let profile = read_profile(link)?;
    Ok(protocol::kzzi_profile_is_lit(&profile))
}

fn set_lighting_state(link: &KzziLink, on: bool) -> Result<(), HardwareError> {
    let original = read_profile(link).unwrap_or_else(|_| {
        let mut def = [0u8; 128];
        def[126] = 0x5a;
        def[127] = 0xa5;
        def
    });
    let modified = protocol::kzzi_create_lighting_profile(&original, on);
    let frames = protocol::kzzi_split_lighting_profile(&modified);
    let mut buffer = [0u8; 64];
    for frame in &frames {
        link.drain();
        link.send(frame)?;
        let _ = link.read(&mut buffer, Duration::from_millis(50));
    }
    link.drain();
    link.send(&protocol::kzzi_save_lighting_request())?;
    std::thread::sleep(Duration::from_millis(80));

    let readback = read_profile(link)?;
    if protocol::kzzi_profile_is_lit(&readback) == on {
        Ok(())
    } else {
        Err(HardwareError::new(
            ErrorKind::InvalidResponse,
            "lighting readback mismatch",
        ))
    }
}

fn read_profile(link: &KzziLink) -> Result<[u8; 128], HardwareError> {
    let mut pages = [[0u8; 20]; 10];
    let mut buffer = [0u8; 64];
    for _ in 0..3 {
        link.drain();
        link.send(&protocol::kzzi_lighting_read_request())?;
        let end = Instant::now() + Duration::from_millis(1200);
        let mut count = 0;
        let mut collected = [false; 10];
        while Instant::now() < end && count < 10 {
            if let Some(n) = link.read(&mut buffer, Duration::from_millis(100)) {
                if n >= 20
                    && protocol::kzzi_frame_ok(&buffer[..20])
                    && (buffer[1] == 0x44 || buffer[1] == 0x04)
                    && buffer[2] == 10
                {
                    let seq = buffer[3] as usize;
                    if seq < 10 && !collected[seq] {
                        pages[seq].copy_from_slice(&buffer[..20]);
                        collected[seq] = true;
                        count += 1;
                    }
                }
            }
        }
        if count == 10 && let Some(profile) = protocol::kzzi_parse_lighting_profile(&pages) {
            return Ok(profile);
        }
    }
    Err(HardwareError::new(
        ErrorKind::Timeout,
        "kzzi lighting profile read timed out",
    ))
}

fn identify(link: &KzziLink) -> Result<(String, bool), HardwareError> {
    let mut buffer = [0u8; 64];
    for _ in 0..3 {
        link.drain();
        link.send(&protocol::kzzi_identity_request())?;
        let end = Instant::now() + Duration::from_millis(600);
        while Instant::now() < end {
            if let Some(n) = link.read(&mut buffer, Duration::from_millis(120))
                && let Some(psd) = protocol::kzzi_identity(&buffer[..n])
            {
                return Ok((
                    protocol::compx_model_name(&psd).unwrap_or_default().into(),
                    protocol::kzzi_battery_private(&psd),
                ));
            }
        }
    }
    Ok((String::new(), false))
}

fn probe(info: &DeviceInfo) -> Result<(Option<u8>, String, bool), HardwareError> {
    let link = KzziLink::open(&info.path().to_string_lossy())?;
    let (name, private) = identify(&link)?;
    if private {
        return Ok((None, name, true));
    }
    let mut buffer = [0u8; 64];
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
