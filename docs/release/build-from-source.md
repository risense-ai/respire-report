# 从源码一键构建（Windows x64）

在源码根目录双击 **Build.cmd**。它先检查工具版本，准备锁定依赖，再复用 `scripts/package.ps1` 构建一个 `AgentNotify.exe` 与独立说明 ZIP。它不安装/更新正在使用的 AgentNotify，不修改邮箱、Hook 或系统全局环境。

## 首次准备

安装 PowerShell 7、Node.js 22.12 或更新版本、Rustup，以及 Visual Studio C++ 桌面生成工具（MSVC x64 和 Windows SDK）。这些大型系统工具缺失时入口会明确停止，不会静默安装。

Rust 版本从 `rust-toolchain.toml` 读取；pnpm 版本从 `apps/desktop/package.json` 的 `packageManager` 读取（当前 11.25.0）。正常联网构建可通过 Rustup 准备缺少的固定 Rust 工具链，并将缺少/版本不符的 pnpm 安装到仓库 `target/build-tools`，不替换全局 pnpm。前端使用 `pnpm install --frozen-lockfile`，Rust 使用 `cargo fetch --locked`，正式编译复用现有离线、锁文件及静态 CRT 构建流程。离线模式缺依赖会失败，不会更改锁文件来绕过错误。

## 非默认工具目录

复制根目录 `build.local.example.json` 为 `.build.local.json`，填写已有工具的绝对路径。此文件已被 Git 忽略，仅存本机工具路径，不要放邮箱、令牌或授权码。

- `PwshPath`：`pwsh.exe` 路径，供双击入口查找 PowerShell 7。
- `NodePath`、`PnpmPath`：`node.exe`、`pnpm.cmd` 或 pnpm 可执行文件路径。
- `CargoHome`、`RustupHome`：对应 Cargo 与 Rustup 的根目录；Cargo 的 `bin` 会加入当前构建进程的 PATH。
- `VcvarsPath`：Visual Studio 的 `VC/Auxiliary/Build/vcvars64.bat` 路径。

优先级：脚本显式参数 → 对应进程环境变量 → 本机 JSON → PATH/常规安装位置。环境变量为 `AGENTNOTIFY_PWSH`、`AGENTNOTIFY_NODE`、`AGENTNOTIFY_PNPM`、`CARGO_HOME`、`RUSTUP_HOME`、`AGENTNOTIFY_VCVARS`。无需把开发者机器目录写进正式源码。

## 单独检查 / 离线构建

```powershell
./Build.cmd -CheckOnly
# 或在 PowerShell 7 中：
./scripts/build-package.ps1 -CheckOnly -CargoHome 'D:/Tools/cargo' -RustupHome 'D:/Tools/rustup'
./Build.cmd -Offline
```

`-CheckOnly` 只检查环境，不安装依赖、不编译、不创建软件包。检查通过不等于源码已构建、依赖缓存完整或软件运行验收通过。

正常双击会保留结束提示与退出码；自动化调用可设置当前进程 `AGENTNOTIFY_NO_PAUSE=1`，避免 CMD 等待按键。成功退出码为 0，失败非 0。日志位于 `target/build-logs`；日志不记录环境变量全表、凭据或用户通知内容，但可能包含本机路径。

成功产物位于 `target/packages/AgentNotify-<版本>-windows-x64-unsigned-dev-<时间戳>/`，终端会打印 EXE、说明 ZIP 路径与 SHA256。每次生成新目录，不覆盖已有包。这是未签名开发版本，不能视为正式发布或自动更新渠道。

## 失败处理

检查缺项提示与日志，补齐工具后再运行。网络失败可稍后重试；使用 `-Offline` 需要已经具备全部依赖缓存。显式路径错误会停止，不悄悄换用其他工具。此入口不会自动清理已有发布包，也不执行单元测试全套或本机安装/通知测试。
