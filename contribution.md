# 贡献指南

感谢帮助改进 Periph In One。请先阅读 [README](README.md) 的兼容范围；遇到问题可在 [Issues](https://github.com/FoLeaf/Periph-In-One/issues) 中搜索现有记录，再提交复现信息。较大的功能或协议改动建议先讨论。

## 本地开发

项目使用 Rust 2024 + Slint，面向 Windows 10/11 x64。安装 Rust 1.92+ 及 MSVC / Windows SDK 或完整 MinGW-w64 工具链，保留 `Cargo.lock`。

```powershell
cargo run --locked
cargo fmt --all -- --check
cargo test --workspace --locked --all-targets
cargo clippy --workspace --locked --all-targets -- -D warnings
.\scripts\build-windows.ps1
.\scripts\package.ps1
```

打包前应完成 Release 构建；脚本拒绝覆盖同版本 ZIP。CI 在 Windows MSVC 上执行同一组检查，并在 Linux 检查独立 HAL。版本标签触发完整检查和 Release 草稿，操作步骤见 [CI / CD](.github/CICD.md)。Linux / macOS 不能验证 Windows 界面或硬件访问，可运行 `cargo test -p periph-hal --no-default-features --locked --all-targets`；应用核心逻辑还可运行 `cargo test --locked --lib --test core --test hardware`。

## 代码放在哪里

| 路径 | 职责 |
| --- | --- |
| `crates/periph-hal/` | 独立硬件库：公共设备/能力契约、后端注册、协议与平台实现（见 [库说明](crates/periph-hal/README.md)） |
| `crates/periph-hal/src/protocol.rs` | 共享纯报文构造、校验与解析，不执行设备 I/O |
| `crates/periph-hal/src/windows/` | HID 驱动、传输、拓扑、蓝牙属性后端；不依赖应用与界面 |
| `src/devices.rs`、`src/hardware.rs` | HAL 与应用之间的兼容转换、旧记录键、失败范围及反馈 |
| `src/model.rs` | 设备身份、历史记录、备注及合并规则 |
| `src/config.rs` | 配置读取与原子保存 |
| `src/app.rs` | Slint 数据绑定、后台任务和操作反馈 |
| `src/platform.rs` | Windows 窗口、托盘定位、自启、单实例等集成 |
| `ui/app.slint` | 页面布局和交互控件 |
| `tests/core.rs` | 协议、身份与配置回归测试 |

添加功能前，先检查当前依赖、Rust / Slint / Windows API 与可复用的开源实现。优先使用已有能力；新增依赖需说明用途、许可证与体积影响。运行时和项目工具保持 Rust、Slint 或 PowerShell，不引入 Python / WebView 运行时。

## 新设备和协议

新增或扩展设备适配的完整流程见 [设备适配规范](docs/ADAPTATION.md)。

- 精确记录 VID/PID、Usage Page / Usage、连接方式、接收器与固件版本。避免仅凭商品名或 VID 扩大匹配范围；有跨 PID 匹配需求时给出接口证据。
- 明确区分“已识别”“已实现协议”和“已实测电量”。至少检查正常读数、超时、断开 / 休眠与重连；未验证的型号和充电状态不要写成已支持。
- 协议解析需要测试正常帧、截断 / 坏帧、校验和及电量边界；按协议核实 `0%` 是否有效，不能把未知电量转换成零。
- HID 查询置于后台，设置有界超时，不阻塞 UI。不要把设置、灯效、固件或其他写入命令伪装成电量查询。
- 新设备仍需沿用身份与历史模型。不要把接口路径、ContainerId、VID/PID 或设备名称冒充可靠序列号。
- 提供协议依据的原始来源及其许可；不要提交来源不明的复制代码、厂商驱动、逆向工具或整份抓包。

## 界面和配置

页面只显示当前数据、必要操作和即时反馈。业务约束通过控件状态、字段校验、提交确认和失败反馈表达；不要堆叠常驻规则说明、重复标题、重复按钮文案或实现细节。

配置位于 EXE 同级 `config/settings.json`。保留原子保存和损坏文件保护；配置结构变更需要兼容策略与测试。修改身份匹配时检查备注保留、重复序列号、无序列号、离线历史和手动恢复。

影响窗口生命周期的改动需要在 Windows 实际检查：首次显示、缩放、隐藏、托盘唤出、微缩展开、重复启动与退出。开机启动测试应使用隔离目录和临时快捷方式，不改开发者已有启动项。隔离副本仍共用单实例锁，测试前请自行退出运行中的正式实例。

## README 图片

`assets/readme/hero.svg` 是可编辑的静态 SVG，沿用应用图标的石墨灰和橙色。软件截图必须来自当前页面，使用脱敏数据；示例数据明确标注，不能充当硬件兼容证据。

```powershell
cargo run --locked --example readme_screenshots
```

该示例用 Slint 自带截图 API 渲染完整面板、备注、设置和微缩面板，不访问 HID、真实配置、托盘或启动项。它直接覆盖 `assets/readme/` 的四张 PNG；生成后检查图片和 Git 差异。窗口管理器圆角、托盘定位与拖动行为需另做真机检查。

SVG 不使用脚本、外部字体、远程素材或 `foreignObject`。检查 GitHub 桌面和手机宽度的可读性、替代文本和相对路径。

## 提交和 Pull Request

1. 每次提交围绕一个可审阅的阶段，使用具体的提交信息，例如 `fix: preserve remarks after receiver reconnect`。
2. 提交前运行上述格式、测试与 Clippy 检查。与改动相关的硬件或界面行为需实际验证；在 PR 中注明环境、结果和未验证部分。
3. PR 描述说明问题、修改后的行为、兼容影响和验证结果；界面改动附截图，设备适配附脱敏证据。
4. 更新受影响的 README 和兼容信息，不将未完成的能力写成已发布功能。

请勿提交 `target/`、`dist/`、真实 `config/`、序列号、用户路径、临时截图、日志、安装包或本地研究目录。README 图片和应用图标属于必要资源；`Cargo.lock` 必须随依赖变更提交。不要仅为缩小仓库删除构建所需图标或第三方许可。

## 许可

提交贡献即表示你有权以本项目的 [MIT 许可证](LICENSE) 提供该贡献。第三方代码与资源必须保留来源、版权和许可证；MIT 不替代 Slint 或其他依赖的独立许可。分发包应包含项目许可和依赖声明，并保留 Slint 署名。
