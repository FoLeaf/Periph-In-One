# periph-hal

独立的 Rust 外设访问库。设备核心只包含描述、带命名空间的身份证据、连接和类型化能力；不依赖 Slint、应用配置、备注或电量历史。已提供 Windows HID 和经典蓝牙系统属性后端，协议编解码复用原项目实现。

## 发现与能力读取

Windows 应用启用 `windows-backend`。默认 features 为空；需要序列化通用描述时启用可选 `serde`。

```toml
[dependencies]
periph-hal = { path = "path/to/periph-hal", features = ["windows-backend"] }
```

```rust
use periph_hal::{Connection, Hardware, battery::{Battery, BatteryState}};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut hardware = Hardware::system()?;
    let discovered = hardware.discover(); // 不采样电量
    for mut device in discovered.devices {
        let Some(battery) = device.capabilities.get::<Battery>("main") else {
            continue; // 设备不必具有电量能力
        };
        match battery.read() { // 调用方明确选择要读取的能力
            Ok(reading) => {
                reading.facts.apply_to(&mut device);
                if let BatteryState::Value(sample) = reading.state
                    && device.connection == Connection::Connected
                {
                    println!("{}: {}%", device.descriptor.name, sample.level.get());
                }
            }
            Err(error) => eprintln!("battery: {error}"),
        }
    }
    for failure in discovered.failures {
        eprintln!("{}: {}", failure.backend, failure.error);
    }
    Ok(())
}
```

`inventory()` 只被动枚举元数据，不采样、不改变发现代次，也不撤销现有能力客户端。`discover()` 允许识别身份和接收器拓扑，但不发送电量查询，也不读取蓝牙电量属性。它返回可调用的能力客户端。`Battery::read()` 每次执行一次独立读取，返回电量结果和本次操作建立的通用设备事实。电量失败本身不能推导设备离线。

没有平台后端时 `Hardware::system()` 返回 Unsupported；其他平台可实现 `Backend` 并注册，应用复用相同接口。后端工厂在专属工作线程创建后端和原生资源，因此不要求原生句柄 `Send`。每个后端独立调度；`DiscoveryStream::poll()` 按就绪顺序返回增量报告，较慢来源不会挡住已完成来源。一次刷新共用一个截止时间，默认 20 秒，可用 `set_operation_timeout` 调整。超时和新发现使用代次隔离，迟到结果不会写入新一轮快照。

同步 `discover()` 是等待流结束的便利 API。桌面刷新应使用 `discovery_stream()` 或可取消版本，在收到每个 `BackendDiscoveryUpdate` 后立即归并和展示结果。后端报告的 `coverage` 是本轮实际完成扫描的范围；`failures` 可以定位到 backend、endpoint、device、component 或 capability，并带操作类型。覆盖范围内成功扫描到的其他目标仍可正常更新离线状态，被 withheld 的范围维持 Unknown。不可安全中断的原生同步调用可能在超时后继续占用其工作线程；HAL 不会无限创建线程，调用方退出不等待它们 join。

## 能力生命周期

电量客户端的契约为 `power.battery` v2。创建客户端不采样，重复读取不需要重新发现。`close()`、所属后端下一次发现（包括发现失败）、Hardware 销毁，都会使旧客户端及其克隆失效；失效后的读取在执行 I/O 前返回 Unavailable。

`Battery` 串行处理同一客户端的操作；Windows HID 同一接收器的不同槽位还共享访问锁，避免交叉发送和抢应答。重新发现会等待已进入的同步读取完成；这不代表能够强制取消 Windows 同步调用。端点已消失或可验证的身份发生变化时拒绝读取；没有本体身份的协议无法保证识别两次发现之间发生的重新配对。

新领域的实时客户端应通过 `Capability::invalidate` 实现自己的关闭和失效语义，并可覆盖 `bind_executor` 将 typed operation 绑定到后端所属线程。`BackendExecutor` 仅负责有界排队和失效检查；领域通过自己的强类型请求与 reply channel 实现具体操作。不可变描述或状态可以使用默认实现。连续音视频、笔输入及长任务使用领域专用会话与有界通道，不通过设备发现快照传输大块数据。

## 扩展

| 扩展 | 改动 |
| --- | --- |
| 同一 HID 协议新增型号 | 私有驱动 matcher / 型号数据和测试 |
| 新 HID 协议 | 驱动、共享纯编解码、驱动注册；分别实现身份/拓扑发现和能力读取 |
| 新访问方式或操作系统 | 实现 `Backend::inventory` / `discover`，提供相应类型的能力客户端 |
| 新能力 | 下游模块实现 `Capability` 与可选 executor 绑定，通过 `ComponentKey` 注册；领域服务按强类型接口调用 |

`tests/portable.rs` 在库外定义摄像头能力，`tests/capabilities.rs` 用计数后端验证发现零采样、独立读取和客户端失效。无需扩充核心设备枚举或修改已有领域契约；这些测试不代表已经实现真实摄像头。

`Capabilities` 的内部类型擦除只使用标准库。调用方使用 `insert::<T>` / `get::<T>`，不操作 Any、字符串命令或任意 JSON；每个组件、契约 ID 和版本只允许一个实例。

## 身份、事实与业务边界

- `DeviceKey` 是带来源作用域的访问路由。永久记录 ID、备注与多来源合并由应用决定。
- `EndpointKey` 标识一组共享访问资源；覆盖和失败可以精确到端点，HID 后端显式提供端点，不解析路由字符串。
- `ComponentDescriptor` 描述逻辑设备内的稳定子组件，含来源协议 `ComponentKey`、可选 parent、roles 和协议支持的 `ComponentClaim`。列表位置和显示名称不能作为键或 claim。`CapabilityDescriptor` 通过 `ComponentKey` 指向组件。
- 应用 `SavedDevice` 保存通用 `SavedComponent` 与来源路由，归并输出路由到持久 component ID 的映射。跨来源只按唯一可靠 claim 关联；claim 缺失时只按相同来源路由复用；同一来源 claim 矛盾时保留分开的历史。传统 `legacy/main` 表示设备根电量组件，在同一设备记录的来源间共用精确 ID `main`，以保持现有配置和 UI 查询兼容。
- `Identity` 有命名空间及主体；`DeviceDescriptor.target` 限定共享接收器下的逻辑目标。接收器序列号不冒充本体 UID，USB VID 和蓝牙 company ID 不属于同一编号体系。
- 角色是可扩展标签，硬件标识可为空；没有电池的设备无需注册电量客户端。
- `BatteryReading.facts` 提供操作确认的连接、名称和身份，应用决定如何归并。设备核心不保存电量数据，电量历史由独立领域服务管理。
- 百分比支持 0..=100；特定协议用零表示未知时，由协议解析器处理。缺读数、不支持和读取错误独立表达。
- 观察时间与设备采样时间分别记录；系统缓存不冒充刚采样的数据。蓝牙读取使用 SystemCache。
- 发现结果区分已完成覆盖与结构化故障范围。应用不能用失败或未覆盖来源的缺席结果确认离线，能力失败也不会抹掉已确认连接或电量历史。

当前应用已经直接使用通用描述及身份证据。配置 v1 的旧格式知识仅位于 `src/migration.rs`；迁移至 v2 时保留记录 ID、备注和历史，首次保存备份原文件。应用不再通过旧 Reading 转换层或蓝牙路径前缀判定设备。

当前没有 Linux/macOS 驱动、GATT 后端或真实媒体实现。可移植性检查证明公共库及其接口可在非 Windows 编译并替换后端，不代表已有跨平台真机支持。超时只限制调用方等待；HAL 无法强行打断正在执行的 Windows 同步系统调用。

## 检查

```text
cargo test -p periph-hal --no-default-features --locked --all-targets
cargo test -p periph-hal --no-default-features --features serde --locked --all-targets
cargo test --workspace --locked --all-targets
cargo clippy --workspace --locked --all-targets -- -D warnings
```

`diagnostics` feature 仅供原始 Windows 协议探针使用，正常应用不依赖它；探针与驱动共享 `protocol` 构造器。
