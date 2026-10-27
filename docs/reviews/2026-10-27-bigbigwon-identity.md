# 墨将烈风休眠/工作模式绑定审查

## 实施记录（2026-10-27）

下文审查描述的是 `9112a09` 基线。P1/P2 已按本报告方案修正：

- 身份层移除墨将 VID/PID 家族合并和展示特例，复用通用 UUID、位置及路由证据；弱观察遇到强身份互相冲突的历史候选时使用独立临时记录，不输出跨手柄历史 remap。身份修正提交：`0e38811`。
- 无 UUID 的不同端口分别保留；相同 UUID 仍能关联有线、无线及换端口。未知连接状态保留原有有效名称；当前 PID 来自本次观察。
- `2106` 的发现返回 Unknown，读取立即返回 Unavailable；不声明充电或连接，不发送额外查询。工作态 UUID 和电量查询沿用原实现。
- 正式回归覆盖六种枚举顺序、多休眠端点、历史路由归属歧义及再次扫描/配置恢复、强证据后续确认合并、备注、电量历史和休眠/唤醒时间戳。

验证：`cargo test -p periph-in-one -p periph-hal --features periph-hal/windows-backend,periph-hal/serde --lib --tests` 全部通过；依赖真机的测试默认忽略，并已在本机 `413D:2106` 上单独运行通过。其输出为 `connection=Unknown`、`battery=None`、`uid=""`、`Unavailable("电量暂不可用")`，发现和读取分支合计约 22 微秒。`cargo run --quiet -- --once` 的实际输出为 `BIGBIG WON 手柄: — (未响应)`。相关 Rust 文件的格式检查和差异检查通过。

F 盘构建时曾因磁盘空间不足中断；将可复用构建产物复制到 C 盘临时 target，并设置 `CARGO_INCREMENTAL=0` 后完成上述完整回归。没有删除 F 盘现有文件、改写正式配置或自动拆分历史。真实手柄从休眠到工作再到休眠的人工操作循环尚未执行；对应应用状态转换由合成回归覆盖。

已经被旧逻辑合并并去重丢失的历史，不能靠此次代码修正自动恢复。不同端口且无共同身份证据的双连接仍可能暂时出现两条记录，需要共同 UUID、可靠配对证据或用户显式绑定才能确认关联。

## 原始审查

审查日期：2026-10-27。基线：`9112a09`，应用版本 `0.23.3`；主要变更来源：`7ec2ef2`。审查过程中工作区出现了其他 ATK/KZZI/DPI 改动，未纳入本审查或提交。本次不修改生产逻辑。

## 结论

当前绑定逻辑不满足项目的身份隔离要求。UUID 查询、协议校验和电量历史分层可以复用，但后来加入的“同厂商人格即同设备”旁路不可靠，会导致记录和电量历史串绑。建议做局部修正，不需要重写 HAL 或引入新的手柄框架。

## P1：无 UUID 的人格可以合并不同的实体手柄

位置：`src/model.rs:154`、`:776`、`:788`、`:851`。

`personality_match` 只检查双方具有 `input.gamepad` 角色，以及 `413D:{2104,2106,2114}` 中的任一产品 ID，未要求共享 UUID、序列号、位置或路由。这个函数甚至会匹配两份相同 PID 的描述，并不要求真的发生模式切换。

`contradictory` 只比较“旧记录与本次观察”，因此缺少 UUID 的 `2106` 同时兼容 UUID 为 A、B 的两只手柄。它们都进入 candidates 后，合并循环仅保护冲突备注，未检查候选记录彼此的强身份冲突，直接输出 B → A 的 confirmed remap。

最严重的链路是：误合并 → `Batteries::apply` 将 B 的电量重绑到 A → 按 `(device_id, component)` 去重 → 两只手柄只保留一份电量历史。现有归并还会丢失被吸收记录的 UUID，后续仅凭这份配置无法可靠自动拆分恢复。

已用现有 `tests/support::gamepad` 和 `snapshot` 复现：

| 输入 | 应满足的约束 | 当前结果 |
| --- | --- | --- |
| 两个不同端口的无 UUID `2106` | 无同一设备证据，应保留两条记录 | 仅一条 |
| A/2104、B/2104、无 UUID 的 C/2106 | A 与 B 不得合并 | 仅一条，输出 B → A |
| 上述输入调换枚举顺序 | 记录归并结果应一致 | 正序一条，逆序两条 |
| A 离开后另一端口出现无 UUID `2106` | 不应凭型号继承 A 的备注 | 直接继承旧记录和备注 |
| 已保存 A、B 两份电量后仅观察无 UUID `2106` | A、B 历史应分别保留 | 历史从两份变一份 |

这违反 `docs/HARDWARE-ARCHITECTURE.md` 第 3 节的“不按相同名称或型号合并”和“强身份矛盾时不能接管旧记录”。

## P2：把 `2106` 直接解释成正在充电且已连接

位置：`crates/periph-hal/src/windows/vendor/bigbigwon.rs:38`、`:61`。

发现入口和读取入口都在 PID 为 `2106` 时直接返回 `Connection::Connected`；读取还返回“手柄正在底座充电”。这两个结论都没有协议应答作为依据。

仓库 `docs/VALIDATION.md:119` 至 `:121` 记录的是未连接待机态，以及已知只读命令均不应答；充电状态字节的语义也明确未验证。当前 HID 节点存在只能证实接收端点仍在，不能证明手柄本体已连接，更不能证明充电。`presentation::status` 因此会输出“已连接”。充电错误消息留在 HAL 的 BatteryState 中，界面目前主要显示通用电量不可用，不能说充电字样一定直接显示给用户。

本机审查时 `Get-PnpDevice -PresentOnly` 可见 `413D:2106`。进一步通过 `Hardware::discover` 及该设备的 `Battery::read` 做只读观察，结果为 `name="BIGBIG WON 手柄"`、`connection=Connected`，身份只有 `windows.usb-port/Location`，没有 UUID；读取返回 `Unavailable("手柄正在底座充电")` 和 `facts.connection=Some(Connected)`。这确认了代码的实际输出；没有现场操作手柄进行休眠、唤醒或充电切换，也不能据此反推真实充电状态。

跳过已验证不应答人格的反复电量超时可以保留。修正应将该分支表达为端点存在、本体连接未知、电量暂不可用，沿用现有 `Unknown`、`Unavailable` 和历史电量机制。除非将来取得明确断连/充电证据，不直接声明 Disconnected 或 charging。

## 代码结构评估

目前尚不需要整体重构。协议编解码在 `protocol.rs`，Windows I/O 在 vendor adapter，记录身份在 `model.rs`，电量历史在 `battery.rs`，这些边界可继续使用。

新增旁路已经形成明显的耦合和特例堆积：

- `model.rs` 文件声明不包含平台规则，实际硬编码 `413D`、三个 PID、游戏手柄角色以及 `2104` 的展示顺序。
- PID 列表同时存在于 vendor adapter 和应用身份层；新增模式时要同步修改两处。
- `personality_match` 同时服务于身份归并和名称/PID 展示保留，身份规则与展示策略互相牵制。
- 两项现有测试把“跨端口、没有 UUID 也合并”写成预期，验证了实现假设，没有验证实体身份不被混淆。
- `docs/ADAPTATION.md` 和 vendor 注释仍称“不锁 PID”，实际 matches 已加入三个 PID 的白名单，文档与代码需要同步。这个问题本身不意味着应盲目支持未知 PID。

## 建议采用的方案

1. 删除应用身份候选里的 `personality_match` 旁路，复用现有命名空间身份、可靠 UUID 和路由匹配。同一端口的休眠/工作切换本来即可由 Location/route 延续；相同 UUID 可以跨端口延续。弱证据必须保持来源和端点边界。
2. 无 UUID 的不同端口不自动输出 confirmed remap。即使当前只有一只已知手柄，也不能据此证明新端点属于它。双连接时若只有一个端点有 UUID，先保持独立；未来获得共同本体 UID、明确的配对证据或用户手动绑定后，再合并备注与历史。
3. 候选合并前检查候选彼此的强身份冲突，含冲突的候选组不能因一个弱观察成为已确认合并。已有身份与电量 remap 机制继续复用，不另写一套历史迁移框架。
4. PID/人格判断收敛到 BIGBIG WON adapter 内部，必要时用私有枚举表达 Active/Idle/Wired。向通用层只输出现有身份和连接事实；展示名称保留不再决定设备身份，不为一个厂商扩张整套通用模式系统。
5. 保留 `2106` 快速返回，撤回仅由 PID 推导 charging/Connected 的结论。醒来后重新查询 UUID 和电量，失败时保持不确定状态和最后一次有效电量的时间。
6. 将上述反例纳入正式回归，并修正现有跨端口无证据合并测试。对已经存在的配置先检查备份；不要根据 PID 批量拆分历史，也不要承诺恢复已经去重丢失的数据。

代价：无身份证据的双连接可能暂时出现两条记录。这是信息不足时的可解释状态；要在不猜测的前提下消除重复，需要额外配对证据或显式绑定入口。

## 现成实现评估

已先检索仓库，再检索开源手柄实现和 Rust 库。本次检索未发现能直接替换墨将 EF UUID、04 电量及跨人格实体绑定的现成 Rust 模块，因此优先复用当前 HAL，而非新增框架。

| 候选 | 可复用价值 | 本次建议 |
| --- | --- | --- |
| 当前 `hidapi` + Windows topology | 已具备 HID 访问、接口聚合和位置证据 | 继续使用，避免重写传输 |
| 当前通用身份、EndpointRoute、组件和电量 remap | 已具备大部分归并和历史基础 | 局部修正证据规则 |
| gilrs | 提供手柄输入、连接状态、power_info、映射等通用 API | 未找到该协议的替代证据；不因 API 名为 UUID 就认定等同 EF 的 8 字节本体 ID |
| SDL Xbox 360 HIDAPI 驱动 | 可参考兼容手柄输入处理 | 不据此推断休眠态墨将端点与另一设备的配对关系；本次不引入 SDL |
| Windows ContainerID | 适合聚合同一 USB 设备的功能接口 | 不直接当作跨 PID、跨接收器/有线连接的手柄本体 UUID |

微软说明，系统自动生成 USB ContainerID 时会使用 VID、PID、版本号和序列号。因此，不能假设切换 PID 后系统生成的 ContainerID 恒定；固件自行提供稳定 ContainerID 属于另一种情况。参见 [USB ContainerIDs in Windows](https://learn.microsoft.com/en-us/windows-hardware/drivers/usbcon/usb-containerids-in-windows)。

其他已核对的一手资料：[hidapi DeviceInfo](https://docs.rs/hidapi/latest/hidapi/struct.DeviceInfo.html)、[gilrs Gamepad](https://docs.rs/gilrs/latest/gilrs/struct.Gamepad.html)、[SDL Xbox 360 HIDAPI 实现](https://github.com/libsdl-org/SDL/blob/main/src/joystick/hidapi/SDL_hidapi_xbox360.c)。检索未找到不等于生态中绝对不存在。

## 验证记录

- `cargo test --test model --test core`：42 项通过（model 20、core 22）。
- 临时反例集 `cargo test --test _review_gamepad_identity -- --nocapture`：5 项均失败，分别对应前述五种场景；这是对现有缺陷的验证，未作为失败测试留在常规测试目录。
- 显式运行临时的只读真机观察测试：1 项通过，确认当前 `2106` 无 UUID 仍返回 Connected 及充电错误详情；未读写正式配置。
- 未修改正式配置、用户备注或电量历史；未改生产代码。未进行休眠→唤醒→充电的人工真机循环。
- Cargo 出现增量编译缓存硬链接/权限告警，但上述测试已完成运行，不影响反例断言结果。

最小复现：将以下内容临时保存到 `tests/_review_gamepad_identity.rs` 后运行 `cargo test --test _review_gamepad_identity`。当前版本两个断言均会失败：第一次把 A、B 合并，第二次将各自电量历史去重为一份。复现结束删除该临时文件。

```rust
mod support;
use periph_in_one::{battery::{Batteries, SavedBattery}, model};
use support::{gamepad, snapshot};

fn initial() -> model::MergeResult {
    model::merge_snapshot(&[], &snapshot(vec![
        gamepad("port-a", 0x2104, "423bca92863cba8d"),
        gamepad("port-b", 0x2104, "aabbccddeeff0011"),
    ], &["windows.hid"]))
}

#[test]
fn idle_observation_must_not_absorb_distinct_uids() {
    let first = initial();
    let next = model::merge_snapshot(&first.devices, &snapshot(vec![
        gamepad("port-c", 0x2106, ""),
    ], &["windows.hid"]));
    assert!(next.devices.len() >= 2);
}

#[test]
fn independent_battery_histories_must_survive() {
    let first = initial();
    let mut batteries = Batteries::restore(first.devices.iter().enumerate()
        .map(|(i, device)| SavedBattery {
            device_id: device.saved.id.clone(), component: "main".into(),
            level: 30 + i as u8, observed_at: 100 + i as u64,
        }).collect());
    let next = model::merge_snapshot(&first.devices, &snapshot(vec![
        gamepad("port-c", 0x2106, ""),
    ], &["windows.hid"]));
    batteries.apply(&[], &next, 200);
    assert_eq!(batteries.saved.len(), 2);
}
```
