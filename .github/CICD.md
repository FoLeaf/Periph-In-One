# CI / CD

## 工作流

| 入口 | 执行内容 | 产物 |
| --- | --- | --- |
| 分支 push、Pull Request、手动运行 CI | 格式 → Windows 测试 → Clippy → Release 构建 → 打包 | Actions artifact，保留 14 天 |
| 推送 `v*` 标签 | 复用完整 CI → 下载该次构建产物 → 验证 SHA256 → 创建 Release 草稿 | 便携 ZIP、ZIP 的 `SHA256SUMS.txt`、自动生成的发布说明 |

格式检查使用 Ubuntu；Portable HAL 在 Ubuntu 测试关闭默认 features 的独立库、可选 serde 及 Clippy。程序测试、Slint 编译、资源嵌入和打包使用 Windows Server 2022 + x64 MSVC。所有 Cargo 构建和依赖解析保留 `--locked`。工具链沿用 stable；Cargo / 构建缓存使用 `Swatinem/rust-cache`，只有 main 更新缓存。

同分支的新提交会取消旧 CI；标签发布不自动取消。普通检查只有仓库读取权限，仅创建草稿的 job 具有 `contents: write`。无需个人访问令牌，使用该次运行的 `GITHUB_TOKEN`。工作流不使用 `pull_request_target`，不把写权限交给 PR 构建。

CI 调用已有 PowerShell 构建和打包脚本，ZIP 包含许可声明与程序自身的校验和；外层 `SHA256SUMS.txt` 校验整个 ZIP。Release 不重新构建、不覆盖现有发布。同一标签重跑时，已有 Release 会使创建步骤失败；修正问题后发布新版本，不移动已公开标签。

## 发布一个版本

1. 更新 `Cargo.toml` 的版本、`Cargo.lock` 和 README 中的版本 / 包名，提交并合并到 main，确认 CI 通过。
2. 在该提交创建与 Cargo 版本完全一致的标签，例如：

   ```powershell
   git tag -a v0.1.9 -m "Release 0.1.9"
   git push origin v0.1.9
   ```

3. 等待 Release 工作流完成。在 GitHub Releases 打开草稿，下载 ZIP，核对校验和，完成 Windows 10/11 真机验收，编辑发布说明后点击 Publish release。预发布版本同时勾选预发布选项。

标签必须等于 `v` + `Cargo.toml` 中的版本，不一致时在 Windows 编译前失败。标签本身代表发布意图，发布不依赖此前分支 CI 的结果，而是对标签提交重新运行完整检查。

## 仓库设置

在 GitHub Settings 中启用 Actions，并允许上述官方 Actions、`dtolnay/rust-toolchain` 和 `Swatinem/rust-cache`。组织策略需允许发布 job 申请 `contents: write`。

建议为 main 配置分支保护 / ruleset，要求 `Rust format`、`Portable HAL`、`Windows x64` 三项检查成功后再合并，并限制 `v*` 标签的创建、更新与删除权限。这些是 GitHub 仓库设置，提交 YAML 不会自动启用。

Dependabot 每周合并分组检查 GitHub Actions 版本更新，更新 PR 经过同一 CI。Rust 依赖升级仍需评估 Slint 许可、兼容性和体积后单独提交。

## 验收边界

CI 验证编译、自动化测试、Clippy 和打包，不验证真实 HID 读数、Windows 10 兼容性、托盘交互、多屏 / DPI 或实际开机启动行为。这些项目在发布草稿阶段用真机验收。

实现参考：[GitHub Rust CI](https://docs.github.com/en/actions/tutorials/build-and-test-code/rust)、[Rust cache](https://github.com/Swatinem/rust-cache)、[GitHub CLI 发布](https://cli.github.com/manual/gh_release_create)。
