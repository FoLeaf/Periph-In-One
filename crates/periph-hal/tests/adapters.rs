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
