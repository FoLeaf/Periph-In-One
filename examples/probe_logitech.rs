//! Probe Logitech Unifying/Lightspeed/Bolt receivers and connected devices.
//! Read-only inspection of HID++ protocol features, device classification,
//! battery levels, and DPI capabilities.
//!
//! Observed on Lightspeed/Unifying/Bolt:
//! - Receivers expose two FF00 collections: usage 0x01 (short 0x10 frames / registers),
//!   usage 0x02 (long 0x11 frames / device tunnel).
//! - Short register 0x81 on 0x02 reports paired count.
//! - Ping [0x11, slot, 0x00, 0x1B, 0x00, 0x00, 0x42] tests liveness and version.
//! - Feature 0x0003: Model / WPID / Firmware info.
//! - Feature 0x0005: Device Name and Device Type (mouse vs keyboard).
//! - Feature 0x1004, 0x1000, 0x1001: Battery status/voltage.
//! - Feature 0x2201, 0x2202: Adjustable DPI / Extended Adjustable DPI.

use hidapi::{HidApi, HidDevice};
use std::time::{Duration, Instant};

const VID: u16 = 0x046d;

#[derive(Clone, Copy, Debug)]
enum ReceiverFamily {
    Lightspeed,
    Unifying,
    Bolt,
}

impl ReceiverFamily {
    fn from_pid(pid: u16) -> Option<Self> {
        match pid {
            0xc539 | 0xc53a | 0xc53d | 0xc53f | 0xc541 | 0xc545 | 0xc547 | 0xc54d => {
                Some(Self::Lightspeed)
            }
            0xc52b | 0xc532 => Some(Self::Unifying),
            0xc548 => Some(Self::Bolt),
            _ => None,
        }
    }

    fn slot_capacity(self) -> u8 {
        match self {
            Self::Lightspeed => 1,
            Self::Unifying | Self::Bolt => 6,
        }
    }
}

fn dump(label: &str, frame: &[u8]) {
    let hex = frame
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    println!("{label} {hex}");
}

/// Send one frame, return first matching reply or None.
fn query(reader: &HidDevice, writer: &HidDevice, frame: &[u8], timeout: Duration) -> Option<Vec<u8>> {
    let mut buffer = [0u8; 64];
    while reader.read_timeout(&mut buffer, 10).unwrap_or(0) > 0 {}
    if writer.write(frame).is_err() {
        return None;
    }
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        match reader.read_timeout(&mut buffer, 60) {
            Ok(n) if n > 0 => {
                return Some(buffer[..n].to_vec());
            }
            _ => continue,
        }
    }
    None
}

fn decode_device_type(code: u8) -> &'static str {
    match code {
        0 => "Keyboard (键盘)",
        1 => "Remote Control (遥控器)",
        2 => "Numpad (数字键盘)",
        3 => "Mouse (鼠标)",
        4 => "Trackball (轨迹球)",
        5 => "Touchpad (触控板)",
        _ => "未知外设类型",
    }
}

fn probe_slot(long_device: &HidDevice, slot: u8) {
    println!("\n--- 正在探测槽位 {slot} ---");

    // Ping device: [0x11, slot, 0x00, 0x1B, 0x00, 0x00, 0x42]
    let ping_cmd = [0x11, slot, 0x00, 0x1b, 0x00, 0x00, 0x42];
    let ping_res = query(long_device, long_device, &ping_cmd, Duration::from_millis(500));

    let Some(reply) = ping_res else {
        println!("  槽位 {slot}: 设备离线或未配对 (Ping 无应答)");
        return;
    };

    if reply.len() < 7 || reply[6] != 0x42 {
        dump("  Ping 异常应答: ", &reply);
        return;
    }

    let major = reply[4];
    let minor = reply[5];
    println!("  设备在线！HID++ 协议版本: {major}.{minor}");

    // Helper to call Root.GetFeature (feature 0x00, fn 0)
    let get_feature_index = |feat_id: u16| -> Option<u8> {
        let cmd = [
            0x11,
            slot,
            0x00,
            0x0b, // Root.GetFeature
            (feat_id >> 8) as u8,
            feat_id as u8,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ];
        let rep = query(long_device, long_device, &cmd, Duration::from_millis(400))?;
        if rep.len() >= 5 && rep[1] == slot && rep[2] == 0x00 {
            let index = rep[4];
            if index != 0 {
                return Some(index);
            }
        }
        None
    };

    // 1. Feature 0x0003: Model & Firmware Information
    if let Some(idx_0003) = get_feature_index(0x0003) {
        println!("  [Feature 0x0003] 固件/型号信息存在 (index = 0x{idx_0003:02X})");
        // fn2: GetModelId
        let cmd = [0x11, slot, idx_0003, (2 << 4) | 0x0b, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        if let Some(rep) = query(long_device, long_device, &cmd, Duration::from_millis(400)) {
            if rep.len() >= 6 && rep[1] == slot {
                let wpid = u16::from_be_bytes([rep[4], rep[5]]);
                println!("    -> Model ID / WPID: 0x{wpid:04X}");
            }
        }
        // fn3: GetExtendedModelId
        let cmd_ext = [0x11, slot, idx_0003, (3 << 4) | 0x0b, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        if let Some(rep) = query(long_device, long_device, &cmd_ext, Duration::from_millis(400)) {
            if rep.len() >= 6 && rep[1] == slot {
                let ext_id = u16::from_be_bytes([rep[4], rep[5]]);
                println!("    -> Extended Model ID: 0x{ext_id:04X}");
            }
        }
    } else {
        println!("  [Feature 0x0003] 未实现");
    }

    // 2. Feature 0x0005: Device Name & Type
    if let Some(idx_0005) = get_feature_index(0x0005) {
        println!("  [Feature 0x0005] 设备名与类型存在 (index = 0x{idx_0005:02X})");
        // fn0: GetNameCount
        let mut name = String::new();
        let cmd = [0x11, slot, idx_0005, (0 << 4) | 0x0b, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        if let Some(rep) = query(long_device, long_device, &cmd, Duration::from_millis(400)) {
            if rep.len() >= 5 {
                let len = rep[4] as usize;
                // fn1: GetDeviceName chunks
                let mut offset = 0;
                while offset < len {
                    let cmd_name = [0x11, slot, idx_0005, (1 << 4) | 0x0b, offset as u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
                    if let Some(chunk) = query(long_device, long_device, &cmd_name, Duration::from_millis(400)) {
                        if chunk.len() >= 5 {
                            let available = &chunk[4..];
                            let take = (len - offset).min(available.len());
                            if let Ok(s) = std::str::from_utf8(&available[..take]) {
                                name.push_str(s);
                            }
                            offset += take;
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
            }
        }
        if !name.is_empty() {
            println!("    -> 设备名称: {name}");
        }

        // fn2: GetDeviceType
        let cmd_type = [0x11, slot, idx_0005, (2 << 4) | 0x0b, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        if let Some(rep) = query(long_device, long_device, &cmd_type, Duration::from_millis(400)) {
            if rep.len() >= 5 {
                let dev_type = rep[4];
                println!("    -> 设备类型判定: 0x{dev_type:02X} ({})", decode_device_type(dev_type));
            }
        }
    } else {
        println!("  [Feature 0x0005] 未实现");
    }

    // 3. Battery Features: 0x1004, 0x1000, 0x1001
    println!("  -- 正在检查电量特性 --");
    if let Some(idx) = get_feature_index(0x1004) {
        println!("    -> 发现 Unified Battery (0x1004, index 0x{idx:02X})");
        let cmd = [0x11, slot, idx, (1 << 4) | 0x0b, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        if let Some(rep) = query(long_device, long_device, &cmd, Duration::from_millis(400)) {
            if rep.len() >= 6 {
                let soc = rep[4];
                let approx = rep[5];
                println!("       电量应答: SOC={soc}%, 粗略档={approx}");
            }
        }
    }
    if let Some(idx) = get_feature_index(0x1000) {
        println!("    -> 发现 Battery Level Status (0x1000, index 0x{idx:02X})");
        let cmd = [0x11, slot, idx, (0 << 4) | 0x0b, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        if let Some(rep) = query(long_device, long_device, &cmd, Duration::from_millis(400)) {
            if rep.len() >= 5 {
                let level = rep[4];
                println!("       电量状态: 级别={level}");
            }
        }
    }
    if let Some(idx) = get_feature_index(0x1001) {
        println!("    -> 发现 Battery Voltage (0x1001, index 0x{idx:02X})");
        let cmd = [0x11, slot, idx, (0 << 4) | 0x0b, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        if let Some(rep) = query(long_device, long_device, &cmd, Duration::from_millis(400)) {
            if rep.len() >= 7 {
                let mv = u16::from_be_bytes([rep[4], rep[5]]);
                let flags = rep[6];
                let charging = flags & 0x80 != 0;
                println!("       电压应答: {mv} mV, 充电标志={charging}");
            }
        }
    }

    // 4. DPI Features: 0x2201, 0x2202
    println!("  -- 正在检查 DPI 特性 --");
    if let Some(idx) = get_feature_index(0x2201) {
        println!("    -> 发现 Adjustable DPI (0x2201, index 0x{idx:02X})");
        // fn1: sensor count & DPI list
        let cmd_list = [0x11, slot, idx, (1 << 4) | 0x0b, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        if let Some(rep) = query(long_device, long_device, &cmd_list, Duration::from_millis(400)) {
            dump("       0x2201 fn1 应答:", &rep);
        }
        // fn2: read current DPI
        let cmd_read = [0x11, slot, idx, (2 << 4) | 0x0b, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        if let Some(rep) = query(long_device, long_device, &cmd_read, Duration::from_millis(400)) {
            if rep.len() >= 8 {
                let current_dpi = u16::from_be_bytes([rep[5], rep[6]]);
                let default_dpi = u16::from_be_bytes([rep[7], rep[8]]);
                println!("       当前 DPI: {current_dpi}, 默认 DPI: {default_dpi}");
            } else {
                dump("       0x2201 fn2 应答:", &rep);
            }
        }
    }
    if let Some(idx) = get_feature_index(0x2202) {
        println!("    -> 发现 Extended Adjustable DPI (0x2202, index 0x{idx:02X})");
        // fn1: capabilities
        let cmd_caps = [0x11, slot, idx, (1 << 4) | 0x0b, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        if let Some(rep) = query(long_device, long_device, &cmd_caps, Duration::from_millis(400)) {
            dump("       0x2202 fn1 应答:", &rep);
        }
        // fn5: read current DPI
        let cmd_read = [0x11, slot, idx, (5 << 4) | 0x0b, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        if let Some(rep) = query(long_device, long_device, &cmd_read, Duration::from_millis(400)) {
            dump("       0x2202 fn5 当前 DPI 应答:", &rep);
        }
    }
}

fn main() {
    let api = match HidApi::new() {
        Ok(api) => api,
        Err(err) => {
            eprintln!("初始化 hidapi 失败: {err}");
            return;
        }
    };

    let mut found = Vec::new();
    for info in api.device_list() {
        if info.vendor_id() == VID
            && ReceiverFamily::from_pid(info.product_id()).is_some()
            && info.usage_page() == 0xff00
            && matches!(info.usage(), 0x01 | 0x02)
        {
            found.push((info.product_id(), info.usage(), info.path().to_owned()));
        }
    }

    if found.is_empty() {
        println!("未发现罗技接收器（Lightspeed / Unifying / Bolt）");
        return;
    }

    let (pid, _, _) = found[0].clone();
    let family = ReceiverFamily::from_pid(pid).expect("已知接收器");
    let Some((_, _, short_path)) = found.iter().find(|(_, usage, _)| *usage == 0x01) else {
        println!("未找到 usage 0x01 短帧接口");
        return;
    };
    let Some((_, _, long_path)) = found.iter().find(|(_, usage, _)| *usage == 0x02) else {
        println!("未找到 usage 0x02 长帧接口");
        return;
    };

    println!("发现罗技接收器 PID {pid:04X} ({family:?})，vendor 接口数: {}", found.len());
    let short_device = match api.open_path(short_path) {
        Ok(dev) => dev,
        Err(err) => {
            eprintln!("打开短帧接口失败: {err}");
            return;
        }
    };
    let long_device = match api.open_path(long_path) {
        Ok(dev) => dev,
        Err(err) => {
            eprintln!("打开长帧接口失败: {err}");
            return;
        }
    };

    // 1. 读取接收器配对状态（短寄存器 0x81, register 0x02）
    let reg_req = [0x10, 0xff, 0x81, 0x02, 0x00, 0x00, 0x00];
    if let Some(reply) = query(&short_device, &short_device, &reg_req, Duration::from_millis(500)) {
        if reply.len() >= 5 {
            let paired_count = reply[4];
            println!("接收器配对设备数: {paired_count}");
        }
    } else {
        println!("读取配对数寄存器无应答");
    }

    // 2. 遍历各槽位
    let capacity = family.slot_capacity();
    println!("接收器理论槽位上限: {capacity}");
    for slot in 1..=capacity {
        probe_slot(&long_device, slot);
    }
}
