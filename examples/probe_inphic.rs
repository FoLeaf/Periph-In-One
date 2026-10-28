//! Probe Inphic 2.4G mice over vendor collections.
//! Read-only inspection of input report 0x03 notifications (battery, DPI stage, etc.).
//!
//! Match rules (per docs/driver-analysis/inphic.md):
//! - VID 0x1D57
//!   - PID 0xFA60: usage_page 0x000A (notification) / 0x000B (feature)
//!   - PID 0xFA65: usage_page 0xFF00, usage 0x0001
//!   - Note: 045E:* is excluded from matchers.
//!
//! Report 0x03 notification structure:
//! [0] = 0x03
//! [1] = tag (0x95: IN9 PRO, 0x90: IN9 PURE, 0x93: IN10)
//! [2] = subcmd:
//!       0x40: battery / mode ([3]=mode, [4]=level 0..=100)
//!       0x10: DPI stage switch ([3]=stage 1..=8)
//!       0x20: report rate (one-hot)
//!       0x50: transfer end
//!       0x70: lighting mode
//!       0x80: profile switch (1..=5)

use hidapi::HidApi;
use std::time::{Duration, Instant};

const VID: u16 = 0x1d57;
const PID_FA60: u16 = 0xfa60;
const PID_FA65: u16 = 0xfa65;

fn model_name_from_tag(tag: u8) -> &'static str {
    match tag {
        0x95 => "IN9 PRO",
        0x90 => "IN9 PURE",
        0x93 => "IN10",
        _ => "未知英菲克型号",
    }
}

fn dump_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn main() {
    let api = match HidApi::new() {
        Ok(api) => api,
        Err(err) => {
            eprintln!("初始化 hidapi 失败: {err}");
            return;
        }
    };

    let mut endpoints = Vec::new();
    for info in api.device_list() {
        if info.vendor_id() != VID {
            continue;
        }
        let pid = info.product_id();
        let up = info.usage_page();
        let u = info.usage();

        let is_match = match pid {
            PID_FA60 => (up == 0x000a || up == 0x000b) && u == 0x0000,
            PID_FA65 => up == 0xff00 && u == 0x0001,
            _ => false,
        };

        if is_match {
            endpoints.push((pid, up, u, info.interface_number(), info.path().to_owned()));
        }
    }

    if endpoints.is_empty() {
        println!(
            "未检测到英菲克接收器或鼠标端点（1D57:FA60 usage_page 0x0A/0x0B 或 1D57:FA65 FF00:0001）"
        );
        return;
    }

    println!("发现 {} 个符合匹配规则的英菲克端点：", endpoints.len());
    for (idx, (pid, up, u, iface, _)) in endpoints.iter().enumerate() {
        let desc = match (*pid, *up, *u) {
            (PID_FA60, 0x000a, 0x0000) => "通知端点 (中断 IN / OSD handle)",
            (PID_FA60, 0x000b, 0x0000) => "Feature 端点 (handle)",
            (PID_FA65, 0xff00, 0x0001) => "第二端点 (FF00:0001)",
            _ => "匹配端点",
        };
        println!(
            "  [{idx}] PID: {pid:04X}, UsagePage: 0x{up:04X}, Usage: 0x{u:04X}, 接口号: {iface} ({desc})"
        );
    }

    // Open notification endpoints (usage_page 0x0A on FA60, or FF00:0001 on FA65)
    let notif_endpoint = endpoints.iter().find(|(pid, up, u, _, _)| {
        (*pid == PID_FA60 && *up == 0x000a && *u == 0x0000)
            || (*pid == PID_FA65 && *up == 0xff00 && *u == 0x0001)
    });

    let Some((pid, up, u, iface, path)) = notif_endpoint else {
        println!("未找到可用于监听通知的中断端点");
        return;
    };

    println!("\n正在打开通知端点: PID {pid:04X}, UP 0x{up:04X}, U 0x{u:04X}, 接口 {iface}");
    let device = match api.open_path(path) {
        Ok(dev) => dev,
        Err(err) => {
            eprintln!("打开端点失败: {err}");
            return;
        }
    };

    println!("正在监听只读输入通知（持续 5 秒，零写入）...");
    let mut buf = [0u8; 128];
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut received_any = false;

    while Instant::now() < deadline {
        match device.read_timeout(&mut buf, 100) {
            Ok(n) if n > 0 => {
                received_any = true;
                let frame = &buf[..n];
                println!("收到报文 (长度 {n}): {}", dump_hex(frame));

                // Verify report id 0x03
                if frame[0] == 0x03 && frame.len() >= 5 {
                    let tag = frame[1];
                    let subcmd = frame[2];
                    let model = model_name_from_tag(tag);

                    match subcmd {
                        0x40 => {
                            let mode = frame[3];
                            let level = frame[4];
                            let mode_desc = match mode {
                                0x02 => "无线正常使用",
                                0x03 => "有线供电/充电",
                                _other => "其他状态码",
                            };
                            let level_str = if level <= 100 {
                                format!("{level}%")
                            } else {
                                format!("{level} (超限无效，丢弃)")
                            };
                            println!(
                                "  -> 电量通知 [{model} (tag 0x{tag:02X})]: 模式 0x{mode:02X} ({mode_desc}), 电量: {level_str}"
                            );
                        }
                        0x10 => {
                            let stage = frame[3];
                            println!(
                                "  -> DPI 切换通知 [{model} (tag 0x{tag:02X})]: 当前档位 {stage} (1..=8)"
                            );
                        }
                        0x20 => {
                            let rate = frame[3];
                            println!(
                                "  -> 回报率通知 [{model} (tag 0x{tag:02X})]: 原始值 0x{rate:02X}"
                            );
                        }
                        0x50 => {
                            println!("  -> 传输结束标志 [{model} (tag 0x{tag:02X})]");
                        }
                        0x70 => {
                            let mode = frame[3] >> 4;
                            println!("  -> 灯效模式通知 [{model} (tag 0x{tag:02X})]: 模式 {mode}");
                        }
                        0x80 => {
                            let profile = frame[3];
                            println!(
                                "  -> 配置档切换通知 [{model} (tag 0x{tag:02X})]: 配置档 {profile} (1..=5)"
                            );
                        }
                        other => {
                            println!("  -> 其他子命令 0x{other:02X} [{model} (tag 0x{tag:02X})]");
                        }
                    }
                }
            }
            Ok(_) => {}
            Err(err) => {
                eprintln!("读取端点出错: {err}");
                break;
            }
        }
    }

    if !received_any {
        println!("未收到来自鼠标的主动通知（可能未开机、休眠或当前处于静默状态）");
    }
}
