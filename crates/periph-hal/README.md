# periph-hal

独立的 Rust 外设访问库。设备发现与能力契约不依赖 Slint、应用配置、备注或历史记录。首期提供 Windows HID 和经典蓝牙系统属性后端，具体协议编解码复用原项目实现。

## 使用现有后端

Windows 应用启用 `windows-backend` feature；默认 features 为空，便于单独使用公共契约或实现其他平台后端。

```toml
[dependencies]
periph-hal = { path = "path/to/periph-hal", features = ["windows-backend"] }
```

```rust
use periph_hal::{Hardware, battery::BatteryState};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut hardware = Hardware::system()?;
    let snapshot = hardware.scan();
    for device in snapshot.devices {
        if let Some(BatteryState::Value(sample)) =
            device.capabilities.get::<BatteryState>("main")
        {
            println!("{}: {}%", device.descriptor.name, sample.level.get());
        }
    }
    for failure in snapshot.failures {
        eprintln!("{}: {}", failure.backend, failure.error);
    }
    Ok(())
}
```

`Hardware::system()` 在没有可用平台后端时返回 Unsupported，不将空列表伪装成枚举成功。非 Windows 应用可以实现 `Backend` 并通过 `Hardware::register` 使用相同接口。`Backend` 的对象和原生资源留在所属工作线程，不要求原生句柄跨线程。

## 扩展方式

| 扩展 | 改动 |
| --- | --- |
| 同一 HID 协议新增型号 | 私有驱动中的 matcher / 型号数据和对应测试 |
| 新 HID 协议 | `src/windows/vendor/` 的薄驱动、`src/protocol.rs` 的纯编解码、私有驱动注册 |
| 新访问方式或操作系统 | 实现 `Backend`，返回公共描述和能力；应用启动时注册 |
| 新领域能力 | 在独立模块或下游 crate 实现 `Capability`，提供强类型状态或客户端，注册到目标组件 |

`tests/portable.rs` 在库外定义了摄像头能力和后端。它无需增加核心枚举成员、导入电量模块或修改已有能力。设备角色是可扩展标签，不决定能力；无电池设备无需提供 BatteryState。

公共 `Device` 仅包含描述、连接、能力集合和结构化错误。`Capabilities` 内部用标准库 Any 隐藏类型，业务只使用 `insert::<T>` / `get::<T>`，不直接接触 Any、字符串命令或任意 JSON。每个组件、契约 ID 和版本只允许一个实现；同设备的多来源融合由应用选择策略。

能力值可以是本轮观测，也可以是领域定义的客户端。当前电量使用观测 `BatteryState`；未来命令、订阅、音视频或笔输入可使用各自类型的客户端和专用会话，核心不预建空接口，也不通过快照通道传递媒体帧。`Backend::scan` 目前是同步轮询接口，按注册顺序调用，不提供并行调度或强制中断同步系统调用。

## 事实与业务边界

- `DeviceKey` 是带来源作用域的访问路由，不能直接当成永久设备身份。身份凭据带命名空间和主体，接收器序列号不等于槽内外设 UID。
- HAL 的 HID 槽位路由始终包含槽号；当前应用在 `src/hardware.rs` 中兼容旧版单设备记录键。旧配置及备注合并仍由应用负责。
- `Connection` 和电量结果独立。超时、缺属性与不支持是不同状态；缓存电量不能证明设备在线。
- 百分比支持 0..=100；特定协议若用零表示未知，由该协议解析器处理。
- `Sample.observed_at` 是读取时间，`sampled_at` 缺失时不声称知道设备采样时间。Windows 蓝牙属性使用 SystemCache 来源。
- 一个后端失败不丢弃其他后端结果。`Snapshot.completed` 只列举成功完成的后端；应用不得把失败后端的旧记录当作已确认离线。
- 当前没有 Linux/macOS 驱动、GATT 后端、真实摄像头或音视频实现。可移植性指公共库和后端接口能在非 Windows 编译并被替换，不意味着已有跨平台硬件支持。

## 检查

```text
cargo test -p periph-hal --no-default-features --locked --all-targets
cargo test --workspace --locked --all-targets
cargo clippy --workspace --locked --all-targets -- -D warnings
```

`diagnostics` feature 仅供原始协议探针使用，公开独立的 Windows 专用辅助入口；应用正常运行不依赖它。协议构造器仍通过 `protocol` 共享，避免探针与真实驱动复制报文代码。
