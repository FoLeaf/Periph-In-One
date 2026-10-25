use periph_in_one::protocol::*;

#[test]
fn crc_reference_and_corrupt_frames() {
    assert_eq!(crc16(b"123456789"), 0x29b1);
    let mut report = darkrim_query(7, 1);
    report[9] = 0;
    assert_eq!(darkrim_tlvs(&report, 7).unwrap(), vec![(1, &[][..])]);
    assert!(darkrim_tlvs(&report, 8).is_none());
    report[11] ^= 1;
    assert!(darkrim_tlvs(&report, 7).is_none());
    assert!(darkrim_tlvs(&report[..10], 7).is_none());
}

#[test]
fn rejects_malformed_tlv_even_with_valid_crc() {
    let mut report = darkrim_query(1, 1);
    report[11] = 4;
    let crc = crc16(&report[1..12]).to_be_bytes();
    report[12..14].copy_from_slice(&crc);
    assert!(darkrim_tlvs(&report, 1).is_none());
}

#[test]
fn native_hidapi_zero_report_id_is_restored() {
    let frame = darkrim_query(7, 1);
    assert_eq!(restore_darkrim_report_id(&frame[1..]), frame);
    assert_eq!(restore_darkrim_report_id(&frame), frame);
    assert!(darkrim_tlvs(&restore_darkrim_report_id(&frame[1..]), 7).is_some());
}

#[test]
fn mouse_requires_correct_reply_and_valid_percentage() {
    let mut reply = [0; 64];
    reply[1] = 0x72;
    reply[5] = 7;
    reply[7] = 0;
    assert_eq!(atk_battery(&reply), Some(0));
    reply[7] = 101;
    assert_eq!(atk_battery(&reply), None);
    assert_eq!(atk_battery(&[]), None);
}

#[test]
fn atk_ff02_fallback_frame_and_level_gate() {
    let request = atk_ff02_request();
    assert_eq!(request.len(), 17);
    assert_eq!(&request[..2], &[8, 4]);
    assert!(request[2..].iter().all(|&byte| byte == 0));
    let mut reply = [0u8; 7];
    reply[6] = 76;
    assert_eq!(atk_ff02_battery(&reply), Some(76));
    reply[6] = 101;
    assert_eq!(atk_ff02_battery(&reply), None);
    assert_eq!(atk_ff02_battery(&reply[..6]), None);
}

#[test]
fn atk_ff04_battery_frame_and_reply_parse() {
    // Driver-derived wiring (ATK HUB Ke base): report id 8, command 4,
    // checksum 0x55 - (8 + 4) = 0x49.
    let request = atk_ff04_battery_request();
    assert_eq!(request.len(), 17);
    assert_eq!(&request[..2], &[0x08, 0x04]);
    assert_eq!(request[16], 0x49);
    assert!(request[2..16].iter().all(|&byte| byte == 0));

    let checksum = |reply: &[u8; 17]| {
        0x55u8.wrapping_sub(reply[..16].iter().fold(0u8, |sum, &b| sum.wrapping_add(b)))
    };
    // Reply: command echo 4, dataValidLen 2, level 88, charging 1, voltage
    // 0x012C ignored.
    let mut reply = [0u8; 17];
    reply[0] = 8;
    reply[1] = 0x04;
    reply[5] = 2;
    reply[6] = 88;
    reply[7] = 1;
    reply[8..10].copy_from_slice(&[0x01, 0x2c]);
    reply[16] = checksum(&reply);
    assert_eq!(atk_ff04_battery(&reply), Some((88, 1)));

    reply[16] ^= 1;
    assert_eq!(atk_ff04_battery(&reply), None);
    reply[16] = checksum(&reply);
    reply[1] = 0x10;
    assert_eq!(atk_ff04_battery(&reply), None);
    reply[1] = 0x04;
    reply[6] = 101;
    reply[16] = checksum(&reply);
    assert_eq!(atk_ff04_battery(&reply), None);
    assert_eq!(atk_ff04_battery(&reply[..10]), None);
    assert_eq!(atk_ff04_battery(&[]), None);
}

#[test]
fn kzzi_battery_reply_requires_checksum_command_and_level_range() {
    let mut reply =
        *b"\x13\x4a\x01\x00\x02\x61\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\xc2";
    assert_eq!(kzzi_battery(&reply), Some((0x61, 0x01)));
    reply[5] = 100;
    reply[6] = 0x10;
    reply[19] = reply[..19].iter().fold(0u8, |s, &b| s.wrapping_add(b));
    assert_eq!(kzzi_battery(&reply), Some((100, 0x10)));
    for level in [0u8, 101] {
        reply[5] = level;
        reply[19] = reply[..19].iter().fold(0u8, |s, &b| s.wrapping_add(b));
        assert_eq!(kzzi_battery(&reply), None);
    }
    reply[19] ^= 1;
    assert_eq!(kzzi_battery(&reply), None);
    assert_eq!(kzzi_battery(&reply[..12]), None);
    assert_eq!(kzzi_battery(&[]), None);
    // Unrelated frames the receiver also streams must be ignored.
    let mut status =
        *b"\x13\x0a\x01\x00\x02\x61\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x9b";
    status[19] = status[..19].iter().fold(0u8, |s, &b| s.wrapping_add(b));
    assert_eq!(kzzi_battery(&status), None);
}

#[test]
fn kzzi_identity_reply_yields_the_model_signature() {
    let reply =
        *b"\x13\x05\x01\x00\x0a\x02\x00\x00\x00\x00\x37\x00\x01\x00\x00\x00\x00\x00\x00\x5d";
    assert_eq!(kzzi_identity(&reply), Some([2, 0, 0, 0, 0, 55]));
    assert_eq!(
        kzzi_model_name(&[2, 0, 0, 0, 0, 55]),
        Some("Kzzi-K75 RGB key")
    );
    assert_eq!(kzzi_model_name(&[2, 0, 0, 0, 0, 54]), Some("Kzzi-K75 RGB"));
    // AULA F87 Pro signature measured from the device (identity 0x05 reply).
    assert_eq!(kzzi_model_name(&[3, 0, 0, 0, 1, 11]), Some("AULA F87 Pro"));
    assert_eq!(kzzi_model_name(&[9, 9, 9, 9, 9, 9]), None);
    let mut corrupt = reply;
    corrupt[19] ^= 1;
    assert_eq!(kzzi_identity(&corrupt), None);
    assert_eq!(kzzi_identity(&reply[..9]), None);
    // A battery reply is not an identity reply.
    assert_eq!(kzzi_identity(kzzi_request(0x4a).as_slice()), None);
}

#[test]
fn kzzi_battery_private_covers_only_the_verified_profile() {
    // Kzzi-K75 RGB key, live-verified silent on 0x4A while linked.
    assert!(kzzi_battery_private(&[2, 0, 0, 0, 0, 55]));
    // The K75 RGB sibling and unknown profiles stay unverified: silence
    // there may still mean the keyboard is off.
    assert!(!kzzi_battery_private(&[2, 0, 0, 0, 0, 54]));
    assert!(!kzzi_battery_private(&[9, 9, 9, 9, 9, 9]));
    assert!(!kzzi_battery_private(&[]));
}

#[test]
fn bigbigwon_request_frame_matches_verified_wiring() {
    let report = bigbigwon_request();
    assert_eq!(report.len(), 65);
    assert_eq!(&report[..5], &[0x00, 0xa5, 0x04, 0x04, 0xad]);
    assert!(report[5..].iter().all(|&byte| byte == 0));
    // GetWirelessFlag mirrors DevMgr.dll's precomputed constant A5 04 FA A3.
    assert_eq!(
        &bigbigwon_short_request(0xfa)[..5],
        &[0x00, 0xa5, 0x04, 0xfa, 0xa3]
    );
}

#[test]
fn bigbigwon_battery_parses_verified_power_reply() {
    // Captured replies: 88%/90% alternation from a near-full GALE controller.
    let reply = [0xa5, 0x06, 0x04, 0x58, 0x00, 0x07];
    assert_eq!(bigbigwon_battery(&reply), Some(88));
    let reply = [0xa5, 0x06, 0x04, 0x5a, 0x00, 0x09];
    assert_eq!(bigbigwon_battery(&reply), Some(90));
    // The trailing zeros of the 64-byte hidapi report must not disturb parsing.
    let mut padded = [0u8; 64];
    padded[..6].copy_from_slice(&[0xa5, 0x06, 0x04, 0x58, 0x00, 0x07]);
    assert_eq!(bigbigwon_battery(&padded), Some(88));
}

#[test]
fn bigbigwon_battery_rejects_bad_frames() {
    // Broken additive checksum.
    assert_eq!(
        bigbigwon_battery(&[0xa5, 0x06, 0x04, 0x58, 0x00, 0x08]),
        None
    );
    // Wrong command echo.
    assert_eq!(
        bigbigwon_battery(&[0xa5, 0x06, 0x0b, 0x36, 0xeb, 0x00]),
        None
    );
    // Level beyond 100%.
    assert_eq!(
        bigbigwon_battery(&[0xa5, 0x06, 0x04, 0xc8, 0x00, 0x77]),
        None
    );
    // Truncated and wrong-magic frames.
    assert_eq!(bigbigwon_battery(&[0xa5, 0x06, 0x04]), None);
    assert_eq!(
        bigbigwon_battery(&[0x13, 0x06, 0x04, 0x58, 0x00, 0x07]),
        None
    );
    // Length byte larger than the buffer.
    assert_eq!(bigbigwon_battery(&[0xa5, 0xff, 0x04, 0x58]), None);
    // Zero length byte must not underflow.
    assert_eq!(bigbigwon_battery(&[0xa5, 0x00]), None);
    // A longer frame with a valid checksum: the vendor handler fixes len at 6.
    assert_eq!(
        bigbigwon_battery(&[0xa5, 0x07, 0x04, 0x58, 0x00, 0x00, 0x08]),
        None
    );
}

#[test]
fn bigbigwon_uuid_request_mirrors_the_official_client() {
    let request = bigbigwon_uuid_request();
    assert_eq!(request.len(), 65);
    assert_eq!(&request[1..4], &[0xa5, 0x0c, 0xef]);
    assert_eq!(request[12], 0xa0);
    assert!(request[13..].iter().all(|&byte| byte == 0));
}

#[test]
fn bigbigwon_uuid_parses_the_captured_identity_reply() {
    // Captured live from a linked GALE controller.
    let reply = [
        0xa5, 0x0c, 0xef, 0x42, 0x3b, 0xca, 0x92, 0x86, 0x3c, 0xba, 0x8d, 0x82,
    ];
    assert_eq!(
        bigbigwon_uuid(&reply),
        Some([0x42, 0x3b, 0xca, 0x92, 0x86, 0x3c, 0xba, 0x8d])
    );
    // The trailing zeros of the hidapi report must not disturb parsing.
    let mut padded = [0u8; 64];
    padded[..reply.len()].copy_from_slice(&reply);
    assert_eq!(
        bigbigwon_uuid(&padded),
        Some([0x42, 0x3b, 0xca, 0x92, 0x86, 0x3c, 0xba, 0x8d])
    );
    let mut corrupt = reply;
    corrupt[11] ^= 1;
    assert_eq!(bigbigwon_uuid(&corrupt), None);
    // Wrong opcode echo and truncated frames.
    assert_eq!(
        bigbigwon_uuid(&[0xa5, 0x0c, 0x04, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
        None
    );
    assert_eq!(bigbigwon_uuid(&[0xa5, 0x0c, 0xef, 0x42]), None);
    assert_eq!(bigbigwon_uuid(&[]), None);
}

#[test]
fn hidpp_ping_frames_match_only_the_original_device_reply() {
    let request = hidpp_ping(1);
    assert_eq!(
        &request[..8],
        &[
            HIDPP_LONG,
            0x01,
            0x00,
            0x10 | HIDPP_SOFTWARE_ID,
            0,
            0,
            0x42,
            0
        ]
    );
    // Captured LIGHTSPEED reply: long frame, protocol 4.2, echoed mark.
    let reply = [HIDPP_LONG, 0x01, 0x00, 0x10 | HIDPP_SOFTWARE_ID, 4, 2, 0x42];
    assert_eq!(hidpp_ping_reply(&reply, 1), Some((4, 2)));
    // Another device's reply, a truncated frame and a notification
    // (software id 0) must never look like a ping answer.
    assert_eq!(hidpp_ping_reply(&reply, 2), None);
    assert_eq!(hidpp_ping_reply(&reply[..6], 1), None);
    let notification = [HIDPP_LONG, 0x01, 0x00, 0x10, 4, 2, 0x42];
    assert_eq!(hidpp_ping_reply(&notification, 1), None);
}

#[test]
fn hidpp_get_feature_index_requires_an_explicit_reply() {
    let request = hidpp_get_feature(1, HIDPP_FEATURE_UNIFIED_BATTERY);
    assert_eq!(
        &request[..6],
        &[HIDPP_LONG, 0x01, 0x00, HIDPP_SOFTWARE_ID, 0x10, 0x04]
    );
    let mut reply = [HIDPP_LONG, 0x01, 0x00, HIDPP_SOFTWARE_ID, 0x07, 0, 0];
    assert_eq!(hidpp_feature_index(&reply, 1), Some(0x07));
    // An unsupported feature reports index 0, not a failure.
    reply[4] = 0;
    assert_eq!(hidpp_feature_index(&reply, 1), None);
    assert_eq!(hidpp_feature_index(&reply[..5], 1), None);
}

#[test]
fn hidpp_battery_status_treats_zero_level_as_unknown() {
    let reply = [0x10, 0x01, 0x05, HIDPP_SOFTWARE_ID, 55, 60, 1];
    assert_eq!(hidpp_battery_status(&reply, 1, 0x05), Some(55));
    assert_eq!(
        hidpp_battery_status(&[0x10, 0x01, 0x05, HIDPP_SOFTWARE_ID, 0, 0, 0], 1, 0x05),
        None
    );
    assert_eq!(hidpp_battery_status(&reply, 2, 0x05), None);
    assert_eq!(hidpp_battery_status(&[], 1, 0x05), None);
}

#[test]
fn hidpp_name_chunks_are_returned_as_raw_bytes() {
    let request = hidpp_feature_call1(1, 0x07, 1, 3);
    assert_eq!(
        &request[..5],
        &[HIDPP_LONG, 0x01, 0x07, (1 << 4) | HIDPP_SOFTWARE_ID, 3]
    );
    // Long-frame replies carry the chunk straight after the echo.
    let reply = [
        HIDPP_LONG,
        0x01,
        0x07,
        (1 << 4) | HIDPP_SOFTWARE_ID,
        b'G',
        b'5',
        b'0',
    ];
    assert_eq!(hidpp_name_text(&reply, 1, 0x07), Some(&b"G50"[..]));
    assert_eq!(hidpp_name_text(&reply, 1, 0x08), None);
    assert_eq!(hidpp_name_text(&reply[..6], 1, 0x07), None);
    let length = [HIDPP_LONG, 0x01, 0x07, HIDPP_SOFTWARE_ID, 15, 0, 0];
    assert_eq!(hidpp_name_length(&length, 1, 0x07), Some(15));
    // Zero and oversized lengths are rejected so the chunk loop is bounded.
    assert_eq!(
        hidpp_name_length(
            &[HIDPP_LONG, 0x01, 0x07, HIDPP_SOFTWARE_ID, 0, 0, 0],
            1,
            0x07
        ),
        None
    );
    assert_eq!(
        hidpp_name_length(
            &[HIDPP_LONG, 0x01, 0x07, HIDPP_SOFTWARE_ID, 33, 0, 0],
            1,
            0x07
        ),
        None
    );
}

#[test]
fn hidpp_receiver_registers_follow_the_rap_layout() {
    let read = hidpp_get_register(0xff, HIDPP_REGISTER_NOTIFICATIONS);
    assert_eq!(&read[..], &[0x10, 0xff, 0x81, 0x00, 0, 0, 0]);
    let write = hidpp_set_register(0xff, HIDPP_REGISTER_CONNECTIONS, [0x02, 0, 0]);
    assert_eq!(&write[..], &[0x10, 0xff, 0x80, 0x02, 0x02, 0, 0]);
    // A read reply echoes sub id and register before the three data bytes.
    let reply = [0x10, 0xff, 0x81, 0x00, 0x00, 0x01, 0x00];
    assert_eq!(
        hidpp_get_register_reply(&reply, 0xff, HIDPP_REGISTER_NOTIFICATIONS),
        Some([0x00, 0x01, 0x00])
    );
    assert_eq!(hidpp_pairing_count([0x00, 0x01, 0x00]), 1);
    // Another register or an error frame never satisfies the matcher.
    assert_eq!(
        hidpp_get_register_reply(&reply, 0xff, HIDPP_REGISTER_CONNECTIONS),
        None
    );
    assert_eq!(
        hidpp_get_register_reply(&[0x10, 0xff, 0x8f, 0x00, 0x01, 0x09, 0x00], 0xff, 0x00),
        None
    );
    assert!(hidpp_set_register_ok(
        &[0x10, 0xff, 0x80, 0x02, 0x00, 0x00, 0x00],
        0xff,
        0x02
    ));
}

#[test]
fn hidpp_arrival_notification_reports_slot_link_and_wpid() {
    // Captured from a LIGHTSPEED receiver: slot 1, mouse, G502 wpid 0x407F.
    let online = [0x10, 0x01, 0x41, 0x0c, 0x22, 0x7f, 0x40];
    assert_eq!(
        hidpp_connection_notification(&online),
        Some((0x01, true, 0x407f))
    );
    // Device-info bit 6 set: the link is not established.
    let offline = [0x10, 0x01, 0x41, 0x0c, 0x62, 0x7f, 0x40];
    assert_eq!(
        hidpp_connection_notification(&offline),
        Some((0x01, false, 0x407f))
    );
    assert_eq!(hidpp_connection_notification(&offline[..6]), None);
    assert_eq!(
        hidpp_connection_notification(&[0x10, 0x01, 0x40, 0x0c, 0, 0, 0]),
        None
    );
}

#[test]
fn aula_command_frames_carry_the_ccitt_crc() {
    // CRC-16/CCITT-FALSE check value for "123456789" is 0x29B1.
    assert_eq!(aula_crc16(b"123456789"), 0x29b1);
    // Battery request frames from the vendor SDK: cmd 0x11 (0x51 fallback),
    // mode 0x16, CRC low byte first at [6..8] over the zeroed frame.
    let request = aula_command_request(AULA_CMD_BATTERY, AULA_MODE_BATTERY);
    assert_eq!(
        &request[..8],
        &[0x11, 0x16, 0x00, 0x00, 0x00, 0x00, 0xc2, 0x1b]
    );
    let fallback = aula_command_request(AULA_CMD_BATTERY_ALT, AULA_MODE_BATTERY);
    assert_eq!(
        &fallback[..8],
        &[0x51, 0x16, 0x00, 0x00, 0x00, 0x00, 0xb5, 0x38]
    );
}

#[test]
fn aula_command_battery_reads_level_and_charging_state() {
    // A Windows read carries the report-id prefix: [0]=0, frame at 1..33,
    // level at frame[8], charging flag at frame[9].
    let mut reply = [0u8; 33];
    reply[1] = AULA_CMD_BATTERY;
    reply[2] = AULA_MODE_BATTERY;
    reply[9] = 73;
    reply[10] = 1;
    assert_eq!(
        aula_command_battery(&reply, AULA_CMD_BATTERY),
        Some((73, 1))
    );
    // A bare 32-byte frame parses the same way.
    let mut bare = [0u8; 32];
    bare[0] = AULA_CMD_BATTERY;
    bare[1] = AULA_MODE_BATTERY;
    bare[7] = 5;
    bare[8] = 100;
    assert_eq!(
        aula_command_battery(&bare, AULA_CMD_BATTERY),
        Some((100, 0))
    );
    // Wrong echo, non-zero status, short frames and level 0 are rejected.
    assert_eq!(aula_command_battery(&reply, AULA_CMD_BATTERY_ALT), None);
    let mut bad_status = reply;
    bad_status[3] = 1;
    assert_eq!(aula_command_battery(&bad_status, AULA_CMD_BATTERY), None);
    let mut empty = reply;
    empty[9] = 0;
    assert_eq!(aula_command_battery(&empty, AULA_CMD_BATTERY), None);
    assert_eq!(aula_command_battery(&reply[..32], AULA_CMD_BATTERY), None);
    assert_eq!(aula_command_battery(&reply[..20], AULA_CMD_BATTERY), None);
}
