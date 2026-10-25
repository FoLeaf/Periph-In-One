//! Application labels derived from generic facts, never used for identification.
use crate::{battery::BatteryView, model::Device};
use periph_hal::{Connection, DeviceDescriptor, Identity, IdentityKind};

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
    descriptor
        .identifiers
        .iter()
        .map(|id| {
            if id.namespace == "usb" {
                format!("USB VID: 0x{:04X} · PID: 0x{:04X}", id.vendor, id.product)
            } else {
                format!(
                    "{}: 0x{:04X} / 0x{:04X}",
                    id.namespace, id.vendor, id.product
                )
            }
        })
        .collect::<Vec<_>>()
        .join(" · ")
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
