# M0 锁定依赖与开发入口

| 依赖 | 实际版本 / features | 选择与验证 |
| --- | --- | --- |
| Rust/Cargo | 1.95.0 / edition 2024 | rust-toolchain.toml 固定；本机编译与 cargo metadata --locked 成功；不是最低 Rust 版本承诺 |
| keyring | 3.6.3 / default-features=false, windows-native（仅 Windows target） | 原生 WinCredential 类型检查和真实随机条目读写删；没有调用 mock |
| rustls | 0.23.46 / default-features=false, std, ring, tls12 | loopback STARTTLS；不使用默认 aws-lc 构建链；客户端正常证书/主机名验证 |
| rcgen | 0.14.5 / default-features=false, crypto, ring | 只生成实验用内存 CA/leaf；不保存私钥、不装系统根证书 |
| serde / serde_json | 1.0.229 / 1.0.150 | serde derive；无秘密观察 JSON |
| uuid | 1.28.0 / v4 | 随机实验凭据名称/内容；不记录秘密 |
| tempfile | 3.27.0 | 隔离目录自动清理 |
| Node / pnpm | 本次 24.21.0 / 11.25.0 | desktop packageManager 固定 pnpm；已生成 pnpm-lock.yaml，并冻结解析成功；没有 GUI 依赖 |
| 原生编译依赖 | MSVC cl 19.44.35229，Windows SDK 10.0.22621.0 | 复用已安装组件、只设置本次进程环境；没有安装/升级工具链 |
| WebView2 Runtime | 156.0.4314.50 | 本机注册表可见，不等于新 Tauri 应用已经启动 |

Cargo.lock 锁定直接与传递依赖。此次 cargo 真实解析/下载了所需缺包，并通过编译；未把 crates.io 的候选名字当作通过。所选 keyring 3.6.3 不是最新主版本，这是本次有明确原生 feature 文档并已实测的实验锁定，不代表免除后续版本维护评估。

核查依据：

- [keyring 3.6.3 官方 API/features 文档](https://docs.rs/keyring/3.6.3/keyring/)：未启用对应原生 feature 时可能使用 mock，因此本任务强制 windows-native 且检查具体类型。
- [rustls 官方 API/feature 文档](https://docs.rs/rustls/0.23.46/rustls/)；实际锁定源码的 Cargo.toml 与编译解析确认 std/ring/tls12 配置。
- [rcgen 0.14.5 官方 API](https://docs.rs/rcgen/0.14.5/rcgen/)；已下载的锁定源码含 sign-leaf-with-ca 示例，实验按该版 CertifiedIssuer/CertificateParams API 编译通过。
- 具体包版本、source/checksum 以仓库 Cargo.lock 为准；不将本机缓存路径写入项目配置。

## 运行

先在具有已安装 Rust 1.95.0、Node、pnpm 的终端执行；非默认 Rust 安装可在当前进程设置 PATH、RUSTUP_HOME、CARGO_HOME。不修改全局 PATH，不安装 Rust/MSVC 来掩盖缺项。

Windows 的 scripts/dev.ps1 优先复用当前 Native Tools 环境；否则通过 vswhere 发现安装；便携安装可在当前进程设置 AGENTNOTIFY_VCVARS 指向自己的 vcvars64.bat。正式脚本不含本机磁盘位置。

```powershell
pwsh -NoProfile -File scripts/feasibility.ps1 -Mode Baseline
# 预期退出 2：只登记缺失/尚未接受的能力，不是功能通过。
pwsh -NoProfile -File scripts/feasibility.ps1 -Mode Verify -Platform Windows
pwsh -NoProfile -File scripts/dev.ps1 test --workspace --locked
pwsh -NoProfile -File scripts/dev.ps1 metadata --locked --format-version 1
pwsh -NoProfile -File scripts/dev.ps1 fmt --all -- --check
pnpm --dir apps/desktop install --frozen-lockfile
pnpm --dir apps/desktop run check
```

scripts/dev.ps1 不传参数时等价于 cargo build --workspace --locked。调用此脚本时请用 pwsh -File 保留 Cargo/libtest 的 -- 分隔符；PowerShell 脚本内部调用要将分隔符写作字符串 '--'。

当前 workspace 的 core/adapters/platform/integration/agent_support 是独立空 lib，core 不依赖 adapters 或 platform。CLI/daemon 只有明确返回 2 的占位入口。desktop 只有 package/锁文件和语法检查；这些零业务代码均不计入功能完成。Tauri/Vue、SQLite、clap、tokio、lettre 等尚未加入产品依赖，后续任务按真实需要再锁定。

## SMTP 选择限制

本轮证明可审计 SMTP 实验能够在 DATA 前同步阶段文件。没有采用高层 lettre send() 的单一成功/失败结果来猜阶段，也没有评估/声称 lettre 的阶段回调能力。后续产品实现必须把 data_started 纳入 SQLite 持久状态、明确 DATA 后不确定结果，并另测 AUTH、隐式 TLS、真实服务器、多行响应、点转义和中途断连。实验使用保留 .invalid 地址、无 AUTH、合成正文和 loopback，不是完整 SMTP 客户端。

Linux Secret Service 与 macOS Keychain 尚未启用或解析实机原生依赖。不能从 Windows Cargo.lock 解析推断两个平台的安全存储、WebView 或后台能力。
