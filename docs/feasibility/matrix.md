# M0 平台可行性矩阵

本任务只建立实验和依赖边界。记录日期：2026-10-30。状态为 `passed / failed / blocked / not_run`，不把未测项当失败布尔值，也不把本表当产品发布验收。

| 平台/能力 | 后端或条件 | 当前状态 | 实际边界 |
| --- | --- | --- | --- |
| 当前 Windows x64 | NT 10.0.26300.9711，DisplayVersion=26H2 | passed（四项 M0 实验） | 当前构建号的本机证据；不是干净稳定 Windows 10/11 验收 |
| Windows 10 22H2 x64 | 目标，最低版本尚未实机收窄 | not_run | 待独立机器安装/用户权限/后台/凭据验证 |
| Windows 11 x64 稳定版 | 目标，具体构建待记录 | not_run | 不用当前预览构建替代 |
| 当前 Windows 凭据 | keyring 3.6.3，windows-native，具体类型 WinCredential | passed | 随机测试目标写入、读回、删除和 NoEntry；没有枚举/读取真实 SMTP 凭据；DPAPI 单独封装未试 |
| 当前 Windows 后台进程 | DETACHED_PROCESS + CREATE_NEW_PROCESS_GROUP，当前用户 | passed | 用户测试入口退出之后，自有 worker 才写完成标记；不启动 GUI；未注册登录任务 |
| Windows 当前用户登录任务/重启恢复 | 计划的正式启动后端 | not_run | 本轮未安装真实后台，不替代用户登录/重启/多实例验收 |
| 当前 Windows SMTP STARTTLS | 127.0.0.1 随机端口，内存测试 CA，rustls ring | passed | 受信 CA 成功；不受信 CA 拒绝；缺失 STARTTLS 关闭；没有外部连接/真实邮件 |
| SMTP DATA 阶段 | 显式可审计实验，File::sync_all 在 DATA 前 | passed | 服务器观察阶段文件；落盘失败不发 DATA；完整测试正文后断开为 uncertain 且标记仍在；无重试器 |
| Tauri/Vue 桌面 | 计划任务 08；WebView2 156.0.4314.50 可用 | not_run | 本轮只有空前端依赖脚手架，未构建 Tauri、未运行 GUI |
| macOS 13+ arm64/x64 | 计划 Keychain + LaunchAgent | not_run | 未在 macOS 实机执行；尚未在本 workspace 启用 apple-native |
| Ubuntu 24.04 LTS x64 | 计划 Secret Service + systemd 用户服务 | not_run | 未确认已解锁 Secret Service、DBus 会话或用户服务；未启用/验证 Linux keyring 后端 |
| OAuth/签名发布 | 发行者注册、私有发布材料、真实供应商环境 | blocked | 详见 provider-registration.md；这些是外部门槛 |

运行 `scripts/feasibility.ps1 -Platform macOS/Linux -Mode Verify` 在本阶段返回 3 和 blocked 记录，不退到 mock。当前 Cargo 原生凭据依赖仅对 Windows 启用；直接在其他平台跑原生门禁会返回要求真实后端的错误。

正式无秘密观察使用 `FeasibilityResult { probe, os_build, backend, dependency_version, features, actual, evidence, status }`。执行过程和原始控制台日志在被忽略的 `test-artifacts/m0/`；审阅快照在 [docs/acceptance/m0](../acceptance/m0/README.md)。进程级环境中的本机工具链路径没有写死到正式脚本。

M0 结论：本机四项实验没有发现继续实现 Windows 核心/平台层的硬阻塞。后续任务必须实现产品级错误分类、事务化 DATA 状态和后台生命周期；不得调用实验作为生产服务。Windows 稳定版、其他平台、登录恢复、真实 SMTP 认证/投递与发布签名仍需独立证据。
