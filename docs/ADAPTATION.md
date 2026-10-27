# 设备适配规范

本文规定独立硬件库与应用的边界，硬性红线见 [contribution.md](../contribution.md)，可复用接口及用法见 [periph-hal](../crates/periph-hal/README.md)。第 7 节保留现有协议的研究证据，历史叙述中的旧字段名不代表当前接口。

## 1. 公共契约与后端实现

| 层 | 位置 | 职责 |
| --- | --- | --- |
| 设备核心 | `crates/periph-hal/src/device.rs`、`hardware.rs`、`capability.rs` | 协议无关的描述、身份凭据、连接事实、类型化能力和后端注册；不含电量字段、UI 或配置 |
| 电量能力 | `crates/periph-hal/src/battery.rs` | Battery 客户端与独立读取；Value / Unsupported / Unavailable、来源及时间 |
| 协议家族 | `crates/periph-hal/src/protocol.rs` | 报文构造、校验和解析的纯函数，不执行 I/O |
| HID 驱动 | `crates/periph-hal/src/windows/vendor/` | 精确匹配、协议组合、设备角色和能力结果；不生成界面文案 |
| Windows 访问后端 | `crates/periph-hal/src/windows/` | `hid.rs` 发现与分发、`transport.rs` 原生 I/O、`topology.rs` 拓扑与元数据、`bluetooth.rs` 系统属性 |
| 应用刷新 | `src/hardware.rs`、`src/devices.rs` | 通过可取消 `RefreshSession` 增量 poll 各 backend 与已投递能力；不再转换旧 Reading |
| 应用业务 | `src/model.rs`、`src/battery.rs`、`src/config.rs` | 通用身份与备注、独立电量历史、配置保存 |
| 配置迁移 | `src/migration.rs` | 读取 v1 旧字段并归一化既有 v2 endpoint/component，保留 ID、备注及历史 |

新增同能力协议不修改公共核心或应用规则；新增领域在独立模块实现 `Capability`，不向公共设备结构增加功能字段。能力值可以是观测或领域客户端；当前真实落地的是电量，连续数据和写能力待实际需求实现自己的接口与会话。

规则：

- 优先复用现有 hidapi、Windows API 和协议函数。新后端实现 `Backend` 并注册，不能把所有协议强制包装成同一种字节流。
- HID 驱动只组合共享协议函数，probe 从库导入同一构造器。原始 Windows 探针辅助接口通过开发用 `diagnostics` feature 提供，应用正常运行不得依赖它。
- 错误使用 `HardwareError` / `ErrorKind`。诊断详情与 UI 文案分离，禁止通过错误字符串包含某个中文词判断类别。
- 库不访问 Slint、应用配置、用户备注或历史；不能新增这些依赖来解决驱动差异。

## 2. 公模（OEM）规则

1. 同协议家族共用纯函数；各品牌 matcher 和型号数据保持证据边界，不因共享 VID 扩大识别范围。
2. 同品牌不同协议分别实现；原始句柄与会话调度放在访问后端，避免领域业务争抢接口。
3. 身份凭据包含命名空间和主体。接收器序列号不冒充外设 UID；USB VID 与 Bluetooth company ID 不属于同一编号体系。

## 3. 匹配与收录

- HID matcher 锚定 VID 加 Usage Page / Usage；共享 VID 不得只按 VID 匹配。PID 放开须有跨固件人格的接口证据。
- 系统属性后端按总线、节点类型和属性语义匹配，不要求具有 HID Usage Page。未来 GATT 后端按实际服务/特征及设备证据匹配。
- 区分已识别、已实现、已实测。静态分析结论不能升级为真机兼容承诺。
- 当前电量产品的无线 matcher 可凭可核验静态证据进入（标未实测）；仅有线本体或无相关能力的条目不因商品名进入现有电量 matcher，先记录证据。未来新领域按自己的能力和产品需求决定收录，不沿用键鼠电量筛选器。
- 角色标签用于分类与展示；能力由强类型契约决定，不要求每台设备具有电池。

## 4. 能力与状态

能力按 `ComponentKey`、契约 ID 和版本注册；应用用 `get::<T>` 或 `get_component::<T>` 访问。旧字符串 `insert/get` 是 `legacy` 命名空间的兼容入口。新增能力在下游模块实现 `Capability`，可通过 `bind_executor` 提供 backend 所属线程上的 typed 操作；核心不依赖具体领域能力或建立字符串 RPC。

发现只提供 `Battery` 客户端，不采样电量；调用 `Battery::read()` 才执行读取。读取结果带独立的设备事实。新实时客户端须实现 invalidate；电量客户端在重新发现、关闭和发现失败后撤销，失效的克隆不能执行 I/O。queued operation 在 backend worker 进入 I/O 前检查当前 generation；后端和原生句柄固定在专属 worker 上创建与使用，不要求 `Send`。

- `BatteryState::Value`：0..=100 的有效观测，带 DeviceResponse / SystemCache 来源及观察时间。
- `Unsupported`：型号或能力查询明确证明不支持。超时、未知特征和缺属性不能转换成它。
- `Unavailable`：本轮读取失败或暂不可用，保留结构化原因。
- `Connection` 独立于能力结果。接收器存在不证明外设在线，缓存电量也不证明在线；缺少连接证据使用 Unknown。

电量缺失不能清零，也不能使历史值变成新读数。系统未提供采样时间时 `sampled_at` 为 None；重复读取系统属性不能声称设备刚上报。UI 文案和历史样式在应用生成。

## 5. 适配流程

1. **研究**：先查现有协议、依赖和开源实现。需要协议探针时写 `examples/probe_<vendor>.rs`，结论注明来源、许可及未验证边界；报文函数不复制。
2. **实现**：已有 HID 访问方式新增私有驱动及协议纯函数，注册 `ADAPTERS`；新访问方式实现 Backend 的 inventory/discover。身份与拓扑查询留在发现，能力操作由独立客户端执行；平台句柄留在后端内部。
3. **能力**：返回公共设备描述和实际能力。电量、连接、错误分别表达；没有电池的设备不创建伪电量。
4. **身份**：协议能报告本体 UID 则提供对应凭据；路由与身份分开。`DeviceDescriptor.endpoint` 显式描述共享访问端点；`ComponentDescriptor{key,parent,roles,claims}` 显式描述子组件，key 来自协议稳定标识，claim 来自可靠身份证据，不能由名称或数组顺序猜测。`CapabilityDescriptor.component` 必须引用对应组件 key。HID 多槽位的 HAL 路由始终含槽号，应用旧记录兼容不得泄漏进 HAL。
5. **测试**：协议测试覆盖正常、截断、坏帧、校验、电量边界，确认零的实际语义。HAL 扩展测试放 `crates/periph-hal/tests/`；应用身份兼容测试放 `tests/`。
6. **文档**：更新 README 兼容表和实际验证记录；不把伪后端测试写成真实硬件支持。原始安装包、设备序列号和整份抓包不入库。
7. **检查**：执行 fmt、workspace all-targets 测试和 Clippy；独立库同时检查关闭默认 features 的可移植构建。相关硬件变更运行 `--once`、`--list-devices`，记录环境及未测部分。

## 6. 身份与故障范围

HAL 提供带命名空间、主体和目标作用域的身份证据；应用优先使用本体 UID、地址和可靠序列号，其次位置与来源内路由。强身份矛盾不能通过同路径兜底覆盖，重复序列号、冲突备注和手动恢复继续受到保护。

应用保存通用设备描述与 `SavedComponent`。组件 ID 独立于设备 ID 和来源 component key；归并通过 component route bindings 与确认 remap 返回给领域服务，`SavedBattery.component` 只保存 opaque persistent component ID。跨来源仅唯一可靠 claim 可以关联，claim 缺失时保留来源路由边界；同路由出现矛盾强 claim 时保留旧历史。传统 `legacy/main` 是设备根电量组件，同一设备的所有来源共用精确 ID `main`，兼容已有历史和 UI 查询。耳机左右耳与盒子须用协议稳定 key/claim，不能按扫描次序推断。

v1 配置在 `src/migration.rs` 中迁至 v2；读取 v2 时补齐 endpoint 与隐式 `main` 组件路由，保留旧记录 ID、备注与历史，首次保存前备份原始字节至 settings.json.v1.bak。旧单接收器键或平台位置只在迁移边界解释。

`DiscoveryReport` 与 `InventoryReport` 分开携带结果、覆盖范围和 scoped failures，范围可指向 backend、endpoint、device、component 或 capability，并标记 discovery/inventory/capability 操作。Windows HID 按 ContainerId 分组接收器接口，并将 endpoint 显式写入 descriptor；endpoint 失败 withheld 该接收器旧槽，其他成功范围可正常更新离线。Battery Err/Unavailable 只属于 capability 范围，不否定连接。

`Hardware::discovery_stream()` 为 backend 独立调度；应用 `RefreshSession` 按就绪顺序 poll 所有发现与能力响应，快速来源可先进入 UI 状态。默认整轮 deadline 为 20 秒。取消唤醒应用等待；超时后无法安全中断的原生同步调用可继续留在固定 worker 执行，UI 退出不 join worker，迟到结果按 generation 丢弃且迟到能力客户端撤销。

## 7. 范例

三节均已随复查落位（2026-10-25）：7.1 五条 PASS 加一条表格污染清洗后采用，7.2 五条 PASS 一条修正后采用，7.3 七条全 PASS。7.4/7.5/7.6 为第二批，随罗技接收器家族扩展、狼蛛驱动分析与蓝牙音频路径的复查写入。

### 7.1 ATK HUB（跨家族品牌）

来源：ATK HUB v3.2.30 Electron 应用两个 asar 包静态分析（app.asar 5956965c…347a、main/app.asar 1e4b689f…b56b），复查五条 PASS、设备表一条确认污染并清洗后采用。

**家族版图**：ATK 自有 VID 0x373B 按 Usage Page 分家族——FF05:0001 鼠标/接收器线（89 个 PID；同页还有显示器、数位板、掌机、眼镜、音箱、摄像头、坞站、麦克风外设线，明确不收录）、FF04:0002 V2 鼠标线（179 个 PID）、FF60/FF10 BYKB 键盘、keyLinker（413D 的 AXEPro 同走此通道）、FFB2 Nearlink、0x0C 耳机 MB 协议等。另有 CompX VID 0x3554 的 Dragonfly 线（FF04:0002，12 个 PID）：与 KZZI 的 0x13 接收器协议同 VID 不同家族——共享 VID 不等于共享协议，识别必须锚定 Usage Page 与帧格式，这是第 2 节公模规则的活例子。

**FF05 鼠标帧**（1216 已实测）：63 字节输出报告、reportId 8；[0] = sum(bytes[1..62]) & 0xFF、[1] = cmdCode 0x72、[5] = commandId（应答匹配键）；GetBatteryLevel=7 应答 data[0] 为电量，GetBatteryChargingStatus=15 应答 1 为充电中。与主应用 atk_sub_request / atk_battery 逐字段一致。

**FF04 V2 鼠标帧**（已实现未实测）：16 字节帧走 feature report（reportId 8），[0] = commandId、[1] = commandStatus、[2..3] = EEPROM 地址大端、[4] = dataValidLen、[5..14] = 数据、[15] = 校验 = 0x55 - ((0x08 + sum(bytes[0..14])) & 0xFF)；GetBatteryLevel=4，应答 data[0] = 电量、data[1] = 充电状态、data[2..3] = 电压大端。实现在 protocol.rs 的 atk_ff04_* 与 vendor/atk_v2.rs。0x3554 的 FF02 早期批次段（0xF31F、0xF400-0xF410）同协议只是推断，无接口证据，不收录。

**每台身份（记录档，未实现）**：BYKB 键盘 UUID 命令 encode(130,1,0×6)，应答 8 字节按大端折叠为 48 位（0x12/0x14 前缀为三模机型）；FF04 GetMouseCIDMID=16（CID/MID 加 uniqueId，设备表以预期 CID,MID 做型号一致性校验）；FF05 MouseCidMid=74 与 GetChipId=71；keyLinker 设备探针（应答字节 19/20 拼 0x2025）。

**写命令（记录档，LIGHTING/CONFIGURATION 预留位）**：FF05 SetLEDLightEffect=20、接收器 133/134；FF04 Set4KRGBMode=20、SetDongleLightMode=24、EEPROM 档位颜色地址 44..77（成对 CRC 槽）；BYKB 区域灯色 encode(4, 区, [参数,R,G,B]) 与矩阵 get/write_custom_led_matrix=10/11；耳机 EQ / 麦克风 / 环绕域。

**复查教训（表污染）**：原报告 FF05 鼠标/接收器表（122 行）混入 31 个外设 PID 与 1 个 OTA 裸 dataFilter 对象——成表脚本只排除了 monitor/tablet/handheld 三类标记。清洗改以对象自带的专属 ID 字段（monitorID / tabletID / handheldID / aiGlassesID / speakerID / webcamID / dockID / microphoneID）与 meta.deviceType 为准，而非仅 firmwareMark。这正是第 3 节档位纪律的价值：进 matcher 前逐条核对，清洗后 89 个 PID 全部有独立提取证据。

**FF02 V2 接收器（PR #3，作者真机验证于 2026-10-25）**：ATK V HUB 2.3.47 设备表中带 receiver 标记的 CompX 接收器使用 `3554` + `FF02:0002`，PID 为 `F201/F503/F505/F50B/F528/F58A/F58E/F590/FB12`。F58E 的 FF04 集合拒绝写入，实际走 17 字节 output/input 报告；请求复用 `atk_ff04_request`，应答校验 report ID、命令回显、状态、有效长度和整帧和 `0x55`。作者实测 VXE R1 SE+ 1K Dongle 电量 60%、充电 0、电压 `0F51`，其余 8 个 PID 已实现未实测；未扩大到前述无接口证据的早期批次。

当前驱动位于 `crates/periph-hal/src/windows/vendor/atk_v2_dongle.rs`。发现只查询在线状态（命令 3）及 RF 身份；电量（命令 4）只在显式能力读取时采样。HID 集合按 FF05 → FF02 → 其余排序。接收器报告无鼠标时使用 Disconnected / NoPairedDevice，不转换为 Unsupported；此时不把保留的旧 RF ID 当作在线外设身份。CID/MID 是型号信息，不作为本体 UID。

### 7.2 KZZI 258A 家族（Beiying OEM 双传输线）

来源：KZZI RGB Keyboard 驱动 DeviceDriver.exe 静态分析（SHA-256 2a35647a…77079），复查六条载荷性结论五条 PASS、一条修正后采用。

**家族关系**：258A 与 3554 是同一 OEM（Beiying）同一驱动引擎的两条传输线。键盘本体 USB 直连时以 258A:<PID> 枚举，走 0x208 字节 feature report 有线协议：reportId 0x09，命令族 0x82（AccessData 单帧）/ 0x84 / 0x88（分页写）/ 0x87（分页读，GetPower 用它读 2 字节），大块数据按 65 字节 0x0A 分块帧承载（62 字节负载，逐包 0xFF-求和校验）。2.4G/蓝牙接收器以 3554:FA09 或伪装 Apple VID 的 05AC:024F 枚举，走 20 字节 reportId 0x13 协议——即主应用已支持的家族。同一型号两种形态：K75RGB Lite 有线是 258A:01B0，经接收器是 3554:FA09。

**识别锚点**：接收器 = FF02/0002 + 20 字节 Input/Output 报表 + 报表 ID 0x13（厂商五重 caps 校验），通用 VID 0x258A 绝不能只按 VID 匹配；有线 = VID/PID（Cfg.ini 表内）+ FF00/01 或 FF02/01 或 FF02/02 + feature 报表长度 0x208，报表 ID 从 caps 派生。型号身份 = 新代档案的 Psd 六字节键（GetPSD cmd 0x05 应答与 KB.ini Psd= 同源），与主应用 `kzzi_model_name` 表交叉验证一致（注意 KB.ini 为十六进制文本、表内为十进制）。01C8/01DA/024D 三个档案缺 Psd，为异常档案。

**电量（复查修正后口径）**：接收器路径的 0x4A 命令在本驱动中有包装函数（0x40E560：发 20 字节帧、回读两字节，与 `kzzi_battery` 的 (level, state) 形态一致；静态可达性未证，虚调用槽 +0xA0 的 8 处 UI 区调用点未能绑定设备对象）。主应用的 0x4A 实现来自真机探测，与此静态锚点互相印证。有线路径的电量（0x87 双页读）未实测。原报告的本驱动无 0x4A、电量仅有线路径两条结论被复查推翻或降档。

**在线/未响应口径（真机）**：接收器应答 0x05 身份即视为在线。0x4A 无应答有两种含义——固件不提供电量的型号（K75 RGB key，Psd `[2,0,0,0,0,55]`，真机验证）经 `protocol::kzzi_battery_private` 标记为主应用显示已连接 · 该型号不提供电量；其余型号无应答仍按未响应处理（可能键盘关机），待真机逐一验证后再加入标记。

**收录决策**：56 个 258A 型号全部是有线本体 PID，接收器形态已被 3554:FA09 适配覆盖；按第 3 节规则有线型号不进 matcher，全表进附录 A，接收器识别的最终锚点沿用 caps 事实而非 VID。

**MCHOSE 公模扩展（PR #3）**：MCHOSE HUB 的 `YProduct.xml` 把 G98/X75/GX87/K99 V2/GX87 V2/KX75/X75 V2/G75 等指向 `3554:FA09` 的 0x13 协议，`Productpassword` 对应 Psd；驱动日志记录 G75 的 Psd `030000000115`。共享表现名为 `compx_model_name`，原 `kzzi_model_name` 保留兼容入口，KZZI/AULA 条目不变。G75 由 PR 作者实测电量 100%，其余新增型号已实现未实测；PSD 只决定型号，不充当设备唯一身份。

### 7.3 BIGBIG WON（Zikway FF7A 家族）

来源：BigBigWonAssistant 1.0.7.7 的 DevMgr.dll 静态分析（SHA-256 41cf4c1e…8c436）。

**身份模型**：全部型号共享同一组 VID/PID——413D×{2104 连接态, 2106 底座态, 2114}，另有 Switch Pro（057E:2009）与 DS4/DS3（054C:05C5、054C:0DF2）模拟人格；型号不按 PID 区分，按 USB 产品字符串区分（ZJ-GALE→GALE、C4_SERVER→Gale2、ZJ-C1ZJ→Rainbow3、ZJ-MJC6→Aether、ZJ-BLACK_→Mage、ZJ-C2SL→BLITZ2 等 12 类映射）。厂商客户端自身不过滤 FF7A（DevMgr.dll 全文零命中，FF7A/usage1 是固件描述符的属性），主应用按 VID 413D + FF7A/usage1 匹配且不锁 PID，比厂商更严且与实测一致。

**电量**：唯一电量操作码 04，应答处理器要求长度字节恰好 6（A5 06 04 电量 状态 校验和）；`protocol.rs` 的 `bigbigwon_battery` 已按此对齐（长度不为 6 即拒绝）。

**身份链第一级**：GetUUID（EF）请求 A5 0C EF 加 8 字节零选择器，应答帧 [3..10] 为 8 字节 UUID；静态语义与现有实现逐字段一致。

**命令空间（记录档，未实现）**：短查询约 28 个操作码（04/0B/D3/D4/D5/D6/D9/DA/DB/DC/DD/E1/E2/F5/F6/F7/FA/70/72/73/F8 等），设置类约 25 个（02 SetLogoLED、05 Switch2UDisk、0E Save2Device、1A Reset、72 亮度、73 灯效模式、D2/D4/DA/DC/E1/F6/F7/FB/FD 等），A4 长帧配置族按 240/280/335/454/484/508 字节分 7 个版本；查询用 len=04、设置用 len=05 且多一个数据字节是普遍规律。新增的 GetWirelessFlag（FA，请求 A5 04 FA A3，应答 A5 05 FA flag 校验和）已加入 examples/probe_bigbigwon.rs，待连接态真机验证。

**灯效（LIGHTING 预留位）**：F5 GetLogoLED、02 SetLogoLED（A5 0D 02 加 10 参数字节）、72 亮度读写、73 灯效模式读写、70 SetLEDShow 提交、DD 查询与 51 设置充电灯效；SetLED 有 16 字节定长与变长（头部 12 字节加 N×3 RGB 三元组）两个重载，长度字节随区数递增。JSON 字段到参数的映射在 WndMgr.dll，未展开。

**复查锚点**：帧常量 0xad0404a5（04）、0xa3fa04a5（FA）、0x7cd304a5（D3）、0x197004a5（70）均为厂商预计算常量，与 8 位环绕累加互证；应答分发三层结构（入口 0x10024113、索引表 0x10024EB8、跳转表 0x10024E64）。

### 7.4 罗技 Unifying 家族接收器（HID++）

来源：Logitech 公开的 HID++ 1.0/2.0 协议文档；协议语义与 Solaar（pwr-Solaar/Solaar，GPL-2.0）交叉核对；放电曲线与 OpenLogi 文档互证。C539 + G502 LIGHTSPEED 真机实测，其余接收器 PID 取自 Solaar `base_usb.KNOWN_RECEIVERS`（Unifying/Bolt/Lightspeed 同为 USB interface 2 的 FF00 双集合布局），标已实现未实测。传输层怪癖以 `examples/probe_logitech.rs` 的真机探针为准（2026-10-25，G502 + C539）。

**家族版图**：接收器三类人格——LIGHTSPEED `C539/C53A/C53D/C53F/C541/C545/C547/C54D`、Unifying `C52B/C532`、Bolt `C548`，全部以 Usage Page FF00 的两个集合承载两种报表宽度：0x01 集合收发 7 字节短帧（寄存器 0x81/0x80 与到达通知），0x02 集合收发 20 字节长帧（设备隧道）。真机确认 Windows 把两个集合暴露为两个 hidapi 设备，短帧写长集合与长帧写短集合都会被系统直接拒绝，因此寄存器走 0x01、设备帧走 0x02。

**扫描路径**：连接数寄存器（短帧 0x81 0x02）应答的第二字节是**已配对**槽数——鼠标休眠时仍报 1（真机验证），逐槽长帧 Root.Ping 探活后对在线槽读 Unified Battery (0x1004) → Battery Level Status (0x1000) → Battery Voltage (0x1001)，与 Solaar 的优先顺序一致。G502 只带电压特性（毫伏 + 充电标志位），按 Solaar 同款单节锂电放电曲线插值换算百分比；全流程零寄存器写入。

**多设备（一拖多）**：`VendorAdapter::read_all` 允许一次探测返回多条结果，Unifying 一拖六的每个在线槽位产出独立 `Outcome`，扫描层以 `物理口#槽位` 区分 physical_id（单设备接收器保持原物理口，避免历史身份漂移）。罗技的逐设备身份寄存器（0xB5 长读，Unifying 槽位 0x2N/0x3N、Bolt 0x5N，应答含类别 nibble 与整机序列号）**未纳入扫描**：C539 上配对子寄存器永不应答，接收器信息子寄存器应答延迟达秒级且跨轮乱序（均为真机观察），且长寄存器读的 sub_id 是 0x83（`0x8100 | 0x2B5` 的 0x200 位进位，0x82 是长写——初版实现误用 0x82 上了硬件，幸而 C539 对该寄存器无写入效果；此教训入 7.7）。帧格式记录于探针，待 Unifying/Bolt 真机再评估接入。

**当前 HAL 行为**：上述为原实现记录。当前 discover 按接收器容量扫槽（LIGHTSPEED 1 槽，其余 6 槽），不把已配对数量当作连续槽号，且只做探活、身份与名称查询。所有逻辑槽位使用稳定的 `#槽号` 路由及 target 作用域；旧单设备键仅由配置迁移兼容。电量在该槽的 Battery.read 中单独查询，多槽位共享接收器访问锁。此修改有接口/模型测试，仍待 Unifying/Bolt 真机验证。

**边界**：HID++ 1.0-only 设备（Unifying 早期键鼠）对长帧 Ping 以 0x8F 错误应答，不匹配即视为离槽，不做 1.0 寄存器电量回退（Windows 集合拆分下短设备帧路由未经验证）；槽位类别（键/鼠）在配对寄存器缺席时按产品线默认 mouse 显示，Unifying 上配键盘的用户可见此局限。

### 7.5 狼蛛 AULA（跨 OEM 平台品牌）

来源：Aula Hub 2.2.3 官方客户端静态分析（Electron，`Aula Hub Setup 2.2.3.exe`，SHA-256 1a91cd2a…748f；NSIS 内 `app-64.7z` → `resources/app.asar` → 渲染层 `dist/assets/index-*.js` 设备表与 SDK chunk，主进程与渲染层均经 javascript-obfuscator 混淆，经 webcrack 反混淆后提取）。C45 无线键盘协议已按静态分析实现（已实现未实测），其余平台标已识别。

**家族版图**：AULA 全系至少横跨 12 个 OEM 平台，同一品牌下互不同协议，仅 3554:FA09 与本仓库既有家族重合——`F75 / F87Pro / F99` 无线键盘均为 `3554:FA09, FF02:0002`（与 KZZI 258A 家族同源，F87 Pro 已真机实测）；表中另有一条 `F87` 固件人格枚举为 `3554:FA09, FF00:0001`，同 PID 不同 Usage Page，识别必须锚定 Usage Page 的又一实例。有线键盘（`F75/F87/F87Pro/F99`）枚举为通用键盘 VID `258A`，PID `0x10C` 为主、`0x01/0x02` 亦有，Usage Page `FF00:0001`——独立的有线协议，按第 3 节规则只入附录 B。

**C45 命令协议（已实现未实测）**：新款无线键盘的 Dongle（F87ProV2 系、F108ProV2 系等）枚举为 `0x0C45×{FEFC/FEFE/FEF9}`，SDK 通道按型号落在 `FF60/FF67/FF68/FF70/FF80` 集合（usage `0x01/0x61/0x71`；FF67/FF70 为设备表 filterAlias 记录的旧固件人格页）。帧为 32 字节无编号报表：`[0]=cmd、[1]=mode、[6..8]=CRC-16/CCITT-FALSE（覆盖全帧且 CRC 槽置零，低字节在前）`；应答回显 cmd/mode、`[2]=0` 为状态正常、电量在 `[8]`、充电标志在 `[9]`，厂商客户端自身不校验应答 CRC。电量命令 `cmd=0x11, mode=0x16`（部分型号回退 `cmd=0x51`）。实现在 `protocol.rs` 的 `aula_command_request` / `aula_command_battery` 与 `vendor/aula.rs`，真机验证探针在 `examples/probe_aula.rs`；应答宽度（Windows 集合内是否带 report-id 前缀）待真机确认，两种帧长都已接受。

**未收录平台**：`38A6`（30 条键盘全部有线：FFB0:0001 ×22、FF68:0061 ×7、FF00:0001 ×1，按规则只入附录 B）、`372E`（12 条，FF60:0061，与 ATK 的 BYKB 键盘方案同源）、`320F`（20 条鼠标，FF1C:0092，`22BA` 为无线接收器）、`2E3C`（FF1B:0091 有线键盘）、`1CA2/1CA5`（FFA0）、`384C`（FFA8）、`6D66`（FF60）、`B6A4`/`A8A4`/`A8A5`（FF01，AULA L3 耳机线）、`258A` 有线键盘线（见附录 B）。各家帧格式均未分析，列入附录 B 备查。

**3554×FF02:0002 鼠标线（F5D5 无线接收器、F511/FB36/F515/F517/FB35 有线）**：与键盘接收器同 VID 同接口但 deviceType 为鼠标，帧兼容性未经证实——KZZI 适配器的 PID 锁不放开；待真机帧捕获后再评估，现记附录 B。

**主进程混淆教训**：javascript-obfuscator 输出中变量名（`_0x25af86`）会伪造成 VID 常量的假象，统计 `0x25af` 之类模式必然污染；字段提取必须以对象字面量的字段名（`'pId':`、`'usagePage':`）为锚，并以条目边界（`'pName'` 分隔）做上下文限定，否则相邻条目互相串扰。webcrack 可解 javascript-obfuscator 的字符串数组，但设备表条目散布在未反混淆的主 bundle 中时，按条目窗口提取比依赖反混淆更可靠。

### 7.6 泥炭 SOUNDPEATS（蓝牙音频，BTHENUM 系统路径）

来源：Windows 蓝牙栈行为验证（BTHENUM 枚举 + DEVPKEY 属性），SOUNDPEATS Space 真机实测；无需厂商驱动——经典蓝牙音频设备的电量由 Windows 经 HFP/AVRCP 收取，落在免提服务节点上。

**识别与身份**：枚举 `BTHENUM` 总线（非蓝牙设备类，免提节点与设备节点分属不同类），以实例 ID 中的 12 位十六进制 MAC 为物理标识（`bt:<MAC>`）；设备节点与免提服务节点按 MAC 合并，按主类音频/视频识别。HAL 优先读取正式 ClassOfDevice 属性 10，兼容旧属性 4；连接属性缺失或格式无效为 Unknown，不能当已连接。电量属性仅在 Battery.read 时读取，校验类型与长度，接受 0..=100；Windows 缓存值使用 SystemCache 来源，无法确认采样时间时不生成采样时间。应用保留明确的断开状态，在线无本轮电量时保留历史但不标成有效新读数。

**边界**：只覆盖 Windows 已记录电量的经典音频设备；蓝牙 HID 外设（LE 触控板等）不走此路径。蓝牙设备无 HID 序列号，历史合并靠 MAC，跨适配器重配对不影响。

### 7.7 复查教训（第二批新增）

**写语义的 sub_id 偏移必须真机核验**：HID++ 长寄存器读是 0x83 而非 0x82（Solaar 的 `0x8100 | 0x2B5` 中，长寄存器值的 0x200 位使 0x81 进位为 0x83；0x82 是长写）。初版把长读当 0x82 发上了真机——接收器对该写入静默忽略，配对未受影响，但这是扫描路径上真实的写操作，与"扫描零寄存器写入"红线冲突。规则：任何新寄存器路径先以探针在真机上确认应答回显，再进适配器；混淆代码中的数字常量（含字符串数组解码残留）不能单独作为字段依据，必须以字段名为锚交叉验证。

### 7.8 iKF Audio（Cm FF00 耳机线）

来源：[PR #3](https://github.com/FoLeaf/Periph-In-One/pull/3) 作者对 iKF Audio 的 ProtocolHandler/HIDManager 静态分析，以及 2026-10-25 的 38B1:3419 V11 Pro 真机记录（70%）。

匹配 `VID 38B1` + `FF00:0001`，不锁 PID；厂商 APO 驱动含 `3412/341E/3419` 多种人格。仅 V11 Pro 已实测，其余已实现未实测。复用既有 hidapi、双句柄与 output-report 传输，不增加依赖。

输出为 64 字节 report ID 2，电量查询前缀 `02 55 80 00 06 00 FF <seq> 02 FA 0C AA`，其余零填充。输入 report ID 3 的短电量帧为 `03 55 80 00 08 00 FF <seq> 04 0C <level> 00 00 AA`。解析完整固定帧头、声明长度、属性和 AA 收尾，驱动核对查询序号；伴随的心跳 `01 FE` 不作为读数。电量保留作者确认的 1..=100 范围，0% 语义未验证；该短帧不声称具有已验证的 CRC 校验。

发现使用接口元数据，不查询电量；显式读取才发 `{FA,0C}`。没有经验证的每台唯一身份命令，使用现有 USB 序列号/位置证据。超时返回结构化错误，不能证明设备不支持电量。

## 8. 独立 HAL 提取时的历史验证（2026-10-25）

实施基线 `8b07ec9`，Windows x64 / Rust GNU 工具链。以下仅为独立库提取阶段记录；当时没有增加第三方依赖或改变应用配置结构，当前配置迁移见第 9 节。

- `cargo fmt --all -- --check`、`cargo test --workspace --locked --all-targets`、`cargo clippy --workspace --locked --all-targets -- -D warnings` 通过，共 58 项测试。
- 关闭默认 features 的独立库测试通过；`x86_64-unknown-linux-gnu` 目标的库及全部测试编译检查通过。该检查不代表 Linux 真机硬件支持。
- 通过下游伪摄像头能力验证新增领域无需修改核心枚举或提供电量；回归检查独立后端失败、多组件能力、未知/不支持区别、有效 0%、历史读数、现有身份键与备注。
- 当前机器 `--list-devices` 成功；`--once` 读到 ATK 鼠标 100%，KZZI K75 报告不提供电量，未唤醒的 BIGBIG WON 路径保持未响应。输出未含序列号或真实地址。
- 本次未复验蓝牙真机电量、罗技一拖多、AULA 新款命令协议、休眠/热插拔与界面生命周期；第 7 节已有实测档位保留其原始来源，不因重构或伪后端测试升级兼容承诺。

## 9. 通用应用模型与独立能力读取验证（2026-10-25）

实现提交 `28ec87f`、`439bb7c`，Windows x64 / Rust GNU 工具链。复用现有依赖；应用启用独立库的可选 serde。配置升级为 v2，迁移测试均使用隔离临时目录，未读取或改写用户真实配置。

- fmt、workspace all-targets 测试与 Clippy（`-D warnings`）通过，共 63 项测试。
- 独立库在 Linux 目标下关闭默认 features，以及启用 serde 的库和全部测试编译检查通过；本地没有执行 Linux 测试或验证 Linux 设备访问。
- 通用身份与电量历史测试覆盖无电池设备、命名空间、跨来源可靠身份、重复序列号、槽位、冲突备注、失败范围、历史转移与读取失败保留历史。配置测试覆盖旧 ID/备注/历史保留、原字节备份、备份冲突、损坏及未知版本保护。
- 计数伪后端确认发现不采样电量、客户端可独立重复读取，以及重发现、失败、显式关闭和 Hardware 关闭会撤销客户端及克隆。这些是接口测试，不代表媒体或新协议真机支持。
- 当前机器的 `--list-devices` 与 `--once` 成功。KZZI K75 报告不提供电量；HID zkm 与 NK mouse NANO dongle 未响应；OPPO Enco Air4 离线。本轮没有取得新的鼠标或蓝牙有效电量，不沿用第 8 节的历史 100% 作为本轮实测结果。
- 总览、备注和设置截图由当前 Slint 渲染，使用明确标记的演示数据；已检查总览与备注布局。未复验蓝牙在线电量、罗技一拖多、AULA 新款协议、休眠/热插拔与完整窗口生命周期。

## 10. 后端调度、故障覆盖和组件身份验证（2026-10-26）

- HAL 为 backend 独立 worker 和能力 executor 提供稳定调度，默认整轮 deadline 20 秒；合成测试覆盖 non-Send backend 线程绑定、排队失效检查、慢后端隔离、迟到代次和可取消等待。`inventory()` 保持被动，不推进 capability generation。
- `DiscoveryReport`/`InventoryReport` 携带覆盖范围和结构化故障；Windows HID 按 ContainerId 分组 receiver interfaces，并显式提供 endpoint。单目标已识别失败仍保留 Unknown+issue 设备，多目标拓扑未知时 withheld 该 endpoint。Battery Err 和 Unavailable 作为 capability failure，不推导离线。
- HAL 显式描述 `ComponentDescriptor`、`ComponentKey`、`ComponentClaim` 和 capability 的组件引用。应用使用 `SavedComponent` 绑定来源 route 到 persistent ID；唯一可靠 claim 跨来源/重连生效，同来源重复 claim 歧义和冲突强 claim 会保留分开的历史。`legacy/main` 保持每设备精确 ID `main`。
- Windows workspace suite 中实际启动的 HAL、应用 unit、battery、core、migration suites 共 71 项通过；`tests/model.exe` 被 Windows 拒绝启动（os error 5），15 项只完成编译。相同模型 suite `--no-run` 成功，Clippy all-targets 与 fmt 通过。独立 HAL 在 Linux target 的 no-default-features / serde tests 编译通过，未在 Linux 运行测试。
- 未做真机组件 claim 或多电池 GATT 验证；当前 Bluetooth/HID 适配只暴露协议确实可确认的电池组件。合成的耳机左右耳/盒子测试验证身份模型，不代表已有真机适配。

## 11. PR #3 迁入当前 HAL 的验证（2026-10-26）

基线 `34244c5`，保留 PR 作者的三个提交并按当前发现/能力边界迁入独立库，复用既有传输，无新增依赖。

- Windows x64 / Rust GNU：fmt、workspace all-targets 测试和 Clippy（`-D warnings`）通过，94 项测试全部执行，包括第 10 节当时未能启动的 15 项 model 测试。
- 新增 6 组可移植协议回归，覆盖 VXE 命令回显、状态、长度、校验和与边界，MCHOSE 型号、iKF 查询和作者捕获的应答、心跳、坏帧头及截断；关闭默认 features 和启用 serde 均执行通过。Linux target 的这两种配置全部测试编译检查通过，本地未执行 Linux 程序。
- VXE 状态测试确认空接收器不标成不支持电量，且不会把旧 RF 身份归给当前离线外设。未修改应用核心、配置版本或组件历史规则。
- 三组设备的真机档位沿用 PR 作者记录：F58E、MCHOSE G75、iKF V11 Pro 已实测，其余条目维持未实测；本轮软件回归不代表重新完成这三台设备的真机验收。

## 附录 A：KZZI 258A 有线型号表（已识别档）

来源：驱动 Cfg.ini 明文全表，Psd 映射经复查抽查验证。全部为有线本体 PID，接收器形态以 3554:FA09 / 05AC:024F 枚举（已被现有适配覆盖），按第 3 节规则不进 matcher。代际：旧代无 Psd，新代（00CF 起）带 Psd 档案；标注沿用厂商 DevName 原文。

| PID | 型号 | 代 | 标注 |
| --- | --- | --- | --- |
| 0049 | KZZI-G68 Keyboard | 旧 | |
| 004A | KZZI-61RGB Wired Keyboard | 旧 | Wired |
| 004B | KZZI-100RGB Wireless Keyboard | 旧 | Wireless |
| 004C | KZZI-87RGB Keyboard | 旧 | |
| 004D | KZZI-71RGB Wired Keyboard | 旧 | Wired |
| 004F | KZZI-G87RGB Wired Keyboard | 旧 | Wired |
| 0055 | KZZI-61 Keyboard | 旧 | |
| 0056 | KZZI-100RGB Keyboard | 旧 | |
| 0059 | KZZI-84RGB Keyboard | 旧 | |
| 005A | KZZI-71RGB Keyboard | 旧 | |
| 005B | KZZI-987RGB | 旧 | |
| 005C | KZZI-G68RGB-UK Keyboard | 旧 | |
| 005D | KZZI-857 Keyboard | 旧 | |
| 005E | KZZI68RGB Bluetooth Dual Mode | 旧 | 蓝牙双模 |
| 0064 | KZZI-68RGB Wired Keyboard | 旧 | Wired |
| 0065 | KZZI-61RGB ISO Return Keyboard | 旧 | |
| 0067 | KZZI-100PRO Keyboard | 旧 | |
| 0068 | KZZI-987RGB Wired Keyboard | 旧 | Wired |
| 0069 | KZZI-71RGB Wireless | 旧 | Wireless |
| 006F | KZZI-68Plus Keyboard | 旧 | |
| 0070 | KZZI-K980 RGB Keyboard | 旧 | |
| 0075 | KZZI-G68RGB ISO Return Keyboard | 旧 | |
| 0078 | KZZI-87 Keyboard | 旧 | |
| 0079 | KZZI-68Pro Keyboard | 旧 | |
| 007A | KZZI-61 RGB Bluetooth Dual Mode | 旧 | 蓝牙双模 |
| 007C | KZZI-87T RGBWired Keyboard | 旧 | Wired |
| 007D | KZZI-G87RGB Wired Keyboard | 旧 | Wired |
| 0089 | KZZI-96 RGB Keyboard | 旧 | |
| 008C | KZZI-61RGB ISO ReturnWired | 旧 | Wired |
| 00A2 | KZZI-68RGB Wired Keyboard | 旧 | Wired |
| 00AA | KZZI-100RGB Keyboard | 旧 | |
| 00C6 | KZZI-K61 RGB Keyboard | 旧 | |
| 00CF | Kzzi-K75 RGB | 新 | |
| 0108 | Kzzi-K68 | 新 | |
| 0112 | Kzzi-K75 RGB key | 新 | |
| 0114 | Kzzi-K84 | 新 | |
| 0149 | Kzzi-K75Pro | 新 | |
| 0160 | Kzzi-Z98 | 新 | |
| 017D | Kzzi-K20RGB | 新 | |
| 0186 | Kzzi-K68Pro | 新 | |
| 0195 | Kzzi-K98 | 新 | |
| 01AA | Kzzi-K75Pro | 新 | |
| 01AE | Kzzi-K87 | 新 | |
| 01B0 | KZZI-K75RGB Lite | 新 | |
| 01B2 | Kzzi-K21 | 新（无 Fw 键） | |
| 01B6 | Kzzi-i75 RGB | 新 | |
| 01C8 | Kzzi-K98Pro | 异常（缺 Psd） | |
| 01D5 | Kzzi-K87MAX | 新 | |
| 01DA | Kzzi-K87Pro | 异常（缺 Psd） | |
| 024D | Kzzi-K75V2Pro | 异常（缺 Psd） | |
| 0253 | Kzzi-K75V2 | 新 | |
| 0260 | Kzzi-Z75 | 新 | |
| 027D | Kzzi K98V2 | 新 | |
| 029B | Kzzi-i98 | 新 | |
| 029F | Kzzi-i20 | 新 | |
| 02E2 | Kzzi-i98 | 新 | |

## 附录 B：狼蛛 AULA 未收录平台与型号（已识别档）

来源同 7.5（Aula Hub 2.2.3 设备表静态提取，共 156 条）。全部不进 matcher：帧格式未分析或为有线协议。3554:FA09 上的无线键盘（F75 / F87Pro / F99，另有一条 F87 FF00:0001 人格）已被现有 KZZI/Compx 适配覆盖，C45 无线 Dongle 已按 7.5 实现待真机验收，均不在此列。

**有线键盘（258A 通用 VID，FF00:0001）**：F75、F87、F87Pro、F99 的有线形态，PID 以 0x10C 为主（另有 0x001、0x002 条目）；表内型号名经字符串解码，与 PID 的逐条对应未核验。

**3554×FF02:0002 的鼠标线（deviceType=鼠标，与键盘接收器同接口不同型号）**：F511（有线）、F5D5（无线接收器）、FB36（有线）、FB35（无线）、F515（有线）、F517（有线）；帧兼容性未证实，放开 KZZI 适配器 PID 锁前需真机帧捕获。

**其余 OEM 平台（无型号级解码）**：

| VID | 条目数 | Usage Page | 备注 |
| --- | --- | --- | --- |
| 38A6 | 30 | FFB0:0001 ×22、FF68:0061 ×7、FF00:0001 ×1 | 全部有线键盘；PID：28D0/28D1/28D2/28DC/28DD/28DF/28E0/28E1/28F0/28F1/28F2/2902/2903/2904/2909/290A/272C/272D/272E/2730/2731/2767（FFB0），2722/2723/2724/2729/2742/2756/2908（FF68），291A（FF00） |
| 320F | 20 | FF1C:0092 | 鼠标平台；22B7/22B8 有线，22BA 无线接收器 |
| 372E | 12 | FF60:0061 | 与 ATK 的 BYKB 键盘方案同源；106C 等含无线键盘 |
| 2E3C | 3 | FF1B:0091 | 有线键盘 C365 |
| 1CA2 / 1CA5 | 6 | FFA0:0001 | |
| 384C | 3 | FFA8 | |
| 6D66 | 1 | FF60:0061 | 8572 无线键盘 |
| B6A4 / A8A4 / A8A5 | 5 | FF01 | AULA L3 耳机线 |

## 2026-10-27 K75 灯效修正

来源为 [KZZI 官方驱动](https://www.kzzi.com/download/1/) KZZI RGB Keyboard V2.6（20260824）；DeviceDriver.exe SHA-256 `2a35647a6d3e65b222e4b9daabcae388683da4f5ba84b15ce55240b09ed77079`。使用 innoextract 1.9（zlib/libpng 许可）仅提取资源，未复制厂商代码。`Dev/0112/KB.ini` 的 `Fw=1`、`Psd=2,0,0,0,0,37` 对应 K75 RGB key；`Dev/sc/led.xml` 的 `tc_eft10` 是川流不息，`tc_eft11` 才是随波逐流。驱动 `0x42478B` 的 Fw 分支选择 `tc_eft%d`，不能混用另两套 `tc_led` / `tc_led_mode` 表。

开灯设配置 byte 10 为 11，参数 byte 78/79 为 `04 27`（亮度 4、速度 2、彩色 7）；关闭使用 byte 10 = 0。只修改相关灯效字段，读配置失败立即返回，不用空白数据覆盖配置；回读检查效果、亮度、速度和彩色设置。写能力仅注册于 PSD `[2,0,0,0,0,55]`，其他共享 `3554:FA09` 的机型保持其既有识别和电量能力，灯效等待各自证据。

诊断使用 `cargo run --locked --example probe_kzzi`（默认只读，`--on` / `--off` 显式切换），复用 HAL 协议构造器与传输。当前 K75 已完成写入及读回验证；视觉效果仍需用户观察确认。安装包、提取资源与配置原文均不入库。
