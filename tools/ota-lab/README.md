# OTA 阶段 A 隔离原型

此 crate 仅含一次性测试程序和库对照测试，不连接真实发布源，不读写应用配置，不修改正在使用的主程序。

```powershell
cargo test -p ota-lab --locked --all-targets
```

2026-10-26，Windows / Rust 1.98 GNU 实测：5 项测试通过。两个测试 EXE 分别内置版本 0.0.1 和 0.0.2。

| 原语 | 结果 | 决策 |
| --- | --- | --- |
| self-replace 1.5.0 | 候选不存在时返回失败，但固定 EXE 已被挪走 | 不采用 |
| self_update 1.3.2 MoveAll | 普通错误可回退；源码逐个移走目标后再放入候选 | 不用于固定 EXE 的提交 |
| self_update 1.3.2 Move，无 replace_using_temp | 运行中的目标拒绝替换且旧入口保留；退出后替换成功；独立复制的持久备份可恢复 | 作为后续事务原型的候选提交原语 |

签名测试通过回环 HTTP 下载 zipsign ZIP，验证大小上限、原文件名绑定、错公钥和截断包拒绝。库允许空公钥直接返回成功，应用必须显式拒绝空信任集。

本轮证据只覆盖进程运行时的文件系统操作，**不代表断电、事务恢复和杀毒隔离验收已经通过**。不能据此直接开启 stable OTA。正式事务须先复制并刷盘备份，再用库的直接替换提交，不得回到先挪走入口的方案。

库源码：[Move](https://docs.rs/self_update/1.3.2/self_update/struct.Move.html)、[MoveAll](https://docs.rs/self_update/1.3.2/self_update/struct.MoveAll.html)、[self-replace Windows](https://github.com/mitsuhiko/self-replace/blob/1.5.0/src/windows.rs)。
