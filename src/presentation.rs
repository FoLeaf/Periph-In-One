//! Application labels derived from generic facts, never used for identification.
use crate::{battery::BatteryView, model::Device};
use periph_hal::{Connection, DeviceDescriptor, Identity, IdentityKind};

pub fn control_error(error: &periph_hal::HardwareError) -> &'static str {
    use periph_hal::ErrorKind;
    match error.kind {
        ErrorKind::Timeout => "设备未响应，请重试",
        ErrorKind::InvalidResponse => "设置未生效，请重试",
        ErrorKind::Unsupported => "当前设备不支持此设置",
        ErrorKind::PermissionDenied => "无法访问设备，请关闭其他驱动后重试",
        ErrorKind::Conflict => "设备状态已变化，请刷新后重试",
        ErrorKind::Unavailable => "设备连接已更新，请重试",
        _ => "与设备通信失败，请重试",
    }
}

pub fn title(device: &Device) -> &str {
    if !device.saved.alias.is_empty() {
        &device.saved.alias
    } else if !device.saved.descriptor.name.is_empty() {
        &device.saved.descriptor.name
    } else {
        "外设"
    }
}

pub fn icon(descriptor: &DeviceDescriptor) -> &'static str {
    for (role, icon) in [
        ("input.mouse", "mouse"),
        ("input.keyboard", "keyboard"),
        ("input.gamepad", "gamepad"),
        ("audio.headset", "headset"),
    ] {
        if descriptor.roles.iter().any(|value| value == role) {
            return icon;
        }
    }
    "device"
}

pub fn category(descriptor: &DeviceDescriptor) -> String {
    let role = if descriptor.roles.iter().any(|role| role == "video.capture") {
        "摄像头"
    } else if descriptor.roles.iter().any(|role| role == "input.pen") {
        "数位板"
    } else {
        match icon(descriptor) {
            "mouse" => "鼠标",
            "keyboard" => "键盘",
            "gamepad" => "手柄",
            "headset" => "耳机",
            _ => "外设",
        }
    };
    let transport = descriptor
        .links
        .iter()
        .find_map(|link| match link.transport.as_str() {
            "usb" => Some("USB"),
            "bluetooth" | "bluetooth.classic" | "bluetooth.le" => Some("蓝牙"),
            _ => None,
        });
    transport.map_or_else(|| role.into(), |transport| format!("{role} · {transport}"))
}

pub fn identifiers(descriptor: &DeviceDescriptor) -> String {
    let mut usb_groups = Vec::<(u16, Vec<u16>)>::new();
    let mut other = Vec::new();
    for id in &descriptor.identifiers {
        if id.namespace == "usb" {
            if let Some((_, pids)) = usb_groups.iter_mut().find(|(vid, _)| *vid == id.vendor) {
                if !pids.contains(&id.product) {
                    pids.push(id.product);
                }
            } else {
                usb_groups.push((id.vendor, vec![id.product]));
            }
        } else {
            let entry = format!(
                "{}: 0x{:04X} / 0x{:04X}",
                id.namespace, id.vendor, id.product
            );
            if !other.contains(&entry) {
                other.push(entry);
            }
        }
    }
    let mut parts: Vec<String> = usb_groups
        .into_iter()
        .map(|(vid, pids)| {
            let pid_str = pids
                .iter()
                .map(|pid| format!("0x{pid:04X}"))
                .collect::<Vec<_>>()
                .join(" / ");
            format!("USB VID: 0x{vid:04X} · PID: {pid_str}")
        })
        .collect();
    parts.extend(other);
    parts.join(" · ")
}

pub fn serial(descriptor: &DeviceDescriptor) -> Option<&Identity> {
    descriptor
        .identities
        .iter()
        .find(|identity| identity.kind == IdentityKind::Serial && !identity.value.is_empty())
}

pub fn serial_label(descriptor: &DeviceDescriptor) -> String {
    serial(descriptor).map_or_else(
        || "序列号：未提供".into(),
        |identity| {
            let subject = if identity.subject == "receiver" {
                "接收器 SN"
            } else {
                "设备 SN"
            };
            format!("{subject}：{}", identity.value)
        },
    )
}

pub fn status(device: &Device) -> &'static str {
    match device.connection {
        Connection::Connected => "已连接",
        Connection::Disconnected => "已离线",
        Connection::Unknown if device.present => "未响应",
        Connection::Unknown => "状态未知",
    }
}

pub fn battery_message(battery: &BatteryView) -> &'static str {
    if battery.unsupported {
        "该设备不提供电量"
    } else if battery.available && !battery.current {
        "电量暂不可用"
    } else {
        ""
    }
}
