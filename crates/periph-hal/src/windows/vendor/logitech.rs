//! Logitech Unifying-family receivers (Lightspeed, Unifying, Bolt) over
//! HID++. The pairing count comes from a read of the connections register on
//! the FF00/0x01 collection — it counts paired slots, so a paired but asleep
//! mouse is still swept — and every slot is then probed with a long-frame
//! ping through the FF00/0x02 collection, the only path these receivers
//! relay device HID++ through. The scan performs no register writes, so it
//! cannot disturb a working link; the ping itself is the liveness test.
//!
//! The receiver pairing registers (0xB5 long reads) are deliberately not
//! touched: on the Lightspeed C539 the pairing sub-registers never answer at
//! all, and an idle receiver answers the ones that do exist seconds late —
//! both observed on hardware (examples/probe_logitech.rs). Slot kinds and
//! unit serials therefore stay unknown; readings fall back to the port id.
//! HID++ 1.0-only devices answer long pings with an error frame and are left
//! unsupported — Windows exposes the two report widths as separate
//! collections and relaying short device frames through them is not
//! verified.

use crate::{
    Capabilities, Connection, ErrorKind, HardwareError,
    dpi::{Dpi, DpiController},
    protocol,
    vendor::{Outcome, TargetCardinality, VendorAdapter},
    windows::{
        topology::container_id,
        transport::{drain, open_pair, send},
    },
};
use hidapi::{DeviceInfo, HidApi, HidDevice};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub struct LogitechReceiver;

/// Receiver personalities from Solaar's receiver table; all speak the same
/// HID++ device tunnel with the same register set.
#[derive(Clone, Copy)]
enum Receiver {
    Lightspeed,
    Unifying,
    Bolt,
}

impl Receiver {
    fn from_pid(pid: u16) -> Option<Self> {
        match pid {
            // Lightspeed receivers: C539 measured on hardware; siblings per
            // Solaar's KNOWN_RECEIVERS (usb interface 2, same layout).
            0xc539 | 0xc53a | 0xc53d | 0xc53f | 0xc541 | 0xc545 | 0xc547 | 0xc54d => {
                Some(Self::Lightspeed)
            }
            0xc52b | 0xc532 => Some(Self::Unifying),
            0xc548 => Some(Self::Bolt),
            _ => None,
        }
    }
}

fn target_cardinality(pid: u16) -> TargetCardinality {
    match Receiver::from_pid(pid) {
        Some(Receiver::Lightspeed) => TargetCardinality::Single,
        Some(Receiver::Unifying | Receiver::Bolt) => TargetCardinality::Multiple,
        None => TargetCardinality::Single,
    }
}

impl VendorAdapter for LogitechReceiver {
    fn matches(&self, info: &DeviceInfo) -> bool {
        // The two FF00 collections carry the two report widths — registers
        // and arrival notifications on 0x01, device-tunnelled HID++ on 0x02.
        // The paired devices ride on the receiver's own identity.
        info.vendor_id() == 0x046d
            && info.usage_page() == 0xff00
            && matches!(info.usage(), 0x01 | 0x02)
            && Receiver::from_pid(info.product_id()).is_some()
    }

    fn kind(&self) -> &'static str {
        // The receiver does not name the device behind a slot (the pairing
        // registers that would are out of scope, see the module notes), so
        // the product line default stands.
        "mouse"
    }

    fn target_cardinality(&self, info: &DeviceInfo) -> TargetCardinality {
        target_cardinality(info.product_id())
    }

    fn read(&self, api: &HidApi, info: &DeviceInfo) -> Outcome {
        self.read_all(api, info)
            .into_iter()
            .next()
            .unwrap_or_default()
    }

    fn read_all(&self, api: &HidApi, info: &DeviceInfo) -> Vec<Outcome> {
        match probe(api, info, true, None) {
            Ok(outcomes) => outcomes,
            Err(error) => vec![Outcome::failure(error)],
        }
    }

    fn discover(&self, api: &HidApi, info: &DeviceInfo) -> Vec<Outcome> {
        match probe(api, info, false, None) {
            Ok(outcomes) => outcomes,
            Err(error) => vec![Outcome {
                slot: matches!(
                    Receiver::from_pid(info.product_id()),
                    Some(Receiver::Lightspeed)
                )
                .then_some(1),
                ..Outcome::failure(error)
            }],
        }
    }

    fn read_target(&self, api: &HidApi, info: &DeviceInfo, slot: Option<u8>) -> Outcome {
        match probe(api, info, true, slot) {
            Ok(outcomes) => outcomes.into_iter().next().unwrap_or_else(|| {
                Outcome::failure(HardwareError::new(
                    ErrorKind::Unavailable,
                    "receiver target disappeared",
                ))
            }),
            Err(error) => Outcome::failure(error),
        }
    }

    fn populate_capabilities(
        &self,
        info: &DeviceInfo,
        slot: Option<u8>,
        lock: Arc<Mutex<()>>,
        capabilities: &mut Capabilities,
    ) {
        let slot = slot.unwrap_or(1);
        let presets = vec![800, 1200, 1600, 3200];
        let _ = capabilities.insert(
            "main",
            Dpi::with_writable(
                presets.clone(),
                false,
                LogitechDpiController {
                    container: container_id(info),
                    slot,
                    lock,
                    presets,
                },
            ),
        );
    }
}

struct LogitechDpiController {
    container: String,
    slot: u8,
    lock: Arc<Mutex<()>>,
    presets: Vec<u32>,
}

impl DpiController for LogitechDpiController {
    fn read_stage(&self) -> Result<Option<u8>, HardwareError> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| HardwareError::new(ErrorKind::Unavailable, "receiver lock poisoned"))?;
        let api = HidApi::new().map_err(HardwareError::transport)?;
        let interfaces: Vec<DeviceInfo> = api
            .device_list()
            .filter(|d| LogitechReceiver.matches(d) && container_id(d) == self.container)
            .cloned()
            .collect();
        let tunnel_info = interfaces
            .iter()
            .find(|i| i.usage() == 0x02)
            .ok_or_else(|| {
                HardwareError::new(ErrorKind::Unavailable, "tunnel interface missing")
            })?;
        let (reader, writer) = open_pair(&api, tunnel_info)?;
        let mut buffer = [0u8; 256];

        // 1. Try 0x2201 (Adjustable DPI)
        drain(&reader);
        send(
            &writer,
            &protocol::hidpp_get_feature(self.slot, protocol::HIDPP_FEATURE_ADJUSTABLE_DPI),
        )?;
        let idx_2201 = read_matching(&reader, &mut buffer, Duration::from_millis(300), |rep| {
            protocol::hidpp_feature_index(rep, self.slot)
        })?;
        if let Some(idx) = idx_2201 {
            drain(&reader);
            send(&writer, &protocol::hidpp_feature_call(self.slot, idx, 2))?;
            let rep = read_matching(&reader, &mut buffer, Duration::from_millis(300), |rep| {
                protocol::hidpp_parse_dpi_0x2201(rep, self.slot, idx)
            })?;
            if let Some((current_dpi, _default_dpi)) = rep {
                let stage = self
                    .presets
                    .iter()
                    .position(|&p| p == current_dpi as u32)
                    .map(|pos| (pos + 1) as u8);
                return Ok(stage);
            }
        }

        // 2. Try 0x2202 (Extended Adjustable DPI)
        drain(&reader);
        send(
            &writer,
            &protocol::hidpp_get_feature(
                self.slot,
                protocol::HIDPP_FEATURE_EXTENDED_ADJUSTABLE_DPI,
            ),
        )?;
        let idx_2202 = read_matching(&reader, &mut buffer, Duration::from_millis(300), |rep| {
            protocol::hidpp_feature_index(rep, self.slot)
        })?;
        if let Some(idx) = idx_2202 {
            drain(&reader);
            send(&writer, &protocol::hidpp_feature_call(self.slot, idx, 5))?;
            let rep = read_matching(&reader, &mut buffer, Duration::from_millis(300), |rep| {
                protocol::hidpp_parse_dpi_0x2202(rep, self.slot, idx)
            })?;
            if let Some(current_dpi) = rep {
                let stage = self
                    .presets
                    .iter()
                    .position(|&p| p == current_dpi as u32)
                    .map(|pos| (pos + 1) as u8);
                return Ok(stage);
            }
        }

        Ok(None)
    }

    fn set_stage(&self, _stage: u8) -> Result<(), HardwareError> {
        Err(HardwareError::new(
            ErrorKind::Unsupported,
            "Logitech DPI write is disabled without verified onboard mode bypass",
        ))
    }
}

fn probe(
    api: &HidApi,
    info: &DeviceInfo,
    sample_battery: bool,
    requested: Option<u8>,
) -> Result<Vec<Outcome>, HardwareError> {
    // The receiver exposes two collections of the same page, so the register
    // and tunnel interfaces are re-matched from the device list and grouped
    // by container — one scan result set per physical receiver.
    Receiver::from_pid(info.product_id())
        .ok_or_else(|| HardwareError::new(ErrorKind::Unsupported, "unknown receiver"))?;
    let container = container_id(info);
    let interfaces: Vec<DeviceInfo> = api
        .device_list()
        .filter(|d| LogitechReceiver.matches(d) && container_id(d) == container)
        .cloned()
        .collect();
    let register_info = interfaces
        .iter()
        .find(|info| info.usage() == 0x01)
        .ok_or_else(|| {
            HardwareError::new(
                ErrorKind::Unavailable,
                "receiver register interface missing",
            )
        })?;
    let tunnel_info = interfaces
        .iter()
        .find(|info| info.usage() == 0x02)
        .ok_or_else(|| {
            HardwareError::new(ErrorKind::Unavailable, "receiver tunnel interface missing")
        })?;
    let (reader, writer) = open_pair(api, register_info)?;
    let (tunnel_reader, tunnel_writer) = open_pair(api, tunnel_info)?;
    let mut buffer = [0; 256];
    let Some(paired) = hidpp_register(
        &reader,
        &writer,
        &mut buffer,
        protocol::HIDPP_REGISTER_CONNECTIONS,
        None,
    )?
    .map(protocol::hidpp_pairing_count) else {
        return Ok(vec![Outcome::failure(HardwareError::new(
            ErrorKind::Timeout,
            "receiver did not respond",
        ))]);
    };
    if paired == 0 {
        return Ok(Vec::new());
    }
    let mut outcomes = Vec::new();
    let capacity = if matches!(
        Receiver::from_pid(info.product_id()),
        Some(Receiver::Lightspeed)
    ) {
        1
    } else {
        6
    };
    for slot in 1..=capacity {
        if requested.is_some_and(|requested| requested != slot) {
            continue;
        }
        drain(&tunnel_reader);
        send(&tunnel_writer, &protocol::hidpp_ping(slot))?;
        let online = read_matching(
            &tunnel_reader,
            &mut buffer,
            Duration::from_millis(400),
            |report| protocol::hidpp_ping_reply(report, slot),
        )?;
        if online.is_none() {
            if capacity == 1 || requested == Some(slot) {
                outcomes.push(Outcome {
                    slot: Some(slot),
                    ..Outcome::failure(HardwareError::new(
                        ErrorKind::Timeout,
                        "paired device did not respond",
                    ))
                });
            }
            continue;
        }
        let (battery, unsupported, issue) = if sample_battery {
            match hidpp_battery(&tunnel_reader, &tunnel_writer, &mut buffer, slot)? {
                HidppBatteryOutcome::Value(level) => (Some(level), false, None),
                HidppBatteryOutcome::Unsupported => (None, true, None),
                HidppBatteryOutcome::Unavailable(err) => (None, false, Some(err)),
            }
        } else {
            (None, false, None)
        };
        let name = if !sample_battery {
            hidpp_name(&tunnel_reader, &tunnel_writer, &mut buffer, slot).unwrap_or_default()
        } else {
            String::new()
        };
        outcomes.push(Outcome {
            battery,
            unsupported,
            issue,
            connection: Connection::Connected,
            hardware_name: (!name.is_empty()).then_some(name),
            slot: Some(slot),
            ..Outcome::default()
        });
    }
    Ok(outcomes)
}

/// One bounded read window; non-matching input (notifications, error frames,
/// other devices' traffic) is consumed and skipped.
fn read_matching<T>(
    reader: &HidDevice,
    buffer: &mut [u8; 256],
    timeout: Duration,
    matcher: impl Fn(&[u8]) -> Option<T>,
) -> Result<Option<T>, HardwareError> {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        let n = reader
            .read_timeout(buffer, 100)
            .map_err(HardwareError::transport)?;
        if n == 0 {
            continue;
        }
        if let Some(value) = matcher(&buffer[..n]) {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

/// One HID++ 1.0 receiver register exchange; `values` selects a write
/// (0x80) over a read (0x81). `None` out means the receiver did not answer.
/// 0x41 arrival notifications streamed in between are consumed and skipped.
fn hidpp_register(
    reader: &HidDevice,
    writer: &HidDevice,
    buffer: &mut [u8; 256],
    register: u8,
    values: Option<[u8; 3]>,
) -> Result<Option<[u8; 3]>, HardwareError> {
    let frame = match values {
        Some(values) => protocol::hidpp_set_register(0xff, register, values),
        None => protocol::hidpp_get_register(0xff, register),
    };
    drain(reader);
    send(writer, &frame)?;
    let end = Instant::now() + Duration::from_millis(400);
    while Instant::now() < end {
        let n = reader
            .read_timeout(buffer, 60)
            .map_err(HardwareError::transport)?;
        if n == 0 {
            continue;
        }
        let matched = match values {
            None => protocol::hidpp_get_register_reply(&buffer[..n], 0xff, register),
            Some(_) => {
                protocol::hidpp_set_register_ok(&buffer[..n], 0xff, register).then_some([0, 0, 0])
            }
        };
        if let Some(reply) = matched {
            return Ok(Some(reply));
        }
    }
    Ok(None)
}

enum HidppBatteryOutcome {
    Value(u8),
    Unsupported,
    Unavailable(HardwareError),
}

/// Unified Battery (0x1004), Battery Level Status (0x1000), then Battery
/// Voltage (0x1001) - Solaar's preference order for HID++ 2.0 devices.
/// If all three features are definitively absent on the hardware (GetFeature returns 0),
/// returns Unsupported. If queries fail or time out, returns Unavailable.
fn hidpp_battery(
    reader: &HidDevice,
    writer: &HidDevice,
    buffer: &mut [u8; 256],
    device: u8,
) -> Result<HidppBatteryOutcome, HardwareError> {
    for (feature, function, parse) in [
        (
            protocol::HIDPP_FEATURE_UNIFIED_BATTERY,
            1u8,
            protocol::hidpp_unified_battery as fn(&[u8], u8, u8) -> Option<u8>,
        ),
        (
            protocol::HIDPP_FEATURE_BATTERY_STATUS,
            0u8,
            protocol::hidpp_battery_status as fn(&[u8], u8, u8) -> Option<u8>,
        ),
        (
            protocol::HIDPP_FEATURE_BATTERY_VOLTAGE,
            0u8,
            protocol::hidpp_voltage_battery_percent as fn(&[u8], u8, u8) -> Option<u8>,
        ),
    ] {
        drain(reader);
        send(writer, &protocol::hidpp_get_feature(device, feature))?;
        let raw_index = read_matching(reader, buffer, Duration::from_millis(400), |report| {
            protocol::hidpp_feature_index_raw(report, device)
        })?;
        match raw_index {
            Some(0) => {
                // Feature explicitly reported as unsupported by the device firmware
                continue;
            }
            Some(index) => {
                drain(reader);
                send(
                    writer,
                    &protocol::hidpp_feature_call(device, index, function),
                )?;
                let level = read_matching(reader, buffer, Duration::from_millis(400), |report| {
                    parse(report, device, index)
                })?;
                if let Some(level) = level {
                    return Ok(HidppBatteryOutcome::Value(level));
                }
                return Ok(HidppBatteryOutcome::Unavailable(HardwareError::new(
                    ErrorKind::Unavailable,
                    "device feature call returned no valid battery reading",
                )));
            }
            None => {
                // Communication timeout or unrecognized frame: cannot confirm Unsupported
                return Ok(HidppBatteryOutcome::Unavailable(HardwareError::new(
                    ErrorKind::Timeout,
                    "battery feature lookup timed out",
                )));
            }
        }
    }
    // All queried battery features were explicitly reported as absent (index 0)
    Ok(HidppBatteryOutcome::Unsupported)
}

/// Device Name (0x0003): one length call plus fixed-size text chunks. A
/// half-asleep device answers feature lookups inconsistently, so the lookup
/// is retried and the result length- and printability-checked.
fn hidpp_name(
    reader: &HidDevice,
    writer: &HidDevice,
    buffer: &mut [u8; 256],
    device: u8,
) -> Option<String> {
    let mut index = 0u8;
    for _ in 0..2 {
        drain(reader);
        send(
            writer,
            &protocol::hidpp_get_feature(device, protocol::HIDPP_FEATURE_DEVICE_NAME),
        )
        .ok()?;
        if let Ok(Some(found)) =
            read_matching(reader, buffer, Duration::from_millis(400), |report| {
                protocol::hidpp_feature_index(report, device)
            })
        {
            index = found;
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    let index = (index != 0).then_some(index)?;
    drain(reader);
    send(writer, &protocol::hidpp_feature_call(device, index, 0)).ok()?;
    let length = read_matching(reader, buffer, Duration::from_millis(400), |report| {
        protocol::hidpp_name_length(report, device, index)
    })
    .ok()??;
    let mut raw = Vec::with_capacity(length);
    while raw.len() < length {
        drain(reader);
        send(
            writer,
            &protocol::hidpp_feature_call1(device, index, 1, raw.len() as u8),
        )
        .ok()?;
        let chunk = read_matching(reader, buffer, Duration::from_millis(400), |report| {
            protocol::hidpp_name_text(report, device, index).map(|bytes| bytes.to_vec())
        })
        .ok()??;
        if chunk.is_empty() {
            return None;
        }
        raw.extend_from_slice(&chunk);
    }
    let raw = &raw[..length];
    if !raw.iter().all(|byte| (0x20..=0x7e).contains(byte)) {
        return None;
    }
    let name = String::from_utf8_lossy(raw).trim().to_string();
    (name.len() >= 4).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::{TargetCardinality, target_cardinality};

    #[test]
    fn receiver_topology_comes_from_protocol_family() {
        assert_eq!(target_cardinality(0xc539), TargetCardinality::Single);
        for pid in [0xc52b, 0xc532, 0xc548] {
            assert_eq!(target_cardinality(pid), TargetCardinality::Multiple);
        }
    }
}
