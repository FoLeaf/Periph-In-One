use periph_hal::protocol::*;

fn seal(frame: &mut [u8; 17]) {
    frame[16] = 0x55u8.wrapping_sub(frame[..16].iter().fold(0u8, |sum, &b| sum.wrapping_add(b)));
}

#[test]
fn vxe_ff02_battery_checks_echo_status_lengths_checksum_and_level() {
    // Constructed from the contributor's documented F58E fields (60%,
    // charging 0, voltage 0F51); this is not a new live capture.
    let mut reply = atk_ff04_request(4);
    reply[5] = 2;
    reply[6] = 60;
    reply[8..10].copy_from_slice(&[0x0f, 0x51]);
    seal(&mut reply);
    assert_eq!(atk_v2_ff02_battery(&reply), Some((60, 0)));
    for length in 0..17 {
        assert_eq!(atk_v2_ff02_battery(&reply[..length]), None);
    }
    assert_eq!(
        atk_v2_ff02_battery(&[reply.as_slice(), &[0]].concat()),
        None
    );
    for index in 0..17 {
        let mut corrupt = reply;
        corrupt[index] ^= 1;
        assert!(!atk_v2_ff02_reply_ok(&corrupt));
    }
    for (index, value) in [(0, 9), (1, 3), (2, 1), (5, 0), (5, 11), (6, 0), (6, 101)] {
        let mut invalid = reply;
        invalid[index] = value;
        seal(&mut invalid);
        assert_eq!(atk_v2_ff02_battery(&invalid), None);
    }
    for level in [1, 100] {
        reply[6] = level;
        seal(&mut reply);
        assert_eq!(atk_v2_ff02_battery(&reply), Some((level, 0)));
    }
}

#[test]
fn vxe_ff02_online_reply_keeps_rf_identity_separate_from_battery() {
    let mut reply = atk_ff04_request(3);
    // The vendor reads the RF ID beyond dataValidLen in this reply.
    reply[5] = 1;
    reply[6] = 1;
    reply[7..10].copy_from_slice(&[0xa5, 0x20, 0xcf]);
    seal(&mut reply);
    assert_eq!(atk_v2_ff02_online(&reply), Some((1, [0xa5, 0x20, 0xcf])));
    assert_eq!(atk_v2_ff02_battery(&reply), None);
    reply[6] = 0;
    seal(&mut reply);
    assert_eq!(atk_v2_ff02_online(&reply), Some((0, [0xa5, 0x20, 0xcf])));
    reply[2] = 1;
    seal(&mut reply);
    assert_eq!(atk_v2_ff02_online(&reply), None);
    for length in 0..17 {
        assert_eq!(atk_v2_ff02_online(&reply[..length]), None);
    }
}

#[test]
fn vxe_ff02_model_reply_requires_three_valid_data_bytes() {
    let mut reply = atk_ff04_request(0x10);
    reply[5] = 3;
    reply[6..9].copy_from_slice(&[1, 2, 3]);
    seal(&mut reply);
    assert_eq!(atk_v2_ff02_cid_mid(&reply), Some((1, 2, 3)));
    for (index, value) in [(1, 4), (2, 1), (5, 2), (5, 11)] {
        let mut invalid = reply;
        invalid[index] = value;
        seal(&mut invalid);
        assert_eq!(atk_v2_ff02_cid_mid(&invalid), None);
    }
    assert_eq!(atk_v2_ff02_cid_mid(&reply[..16]), None);
}

#[test]
fn compx_model_table_covers_the_mchose_oem_line() {
    assert_eq!(compx_model_name(&[3, 0, 0, 0, 1, 21]), Some("MCHOSE G75"));
    assert_eq!(compx_model_name(&[3, 0, 0, 0, 1, 116]), Some("MCHOSE GX87"));
    assert_eq!(
        compx_model_name(&[3, 0, 0, 0, 1, 223]),
        Some("MCHOSE K99 V2")
    );
    assert_eq!(compx_model_name(&[3, 0, 0, 0, 1, 20]), None);
    assert_eq!(compx_model_name(&[3, 0, 0, 0, 1, 11]), Some("AULA F87 Pro"));
    assert_eq!(
        kzzi_model_name(&[2, 0, 0, 0, 0, 55]),
        Some("Kzzi-K75 RGB key")
    );
}

#[test]
fn ikf_query_frame_matches_the_official_pack() {
    let frame = ikf_battery_query(0);
    assert_eq!(
        &frame[..12],
        &[
            0x02, 0x55, 0x80, 0x00, 0x06, 0x00, 0xFF, 0, 0x02, 0xFA, 0x0C, 0xAA
        ]
    );
    assert!(frame[12..].iter().all(|&byte| byte == 0));
    assert_eq!(ikf_battery_query(7)[7], 7);
}

#[test]
fn ikf_battery_parses_the_contributor_capture_and_rejects_malformed_headers() {
    // Captured by the contributor on 38B1:3419 V11 Pro (70%).
    let reply = *b"\x03\x55\x80\x00\x08\x00\xff\x00\x04\x0c\x46\x00\x00\xaa";
    assert_eq!(ikf_battery(&reply), Some(70));
    let mut padded = [0; 64];
    padded[..14].copy_from_slice(&reply);
    assert_eq!(ikf_battery(&padded), Some(70));
    let heartbeat = *b"\x03\x55\x80\x00\x05\x00\xff\x00\x01\xfe\xaa\x00\x00\x00";
    assert_eq!(ikf_battery(&heartbeat), None);
    for index in [0, 1, 2, 3, 4, 5, 6, 8, 9, 13] {
        let mut invalid = reply;
        invalid[index] ^= 1;
        assert_eq!(ikf_battery(&invalid), None);
    }
    for length in 0..14 {
        assert_eq!(ikf_battery(&reply[..length]), None);
    }
    for level in [0, 101] {
        let mut invalid = reply;
        invalid[10] = level;
        assert_eq!(ikf_battery(&invalid), None);
    }
    for level in [1, 100] {
        let mut boundary = reply;
        boundary[10] = level;
        assert_eq!(ikf_battery(&boundary), Some(level));
    }
}

#[test]
fn vxe_v2_eeprom_dpi_requests_and_replies() {
    let req = atk_v2_current_dpi_request();
    assert_eq!(req[0], 8);
    assert_eq!(req[1], 8);
    assert_eq!(req[4], 4);
    assert_eq!(req[5], 1);
    assert!(atk_v2_ff02_reply_ok(&req));

    // Construct valid GetEEPROM reply for currentDpi = 2 (800 DPI)
    let mut reply = atk_ff04_request(8);
    reply[2] = 0; // status
    reply[4] = 4; // address low byte
    reply[5] = 1; // data len
    reply[6] = 2; // stage 2
    seal(&mut reply);

    assert_eq!(atk_v2_current_dpi(&reply), Some(2));

    // Write request for stage 3
    let set_req = atk_v2_set_current_dpi_request(3);
    assert_eq!(set_req[0], 8);
    assert_eq!(set_req[1], 7);
    assert_eq!(set_req[4], 4);
    assert_eq!(set_req[5], 1);
    assert_eq!(set_req[6], 3);
    assert!(atk_v2_ff02_reply_ok(&set_req));

    let mut set_reply = atk_ff04_request(7);
    set_reply[2] = 0;
    set_reply[4] = 4;
    seal(&mut set_reply);
    assert!(atk_v2_set_eeprom_ok(&set_reply, 4));
}

#[test]
fn atk_ff05_dpi_frames_and_checksum() {
    let req = atk_ff05_set_dpi_request(2);
    assert_eq!(req[0], 8);
    assert_eq!(req[2], 0x72);
    assert_eq!(req[5], 0x01);
    assert_eq!(req[6], 0x02); // SetDpi
    assert_eq!(req[8], 1); // 0-based index for stage 2
    assert_eq!(&req[9..11], &800u16.to_le_bytes()); // 800 DPI
    let expected_sum = req[2..].iter().fold(0u8, |sum, &b| sum.wrapping_add(b));
    assert_eq!(req[1], expected_sum);

    let config_req = atk_ff05_current_config_request();
    assert_eq!(config_req[0], 8);
    assert_eq!(config_req[2], 0x72);
    assert_eq!(config_req[6], 0x09); // GetCurrentMouseConfig
    let expected_config_sum = config_req[2..]
        .iter()
        .fold(0u8, |sum, &b| sum.wrapping_add(b));
    assert_eq!(config_req[1], expected_config_sum);

    let mut reply = [0u8; 64];
    reply[2] = 0x72;
    reply[6] = 0x09;
    reply[11..13].copy_from_slice(&1200u16.to_le_bytes());
    assert_eq!(atk_ff05_parse_current_dpi_value(&reply), Some(1200));
    assert_eq!(atk_dpi_value_to_stage(1200), Some(3));
}

#[test]
fn kzzi_lighting_profile_split_parse_and_toggle() {
    let mut dummy_profile = [0u8; 128];
    dummy_profile[10] = 0; // off
    dummy_profile[126] = 0x5a;
    dummy_profile[127] = 0xa5;

    assert!(!kzzi_profile_is_lit(&dummy_profile));

    let on_profile = kzzi_create_lighting_profile(&dummy_profile, true);
    assert!(kzzi_profile_is_lit(&on_profile));
    assert_eq!(on_profile[10], 11); // Official tc_eft11 = 随波逐流
    assert_eq!(&on_profile[78..80], &[4, 0x27]);

    let frames = kzzi_split_lighting_profile(&on_profile);
    assert_eq!(frames.len(), 10);
    for (i, frame) in frames.iter().enumerate() {
        assert_eq!(frame[0], 0x13);
        assert_eq!(frame[1], 0x04);
        assert_eq!(frame[2], 10);
        assert_eq!(frame[3], i as u8);
        assert!(kzzi_frame_ok(frame));
    }

    let parsed = kzzi_parse_lighting_profile(&frames).expect("reassembled profile");
    assert_eq!(parsed, on_profile);

    let off_profile = kzzi_create_lighting_profile(&on_profile, false);
    assert!(!kzzi_profile_is_lit(&off_profile));
    assert_eq!(off_profile[10], 0);

    let save_frame = kzzi_save_lighting_request();
    assert!(kzzi_frame_ok(&save_frame));
    assert_eq!(save_frame[1], 0x0A);
}

#[test]
fn kzzi_lighting_uses_official_effect_and_preserves_other_settings() {
    let mut original = std::array::from_fn(|index| index as u8);
    original[126..].copy_from_slice(&[0x5a, 0xa5]);
    let on = kzzi_create_lighting_profile(&original, true);
    for index in 0..128 {
        if ![9, 10, 78, 79].contains(&index) {
            assert_eq!(on[index], original[index], "unrelated byte {index}");
        }
    }
    assert_eq!(&on[78..80], &[4, 0x27]);
    assert_eq!(on[10], 11);
    assert!(kzzi_lighting_matches(&on, true));
    for (index, value) in [(10, 10), (78, 1), (79, 0x47), (9, 1)] {
        let mut wrong = on;
        wrong[index] = value;
        assert!(!kzzi_lighting_matches(&wrong, true));
    }
    let off = kzzi_create_lighting_profile(&on, false);
    assert!(kzzi_lighting_matches(&off, false));
    assert_eq!(&off[11..], &on[11..]);
    assert!(kzzi_lighting_supported(&[2, 0, 0, 0, 0, 55]));
    assert!(!kzzi_lighting_supported(&[2, 0, 0, 0, 0, 54]));
    assert!(!kzzi_lighting_supported(&[3, 0, 0, 0, 1, 11]));
}

#[test]
fn kzzi_lighting_rejects_malformed_pages() {
    let mut profile = [0; 128];
    profile[126..].copy_from_slice(&[0x5a, 0xa5]);
    let pages = kzzi_split_lighting_profile(&profile);
    let mut bad = pages;
    bad[0][19] ^= 1;
    assert!(kzzi_parse_lighting_profile(&bad).is_none());
    let mut bad = pages;
    bad.swap(0, 1);
    assert!(kzzi_parse_lighting_profile(&bad).is_none());
    for frame in [
        kzzi_chunk_frame(0x44, 10, 0, &[0; 13]),
        kzzi_chunk_frame(0x45, 10, 0, &[0; 14]),
        kzzi_chunk_frame(0x44, 9, 0, &[0; 14]),
    ] {
        let mut bad = pages;
        bad[0] = frame;
        assert!(kzzi_parse_lighting_profile(&bad).is_none());
    }
    profile[127] = 0;
    assert!(kzzi_parse_lighting_profile(&kzzi_split_lighting_profile(&profile)).is_none());
}
