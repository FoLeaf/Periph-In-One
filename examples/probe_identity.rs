//! Probe ATK(FF05), KZZI(0x13 frames) and DARKRIM(TLV) vendor command
//! spaces for identity commands (serial / MAC / firmware / uid).
//! Only frames shaped like the verified read-only commands are sent.
//!
//! Research verdict (community docs + live probing, 2026-10):
//! - ATK: the atkctl-documented protocol exposes no per-unit identity.
//!   0x4A/0x89 mouse ID is company + model only; battery 0x07, charging
//!   0x0F and receiver firmware 0x88 answer live, the FF05 subcommand space
//!   0x00-0x1F holds no identity data.
//! - KZZI: 0x05 identity is the model PSD (npkg has no extended package);
//!   the 0x43 block is empty and the wired-only 0x44 config read does not
//!   answer over the radio. No per-unit identity in 0x00-0x1F / 0x40-0x50.
//! - DARKRIM: probe reserved; hardware pending.
//!
//! Frame construction, checksums and the control-endpoint link all reuse
//! the app's protocol/devices code, so this probe cannot drift from it.
use hidapi::HidApi;
use periph_hal::diagnostics::{KzziLink, drain, send};
use periph_in_one::protocol;
use std::ffi::CString;
use std::time::{Duration, Instant};

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn atk_pass(api: &HidApi) {
    let Some(path) = api
        .device_list()
        .find(|d| d.vendor_id() == 0x373b && d.usage_page() == 0xff05)
        .map(|d| CString::new(d.path().to_string_lossy().into_owned()).unwrap())
    else {
        println!("== ATK: 未连接，跳过 ==");
        return;
    };
    let reader = api.open_path(&path).expect("打开 ATK 读句柄失败");
    let writer = api.open_path(&path).expect("打开 ATK 写句柄失败");
    println!("== ATK FF05 命令空间 ==");
    let mut buffer = [0u8; 256];
    for sub in 0u8..0x20 {
        let request = protocol::atk_sub_request(sub);
        drain(&reader);
        if send(&writer, &request).is_err() {
            continue;
        }
        let deadline = Instant::now() + Duration::from_millis(160);
        while Instant::now() < deadline {
            if let Ok(n) = reader.read_timeout(&mut buffer, 40)
                && n > 1
                && buffer[1] == 0x72
            {
                let echo = buffer.get(5) == Some(&sub);
                println!(
                    "sub {sub:02X}{}: {}",
                    if echo { " 回显" } else { "      " },
                    hex(&buffer[..n.min(16)])
                );
                break;
            }
        }
    }

    // The FF02 interface of the same dongle carries a second frame family
    // (report id 8 requests). Dump the known request's full answer once to
    // look for identity bytes beyond the battery position.
    if let Some(ff02) = api
        .device_list()
        .find(|d| d.vendor_id() == 0x373b && d.usage_page() == 0xff02)
        .map(|d| CString::new(d.path().to_string_lossy().into_owned()).unwrap())
    {
        let reader = api.open_path(&ff02).expect("打开 ATK FF02 读句柄失败");
        let writer = api.open_path(&ff02).expect("打开 ATK FF02 写句柄失败");
        println!("== ATK FF02 已知请求完整应答 ==");
        let mut request = [0u8; 17];
        request[0] = 8;
        request[1] = 4;
        drain(&reader);
        if send(&writer, &request).is_ok() {
            let deadline = Instant::now() + Duration::from_millis(300);
            while Instant::now() < deadline {
                if let Ok(n) = reader.read_timeout(&mut buffer, 60)
                    && n > 0
                {
                    println!("FF02 应答 {n} 字节: {}", hex(&buffer[..n.min(24)]));
                    break;
                }
            }
        }
    }
}

/// The atkctl-documented frame: report id 8, then a 63-byte payload whose
/// byte 0 is the checksum (low byte of the sum of payload bytes 1..), byte 1
/// the 0x72 class, then param length, sequence 0, target (1 = mouse behind
/// the receiver) and the command byte.
fn atk_frame(plen: u8, target: u8, command: u8, args: &[u8]) -> [u8; 64] {
    let mut report = [0u8; 64];
    report[0] = 0x08;
    report[2] = 0x72;
    report[3] = plen;
    report[4] = 0x00;
    report[5] = target;
    report[6] = command;
    let mut sum = 0x72u8
        .wrapping_add(plen)
        .wrapping_add(target)
        .wrapping_add(command);
    for (slot, &arg) in report[7..].iter_mut().zip(args) {
        *slot = arg;
        sum = sum.wrapping_add(arg);
    }
    report[1] = sum;
    report
}

fn atk_targeted(api: &HidApi) {
    let Some(path) = api
        .device_list()
        .find(|d| d.vendor_id() == 0x373b && d.usage_page() == 0xff05)
        .map(|d| CString::new(d.path().to_string_lossy().into_owned()).unwrap())
    else {
        println!("== ATK 定向探测: 未连接 ==");
        return;
    };
    let reader = api.open_path(&path).expect("打开 ATK 读句柄失败");
    let writer = api.open_path(&path).expect("打开 ATK 写句柄失败");
    println!("== ATK 定向身份探测 (atkctl 文档命令) ==");
    let mut buffer = [0u8; 256];
    let probes: [(&str, [u8; 64]); 4] = [
        // Mouse ID (company + model id u32), target 1, read.
        ("MouseID 0x4A", atk_frame(0x08, 0x01, 0x4a, &[0x00])),
        // Same query with the receiver-relayed command.
        ("MouseID 0x89", atk_frame(0x08, 0x01, 0x89, &[0x00])),
        // Receiver firmware version.
        ("RecvFW 0x88", atk_frame(0x05, 0x00, 0x88, &[0x00])),
        // Mouse firmware version.
        ("MouseFW 0x1C", atk_frame(0x05, 0x01, 0x1c, &[0x00])),
    ];
    for (label, request) in probes {
        let command = request[6];
        drain(&reader);
        if send(&writer, &request).is_err() {
            continue;
        }
        let deadline = Instant::now() + Duration::from_millis(250);
        while Instant::now() < deadline {
            if let Ok(n) = reader.read_timeout(&mut buffer, 50)
                && n > 6
                && buffer[1] == 0x72
                && buffer[5] == command
            {
                println!("{label}: {}", hex(&buffer[..n.min(20)]));
                break;
            }
        }
    }
}

fn kzzi_pass(api: &HidApi) {
    let Some(path) = api
        .device_list()
        .find(|d| {
            d.vendor_id() == 0x3554
                && d.product_id() == 0xfa09
                && d.usage_page() == 0xff02
                && d.usage() == 2
        })
        .map(|d| d.path().to_string_lossy().into_owned())
    else {
        println!("== KZZI: 未连接，跳过 ==");
        return;
    };
    // The receiver only answers control-endpoint writes; the app's KzziLink
    // owns that transport.
    let Ok(link) = KzziLink::open(&path) else {
        println!("== KZZI: 接口打开失败 ==");
        return;
    };
    println!("== KZZI 0x13 命令空间 ==");
    let mut buffer = [0u8; 64];
    let commands: Vec<u8> = (0x00..=0x1f).chain(0x40..=0x50).collect();
    for cmd in commands {
        let frame = protocol::kzzi_request(cmd);
        link.drain();
        if link.send(&frame).is_err() {
            continue;
        }
        let deadline = Instant::now() + Duration::from_millis(120);
        while Instant::now() < deadline {
            if let Some(n) = link.read(&mut buffer, Duration::from_millis(40))
                && protocol::kzzi_frame_ok(&buffer[..n])
            {
                let echo = buffer[1] & 0x7f == cmd;
                println!(
                    "cmd {cmd:02X}{}: {}",
                    if echo { " 回显" } else { "      " },
                    hex(&buffer[..n])
                );
                break;
            }
        }
    }

    // Identity answers carry npkg in byte 2; ask for a second package in
    // case extended identity (serial / address) hides there, and capture
    // both frames of the exchange.
    println!("== KZZI identity npkg=2 ==");
    for npkg in [1u8, 2, 3] {
        let frame = protocol::kzzi_package_request(npkg);
        link.drain();
        if link.send(&frame).is_err() {
            continue;
        }
        let deadline = Instant::now() + Duration::from_millis(400);
        while Instant::now() < deadline {
            if let Some(n) = link.read(&mut buffer, Duration::from_millis(60))
                && protocol::kzzi_frame_ok(&buffer[..n])
            {
                println!("npkg {npkg} 应答: {}", hex(&buffer[..n]));
            }
        }
    }

    // Documented multi-package blocks: 0x44 is the 128-byte config block on
    // the wired transport; read them fully over the radio transport and look
    // for pairing-address-like stable bytes.
    println!("== KZZI 块读取 ==");
    for command in [0x44u8, 0x43] {
        let frame = protocol::kzzi_request(command);
        link.drain();
        if link.send(&frame).is_err() {
            continue;
        }
        let mut packages: Vec<(u8, [u8; 14])> = Vec::new();
        let deadline = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < deadline {
            if let Some(n) = link.read(&mut buffer, Duration::from_millis(80))
                && protocol::kzzi_frame_ok(&buffer[..n])
                && buffer[1] == command
            {
                let mut data = [0u8; 14];
                data.copy_from_slice(&buffer[5..19]);
                let index = buffer[3];
                if !packages.iter().any(|(i, _)| *i == index) {
                    packages.push((index, data));
                }
            }
        }
        packages.sort_by_key(|(index, _)| *index);
        let total: usize = packages.iter().map(|(_, data)| data.len()).sum();
        println!("cmd {command:02X}: {total} 字节，{} 个包", packages.len());
        for (index, data) in packages.iter().take(6) {
            println!("  idx {index:02X}: {}", hex(data));
        }
    }
}

fn darkrim_pass(api: &HidApi) {
    let Some(path) = api
        .device_list()
        .find(|d| d.vendor_id() == 0x3654 && d.usage_page() == 0xff02)
        .map(|d| CString::new(d.path().to_string_lossy().into_owned()).unwrap())
    else {
        println!("== DARKRIM: 未连接，跳过 ==");
        return;
    };
    let reader = api.open_path(&path).expect("打开 DARKRIM 读句柄失败");
    let writer = api.open_path(&path).expect("打开 DARKRIM 写句柄失败");
    println!("== DARKRIM TLV kind 枚举 ==");
    let mut buffer = [0u8; 256];
    for kind in 0u8..=8 {
        // Enumerate kinds by varying the sequence id and the kind together:
        // the app's headset query uses seq 0/1, kind 1/2.
        let report = protocol::darkrim_query(kind, kind);
        drain(&reader);
        if send(&writer, &report).is_err() {
            continue;
        }
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            if let Ok(n) = reader.read_timeout(&mut buffer, 60)
                && n > 2
                && buffer[0] == 0xfe
            {
                println!("kind {kind}: {}", hex(&buffer[..n.min(32)]));
                break;
            }
        }
    }
}

fn main() {
    let api = HidApi::new().expect("hidapi 初始化失败");
    atk_pass(&api);
    atk_targeted(&api);
    darkrim_pass(&api);
    kzzi_pass(&api);
}
