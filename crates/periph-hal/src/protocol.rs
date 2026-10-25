//! Wire formats ported from the verified Python readers. No device I/O here.

/// FF05 vendor request for subcommand `sub`; 0x07 reads the battery.
pub fn atk_sub_request(sub: u8) -> [u8; 64] {
    let mut report = [0; 64];
    report[..8].copy_from_slice(&[0x08, 0x7d, 0x72, 0x02, 0x00, 0x01, sub, 0x01]);
    report
}

pub fn atk_request() -> [u8; 64] {
    atk_sub_request(0x07)
}

pub fn atk_battery(report: &[u8]) -> Option<u8> {
    (report.len() >= 8 && report[1] == 0x72 && report[5] == 0x07 && report[7] <= 100)
        .then(|| report[7])
}

/// FF02 fallback interface of the same ATK receiver. Preserves the verified
/// Python report construction: report id 8, command 4, 17 bytes.
pub fn atk_ff02_request() -> [u8; 17] {
    let mut report = [0; 17];
    report[0] = 8;
    report[1] = 4;
    report
}

/// FF02 battery reply: level in byte 6.
pub fn atk_ff02_battery(report: &[u8]) -> Option<u8> {
    (report.len() > 6 && report[6] <= 100).then(|| report[6])
}

/// ATK FF04 (V2 mouse) family: 16-byte frames exchanged as feature reports
/// with report id 8 on usage page FF04/2. The checksum covers the report id
/// plus the first 15 frame bytes: 0x55 - ((0x08 + sum) & 0xFF), mirroring
/// the ATK HUB's Ke command base.
pub fn atk_ff04_request(command_id: u8) -> [u8; 17] {
    let mut report = [0; 17];
    report[0] = 8;
    report[1] = command_id;
    report[16] = 0x55u8.wrapping_sub(
        report[..16]
            .iter()
            .fold(0u8, |sum, &byte| sum.wrapping_add(byte)),
    );
    report
}

/// Battery query for the FF04 family: GetBatteryLevel (command 4).
pub fn atk_ff04_battery_request() -> [u8; 17] {
    atk_ff04_request(0x04)
}

/// FF04 battery reply parsed from the 17-byte feature buffer: command echo
/// at byte 1, dataValidLen at 5, level at 6, charging flag at 7 (the vendor's
/// data[0]/data[1]); the big-endian voltage in 8..10 is ignored. Returns
/// (level, charging).
pub fn atk_ff04_battery(report: &[u8]) -> Option<(u8, u8)> {
    if report.len() < 17 || report[1] != 0x04 {
        return None;
    }
    let sum = report[..16]
        .iter()
        .fold(0u8, |sum, &byte| sum.wrapping_add(byte));
    if sum.wrapping_add(report[16]) != 0x55 {
        return None;
    }
    (report[6] <= 100).then_some((report[6], report[7]))
}

pub fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0xffff_u16;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

pub fn darkrim_query(seq: u8, kind: u8) -> [u8; 65] {
    let mut report = [0; 65];
    report[..12].copy_from_slice(&[0, 0xfe, seq, 0x10, 0, 2, 0, 0, 2, 0, kind, 0]);
    let crc = crc16(&report[1..12]).to_be_bytes();
    report[12..14].copy_from_slice(&crc);
    report
}

/// hidapi's native Windows backend removes the zero report-ID byte on input.
/// The original ctypes reader and our wire parser include that byte.
pub fn restore_darkrim_report_id(input: &[u8]) -> Vec<u8> {
    if input.first() == Some(&0xfe) {
        std::iter::once(0).chain(input.iter().copied()).collect()
    } else {
        input.to_vec()
    }
}

/// Reject truncated TLVs, incorrect sequence IDs and bad CRCs before using data.
pub fn darkrim_tlvs(report: &[u8], expected_seq: u8) -> Option<Vec<(u8, &[u8])>> {
    if report.len() < 12 || report[0] != 0 || report[1] != 0xfe || report[2] != expected_seq {
        return None;
    }
    let end = 10 + u16::from_le_bytes([report[8], report[9]]) as usize;
    if end + 2 > report.len()
        || crc16(&report[1..end]) != u16::from_be_bytes([report[end], report[end + 1]])
    {
        return None;
    }
    let mut tlvs = Vec::new();
    let mut i = 10;
    while i < end {
        if i + 2 > end {
            return None;
        }
        let next = i + 2 + report[i + 1] as usize;
        if next > end {
            return None;
        }
        tlvs.push((report[i], &report[i + 2..next]));
        i = next;
    }
    Some(tlvs)
}

/// KZZI/Compx 2.4G receiver frames: report id 0x13, 20 bytes, the last byte
/// being the sum of the previous 19. The battery layout follows the vendor
/// driver captures mirrored by womier-l65-linux and sanshoku issue 36.
pub fn kzzi_request(command: u8) -> [u8; 20] {
    let mut frame = [0; 20];
    frame[0] = 0x13;
    frame[1] = command;
    frame[19] = frame[..19]
        .iter()
        .fold(0u8, |sum, &byte| sum.wrapping_add(byte));
    frame
}

/// Identity request asking for package `npkg` of the identity reply.
pub fn kzzi_package_request(npkg: u8) -> [u8; 20] {
    let mut frame = kzzi_request(0x05);
    frame[2] = npkg;
    frame[19] = frame[..19]
        .iter()
        .fold(0u8, |sum, &byte| sum.wrapping_add(byte));
    frame
}

/// The identity request answers with npkg=1 in byte 2 on this receiver.
pub fn kzzi_identity_request() -> [u8; 20] {
    kzzi_package_request(1)
}

pub fn kzzi_frame_ok(report: &[u8]) -> bool {
    report.len() == 20
        && report[0] == 0x13
        && report[..19]
            .iter()
            .fold(0u8, |sum, &byte| sum.wrapping_add(byte))
            == report[19]
}

/// Identity reply carries the vendor profile signature (PSD) in bytes 5..11.
pub fn kzzi_identity(report: &[u8]) -> Option<[u8; 6]> {
    if !kzzi_frame_ok(report) || report[1] != 0x05 {
        return None;
    }
    let mut psd = [0; 6];
    psd.copy_from_slice(&report[5..11]);
    Some(psd)
}

/// Battery reply: (level, state); 0x01 on battery, 0x10 cable/charging.
/// The receiver streams unrelated 0x13 frames, so the command nibble, the
/// checksum and the 1..=100 level range all gate a reading. The 0x4A query
/// is also wrapped by the vendor's own DeviceDriver.exe at 0x40E560
/// (review-verified static anchor, reachability unproven).
pub fn kzzi_battery(report: &[u8]) -> Option<(u8, u8)> {
    if !kzzi_frame_ok(report) || report[1] & 0x7f != 0x4a {
        return None;
    }
    let (level, state) = (report[5], report[6]);
    ((1..=100).contains(&level)).then_some((level, state))
}

/// Model names keyed by the profile signature recorded in the vendor driver's
/// device profiles (Dev/*/KB.ini Psd= hex values, names from Cfg.ini DevName).
pub fn kzzi_model_name(psd: &[u8]) -> Option<&'static str> {
    Some(match psd {
        [2, 0, 0, 0, 0, 54] => "Kzzi-K75 RGB",
        [2, 0, 0, 0, 0, 55] => "Kzzi-K75 RGB key",
        [3, 0, 0, 0, 0, 2] => "Kzzi-K68",
        [3, 0, 0, 0, 0, 4] => "Kzzi-K84",
        [3, 0, 0, 0, 0, 103] => "Kzzi-K75Pro",
        [3, 0, 0, 0, 0, 224] => "Kzzi-K20RGB",
        [3, 0, 0, 0, 0, 252] => "Kzzi-K68Pro",
        [3, 0, 0, 0, 1, 10] => "Kzzi-K98",
        // AULA F87 Pro on the same Compx receiver family; signature measured
        // from the device itself (identity request 0x05).
        [3, 0, 0, 0, 1, 11] => "AULA F87 Pro",
        [3, 0, 0, 0, 1, 52] => "Kzzi-K87",
        [3, 0, 0, 0, 1, 68] => "KZZI-K75RGB Lite",
        [3, 0, 0, 0, 1, 104] => "Kzzi-i75 RGB",
        [3, 0, 0, 0, 1, 215] => "Kzzi-K87MAX",
        [3, 0, 0, 0, 3, 162] => "Kzzi-K75V2",
        [3, 0, 0, 0, 3, 187] => "Kzzi-Z75",
        [3, 0, 0, 0, 3, 215] => "Kzzi K98V2",
        [3, 0, 0, 0, 3, 243] => "Kzzi-i98",
        [3, 0, 0, 0, 3, 247] => "Kzzi-i20",
        [3, 0, 0, 0, 5, 62] => "Kzzi-i98",
        [6, 0, 0, 0, 0, 1] => "Kzzi-Z98",
        [6, 0, 0, 0, 0, 29] => "Kzzi-K21",
        _ => return None,
    })
}

/// PSDs of models verified live to keep their battery private: the 0x4A
/// query stays unanswered even while the keyboard is linked, so a missing
/// reading is the model's trait rather than a dead link. Silence on any
/// other profile may still mean the keyboard is off.
pub fn kzzi_battery_private(psd: &[u8]) -> bool {
    // Kzzi-K75 RGB key, verified on the 3554:FA09 receiver.
    matches!(psd, [2, 0, 0, 0, 0, 55])
}

/// BIGBIG WON (Zikway) controller short frames over the FF7A vendor interface.
/// The wire report is 65 bytes with report id 0; the payload starts with 0xA5,
/// byte 1 is the total frame length including A5 and the trailing additive sum
/// of every preceding byte. GetDevicePowerInfo (A5 04 04) answers with
/// A5 <len> 04 <level> <state> <sum>.
/// Short read-only query frame: A5 04 <opcode> <sum>.
pub fn bigbigwon_short_request(opcode: u8) -> [u8; 65] {
    let mut report = [0; 65];
    report[1] = 0xa5;
    report[2] = 0x04;
    report[3] = opcode;
    report[4] = report[1..4]
        .iter()
        .fold(0u8, |sum, byte| sum.wrapping_add(*byte));
    report
}

pub fn bigbigwon_request() -> [u8; 65] {
    bigbigwon_short_request(0x04)
}

pub fn bigbigwon_battery(report: &[u8]) -> Option<u8> {
    if report.first() != Some(&0xa5) {
        return None;
    }
    let total = *report.get(1)? as usize;
    // The vendor's reply handler (DevMgr.dll 0x10024573) fixes the frame
    // length at 6: A5 06 04 <level> <state> <sum>.
    if total != 6 || total > report.len() || report[2] != 0x04 {
        return None;
    }
    let sum = report[..total - 1]
        .iter()
        .fold(0u8, |sum, &byte| sum.wrapping_add(byte));
    if report[total - 1] != sum {
        return None;
    }
    let level = report[3];
    (level <= 100).then_some(level)
}

/// GetUUID asks with eight zero selector bytes, mirroring the official
/// client: A5 0C EF 00×8 <sum>. Verified live: the linked controller answers
/// A5 0C EF <8 uuid bytes> <sum> and the idle personality stays silent.
pub fn bigbigwon_uuid_request() -> [u8; 65] {
    let mut report = [0; 65];
    report[1] = 0xa5;
    report[2] = 0x0c;
    report[3] = 0xef;
    report[12] = report[1..12]
        .iter()
        .fold(0u8, |sum, byte| sum.wrapping_add(*byte));
    report
}

pub fn bigbigwon_uuid(report: &[u8]) -> Option<[u8; 8]> {
    if report.first() != Some(&0xa5) {
        return None;
    }
    let total = *report.get(1)? as usize;
    if total != 12 || total > report.len() || report[2] != 0xef {
        return None;
    }
    let sum = report[..total - 1]
        .iter()
        .fold(0u8, |sum, byte| sum.wrapping_add(*byte));
    if report[total - 1] != sum {
        return None;
    }
    let mut uuid = [0; 8];
    uuid.copy_from_slice(&report[3..11]);
    Some(uuid)
}

// Logitech HID++ 2.0 short frames for mice behind a LIGHTSPEED receiver
// (usage page FF00). Layout per Logitech's public HID++ 1.0/2.0 protocol
// documentation; semantics cross-checked against Solaar (pwr-Solaar/Solaar,
// GPL-2.0) as protocol evidence — implementation written from scratch here.
//
// On a LIGHTSPEED receiver the two vendor collections split the two report
// widths: FF00/0x01 carries the 7-byte short report (0x10) and FF00/0x02 the
// 20-byte long report (0x11). Receiver registers answer short frames on the
// first collection; frames addressed to the paired device are only relayed
// as long frames through the second — a short device frame gets the HID++ 1.0
// error 0x09 there. Device frames below are therefore long.
// Frame: report id, device index, feature index, (function << 4) | software
// id, then params. Replies echo the first four bytes; feature notifications
// carry software id 0, so they never match a request.
pub const HIDPP_SHORT: u8 = 0x10;
pub const HIDPP_LONG: u8 = 0x11;
/// Solaar's table of in-use software ids reserves 0x07 (OpenRGB), 0x0A
/// (LGSTray), 0x0D (G HUB) and 0 (notifications/firmware); avoid them all.
pub const HIDPP_SOFTWARE_ID: u8 = 0x0b;
/// Root.Ping: feature 0x00, function 1. Replies echo the mark byte.
const HIDPP_PING_ADDRESS: u8 = 0x10 | HIDPP_SOFTWARE_ID;
const HIDPP_PING_MARK: u8 = 0x42;

pub const HIDPP_FEATURE_DEVICE_NAME: u16 = 0x0003;
pub const HIDPP_FEATURE_BATTERY_STATUS: u16 = 0x1000;
pub const HIDPP_FEATURE_BATTERY_VOLTAGE: u16 = 0x1001;
pub const HIDPP_FEATURE_UNIFIED_BATTERY: u16 = 0x1004;

/// Root.GetFeature maps a feature id to the device-local feature index.
pub fn hidpp_get_feature(device: u8, feature: u16) -> [u8; 20] {
    let mut frame = [0; 20];
    frame[..6].copy_from_slice(&[
        HIDPP_LONG,
        device,
        0x00,
        HIDPP_SOFTWARE_ID,
        (feature >> 8) as u8,
        feature as u8,
    ]);
    frame
}

/// Root.Ping; the reply carries the protocol version and echoes the mark.
pub fn hidpp_ping(device: u8) -> [u8; 20] {
    let mut frame = [0; 20];
    frame[..7].copy_from_slice(&[
        HIDPP_LONG,
        device,
        0x00,
        HIDPP_PING_ADDRESS,
        0,
        0,
        HIDPP_PING_MARK,
    ]);
    frame
}

pub fn hidpp_feature_call(device: u8, index: u8, function: u8) -> [u8; 20] {
    let mut frame = [0; 20];
    frame[..4].copy_from_slice(&[
        HIDPP_LONG,
        device,
        index,
        (function << 4) | HIDPP_SOFTWARE_ID,
    ]);
    frame
}

/// The caller passes params[0], e.g. the character offset for a name chunk.
pub fn hidpp_feature_call1(device: u8, index: u8, function: u8, param: u8) -> [u8; 20] {
    let mut frame = hidpp_feature_call(device, index, function);
    frame[4] = param;
    frame
}

fn hidpp_reply_ok(report: &[u8], device: u8, feature: u8, address: u8) -> bool {
    report.len() >= 7
        && matches!(report[0], HIDPP_SHORT | HIDPP_LONG)
        && report[1] == device
        && report[2] == feature
        && report[3] == address
}

pub fn hidpp_ping_reply(report: &[u8], device: u8) -> Option<(u8, u8)> {
    hidpp_reply_ok(report, device, 0x00, HIDPP_PING_ADDRESS).then(|| (report[4], report[5]))
}

/// GetFeature replies with the local index, 0 when the device lacks it.
pub fn hidpp_feature_index(report: &[u8], device: u8) -> Option<u8> {
    hidpp_reply_ok(report, device, 0x00, HIDPP_SOFTWARE_ID)
        .then(|| report[4])
        .filter(|index| *index != 0)
}

/// Unified Battery GetStatus: (state of charge, level approximation, charge
/// status). A state of charge of 0 means "not reported"; fall back to the
/// approximation exactly like Solaar: 8→90, 4→50, 2→20, 1→5, else nothing.
pub fn hidpp_unified_battery(report: &[u8], device: u8, index: u8) -> Option<u8> {
    hidpp_reply_ok(report, device, index, (1 << 4) | HIDPP_SOFTWARE_ID)
        .then(|| match (report[4], report[5]) {
            (0, 8) => 90,
            (0, 4) => 50,
            (0, 2) => 20,
            (0, 1) => 5,
            (level, _) => level,
        })
        .filter(|level| (1..=100).contains(level))
}

/// Battery Level Status: (charge level, next level, discharge status); a
/// level of 0 is "unknown", not empty.
pub fn hidpp_battery_status(report: &[u8], device: u8, index: u8) -> Option<u8> {
    hidpp_reply_ok(report, device, index, HIDPP_SOFTWARE_ID)
        .then(|| report[4])
        .filter(|level| (1..=100).contains(level))
}

/// Battery Voltage (0x1001): [voltage_hi, voltage_lo, flags]; flags bit 7 is
/// the charging indicator. Returns (millivolt, charging).
pub fn hidpp_voltage_battery(report: &[u8], device: u8, index: u8) -> Option<(u16, bool)> {
    hidpp_reply_ok(report, device, index, HIDPP_SOFTWARE_ID).then(|| {
        (
            u16::from_be_bytes([report[4], report[5]]),
            report[6] & (1 << 7) != 0,
        )
    })
}

/// Charge percentage estimated from a Battery Voltage reading. Gaming mice
/// report millivolt, so the discharge curve is modelled: the measured
/// single-cell Li-Po table below matches Solaar's
/// `estimate_battery_level_percentage`, with linear interpolation between
/// points (OpenLogi documents the same derivation).
pub fn hidpp_voltage_percent(millivolt: u16) -> u8 {
    const CURVE: [(u16, u8); 13] = [
        (4186, 100),
        (4067, 90),
        (3989, 80),
        (3922, 70),
        (3859, 60),
        (3811, 50),
        (3778, 40),
        (3751, 30),
        (3717, 20),
        (3671, 10),
        (3646, 5),
        (3579, 2),
        (3500, 0),
    ];
    let (top_mv, top_percent) = CURVE[0];
    if millivolt >= top_mv {
        return top_percent;
    }
    for (&(high_mv, high_percent), &(low_mv, low_percent)) in CURVE.iter().zip(CURVE.iter().skip(1))
    {
        if millivolt >= low_mv {
            let span_mv = u32::from(high_mv - low_mv);
            let span_percent = u32::from(high_percent - low_percent);
            let above_low = u32::from(millivolt - low_mv);
            let interpolated =
                u32::from(low_percent) + (span_percent * above_low + span_mv / 2) / span_mv;
            return u8::try_from(interpolated).unwrap_or(high_percent);
        }
    }
    0
}

/// Battery Voltage reading collapsed to the displayed percentage.
pub fn hidpp_voltage_battery_percent(report: &[u8], device: u8, index: u8) -> Option<u8> {
    hidpp_voltage_battery(report, device, index).and_then(|(millivolt, _charging)| {
        let percent = hidpp_voltage_percent(millivolt);
        (percent > 0).then_some(percent)
    })
}

/// Device name replies carry raw bytes, so UTF-8 sequences split across
/// short-frame chunks stay decodable by the caller.
pub fn hidpp_name_length(report: &[u8], device: u8, index: u8) -> Option<usize> {
    hidpp_reply_ok(report, device, index, HIDPP_SOFTWARE_ID)
        .then(|| report[4] as usize)
        .filter(|length| *length > 0 && *length <= 32)
}

pub fn hidpp_name_text(report: &[u8], device: u8, index: u8) -> Option<&[u8]> {
    hidpp_reply_ok(report, device, index, (1 << 4) | HIDPP_SOFTWARE_ID).then(|| &report[4..])
}

// Unifying-family receiver registers; the Lightspeed receiver speaks the same
// HID++ 1.0 register protocol (cross-checked against Solaar and OpenLogi,
// which both classify 046d:C539 as Unifying-compatible). Requests go out as
// [0x10, device, 0x81/0x80, register, three bytes]; replies echo sub id and
// register, and 1.0 errors arrive as [0x8f, sub id, register, code].
pub const HIDPP_GET_REGISTER: u8 = 0x81;
pub const HIDPP_SET_REGISTER: u8 = 0x80;
/// Notification flags, 24-bit big-endian: bit 8 enables the wireless
/// device-connection notifications the slot enumeration relies on.
pub const HIDPP_REGISTER_NOTIFICATIONS: u8 = 0x00;
/// Connection states: read reports the pairing count, writing 0x02 0x00 0x00
/// makes the receiver re-broadcast one 0x41 arrival per paired slot.
pub const HIDPP_REGISTER_CONNECTIONS: u8 = 0x02;

pub fn hidpp_get_register(device: u8, register: u8) -> [u8; 7] {
    [HIDPP_SHORT, device, HIDPP_GET_REGISTER, register, 0, 0, 0]
}

pub fn hidpp_set_register(device: u8, register: u8, values: [u8; 3]) -> [u8; 7] {
    [
        HIDPP_SHORT,
        device,
        HIDPP_SET_REGISTER,
        register,
        values[0],
        values[1],
        values[2],
    ]
}

pub fn hidpp_get_register_reply(report: &[u8], device: u8, register: u8) -> Option<[u8; 3]> {
    hidpp_reply_ok(report, device, HIDPP_GET_REGISTER, register)
        .then_some([report[4], report[5], report[6]])
}

pub fn hidpp_set_register_ok(report: &[u8], device: u8, register: u8) -> bool {
    hidpp_reply_ok(report, device, HIDPP_SET_REGISTER, register)
}

/// The pairing count sits in the second byte of the connections register.
pub fn hidpp_pairing_count(reply: [u8; 3]) -> u8 {
    reply[1]
}

/// A `0x41` device-connection notification: [0x10, slot, 0x41, address,
/// protocol, device info, wpid low, (wpid high truncated in short frames)].
/// Device-info bit 6 set means the link is not established; the wpid reads
/// with data[2] as the high byte and data[1] as the low byte (Solaar:
/// `data[2:3] + data[1:2]`), so `…41 0c 22 7f 40` is wpid 0x407F.
pub fn hidpp_connection_notification(report: &[u8]) -> Option<(u8, bool, u16)> {
    (report.len() >= 7 && report[2] == 0x41).then(|| {
        (
            report[1],
            report[4] & (1 << 6) == 0,
            u16::from_be_bytes([report[6], report[5]]),
        )
    })
}

// Aula command protocol: the 32-byte vendor frames Aula Hub 2.2.3 exchanges
// with its SDK-channel keyboards (usage page FF60/FF68/FF80 collections on
// VID 0x0C45, e.g. the F87ProV2-series dongles). Layout and the battery
// exchange come from the deobfuscated renderer SDK (encodeReadKeyboardData-
// Command / decodeReadKeyboardDataResponse): [0] = cmd, [1] = mode, [6..8] =
// CRC-16/CCITT-FALSE over all 32 bytes with those two zeroed, stored low
// byte first. Replies echo cmd/mode, carry a status byte at [2] that must be
// 0, and the vendor code never checks their CRC — neither do we.
pub const AULA_CMD_BATTERY: u8 = 0x11;
pub const AULA_CMD_BATTERY_ALT: u8 = 0x51;
pub const AULA_MODE_BATTERY: u8 = 0x16;

/// CRC-16/CCITT-FALSE (poly 0x1021, init 0xFFFF, no reflection); check value
/// for "123456789" is 0x29B1.
pub fn aula_crc16(bytes: &[u8]) -> u16 {
    let mut crc: u16 = 0xffff;
    for &byte in bytes {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// One 32-byte command frame; the CRC slot sits zeroed inside the sum.
pub fn aula_command_request(cmd: u8, mode: u8) -> [u8; 32] {
    let mut frame = [0u8; 32];
    frame[0] = cmd;
    frame[1] = mode;
    let crc = aula_crc16(&frame);
    frame[6] = crc as u8;
    frame[7] = (crc >> 8) as u8;
    frame
}

/// The battery reply for mode 22: level at [8], charging flag at [9]. The
/// hub parses both as decimal strings; 0 is not a level it ever acts on, so
/// the same 1..=100 rule as the KZZI family applies. Windows reads carry a
/// report-id prefix (33 bytes) that bare frames lack (32 bytes).
pub fn aula_command_battery(report: &[u8], cmd: u8) -> Option<(u8, u8)> {
    let frame: &[u8] = match report.len() {
        33 => &report[1..33],
        32 => report,
        _ => return None,
    };
    if frame[0] != cmd || frame[1] != AULA_MODE_BATTERY || frame[2] != 0 {
        return None;
    }
    let (level, state) = (frame[8], frame[9]);
    ((1..=100).contains(&level)).then_some((level, state))
}
