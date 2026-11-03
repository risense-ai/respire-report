# M0 实际执行证据（2026-10-30）

实现任务状态：DONE_WITH_CONCERNS，待主控独立审查。该状态只针对任务 01 的实验和脚手架，不是整个产品/平台验收。

本机：Windows NT 10.0.26300.9711，DisplayVersion=26H2，x86_64-pc-windows-msvc；Rust/Cargo 1.95.0，Node 24.21.0，pnpm 11.25.0；cl 19.44.35229、SDK 10.0.22621.0；WebView2 156.0.4314.50。注册表 ProductName 仍写 Windows 10 Pro，不能由该名字推出稳定 Windows 10 支持结论。

命令运行前已记录 git status；既有未跟踪为 .gitignore 和 docs/design 四文件。任务脚手架创建前，仓库只有 LICENSE 和主控交付的设计/过程文件。没有覆写它们。

| 命令 | 实际返回码 | 结果 |
| --- | --- | --- |
| pwsh -File scripts/feasibility.ps1 -Mode Baseline | 2 | 非通过，四项 not_run |
| pwsh -File scripts/dev.ps1 test --locked --no-fail-fast --test credential_real --test service_real --test smtp_local -- --nocapture（仅 stub 时） | 101 | 四测试因对应探针尚未实现而失败，见 red.txt |
| pwsh -File scripts/feasibility.ps1 -Mode Verify -Platform Windows | 0 | 四项真实本机实验 passed；helper 测试是被主探针显式调用的辅助子进程 |
| pwsh -File scripts/dev.ps1 test --workspace --locked | 0 | 四项实验通过；两个 helper 默认 ignored，主探针内部真实执行；其余空脚手架零业务测试 |
| pwsh -File scripts/dev.ps1 metadata --locked --format-version 1 | 0 | 锁定依赖可解析；不是功能通过 |
| pnpm --dir apps/desktop install --lockfile-only | 0 | 生成空前端依赖锁文件 |
| pnpm --dir apps/desktop install --frozen-lockfile | 0 | 冻结依赖解析成功；没有 Tauri/Vue 功能 |
| pnpm --dir apps/desktop run check | 0 | node --check 空工具脚本成功 |
| pwsh -File scripts/dev.ps1 fmt --all -- --check | 0 | Rust 格式检查成功 |

早期 dev.ps1 参数绑定失败（退出 1）、一次 PowerShell -replace 拼接错误（退出 1）、Verify 的 -- 分隔符丢失（退出 1）均已修正。它们不是程序 RED 证据；真正 RED 是编译成功后四个 not implemented 的失败。首个包装 shell 未显式传播子退出码时返回 1，随后显式记录 Baseline 子退出码确为 2。后续加入 accept 超时曾令 Windows accepted socket 继承非阻塞状态，两项 SMTP 测试真实失败（exit 101，EHLO io kind=WouldBlock）；显式 set_nonblocking(false) 后最终 Verify、workspace 与 fmt 检查均重新返回 0。最终按本测试精确进程名查询遗留 worker 为 0。

已保存的正式快照：

- baseline.json：最初基线，OS API 不含 UBR；完整构建号由 Verify 注册表补充。
- verify.json：四项完整 FeasibilityResult、工具可用性与退出码。
- credential_real_roundtrip.json：具体 WinCredential 原生后端、删除/NoEntry。
- service_gui_independence.json：用户入口结束后独立 worker 完成；没有安装真实后台。
- smtp_local_tls_probe.json：测试 CA 正负例与 STARTTLS 拒绝。
- smtp_data_stage_observable.json：同步阶段文件、持久化失败阻断、最终应答丢失为 uncertain。
- red.txt：实现前实际测试失败输出，只有路径/错误，没有秘密。

原始输出、metadata 和过程产物保留在被忽略的 test-artifacts/m0，未暂存。随机测试凭据已删除，临时 CA/密钥只在进程内存，临时目录由 tempfile 清理，实验 worker 自行退出。未创建开机服务/登录任务、未改真实 Hook、未读取现存 SMTP 条目、未发真实邮件、未重启系统/客户端。

DATA 文件的 sync_all 只证明本机实验的持久阶段控制和顺序；没有做断电测试、SQLite 生产事务、网络重试器或完整 SMTP 协议验收。受信根仅放在单次 rustls RootCertStore，系统信任库未修改。

未测/阻塞边界见 [matrix](../../feasibility/matrix.md) 与 [provider registration](../../feasibility/provider-registration.md)。稳定 Windows 10/11、Tauri GUI、登录任务和恢复、macOS/Linux、真实认证投递和签名注册仍是独立门槛。
