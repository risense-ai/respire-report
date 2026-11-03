# AgentNotify 单运行 EXE 实施计划

> **For agentic workers:** 按主控已分派职责执行；参考 superpowers:subagent-driven-development，不自行递归分派。用户已选择单 EXE 并授权实施，不重新发起设计审批。

**Goal:** Windows 用户只需一个 `AgentNotify.exe` 即可运行设置界面、Hook 接收与后台通知；说明材料另行提供 ZIP。

**Architecture:** 一个 Rust/Tauri 运行文件通过参数选择 GUI、CLI 或后台入口，不自解压三个程序。配置、队列、系统凭据与当前用户登录任务仍由程序管理。既有安装的稳定 launcher 与已信任 Hook 需要兼容保留。

**Tech Stack:** Rust、Tauri/Vue、Windows 当前用户计划任务与系统凭据、PowerShell 发布脚本。

**Spec:** 用户本轮已确认的单运行 EXE 方案；基础设计见 `../../design/01-design.md`。本计划中的本轮边界优先于旧多 EXE 交付说明。

## 全局边界

- 只改 AgentNotify 源码和公开交付材料；不因打包迁移移动当前安装目录。
- 保留通知设置、队列、系统凭据和客户端 Hook 原始字节；不替换其他工具入口、不伪造客户端信任。
- 不把个人配置、邮件正文、历史队列或凭据写入交付物。
- 用户运行无需 VBS、CMD、PS 脚本入口；系统内部服务管理依赖按实际实现说明。
- 既有 `launcher.exe` 兼容入口可保留；“一个交付 EXE”不等于承诺安装目录只有一个文件或一个进程。
- 减少测试：集中必要构建、静态契约与纯产物检查；不新增测试套件，不自动发信或重启 Codex；现场保全并确认回退方式后由 native 受控升级。
- macOS/Linux 原生实现与实机验证仍待办。

## 分工和接口

| 责任 | 文件范围 | 交付 |
|---|---|---|
| native_finish | Rust 入口、CLI/daemon 库化、安装与兼容运行接口 | 统一 EXE 和准确命令路由；向包装执行者确认构建目标和产物路径 |
| product_migration_cleanup | `scripts/package.ps1`、`packaging/windows/`、公开交付/迁移文档、本计划 | 单 EXE 交付，独立说明 ZIP，无个人数据 |
| desktop_accept | 独立只读核验最终源码与产物 | 基于具体版本的验收或整改意见 |
| 主控 | 跨模块协调、用户沟通、授权范围和通知故障跟踪 | 统一汇总实际证据与待办 |

默认入口优先发现并安全验证用户 profile 下已有 AgentNotify 安装，避免误入另一个数据目录或放宽 AppData ACL；多个安装时要求明确选择。

native 已确认接口：Cargo 目标 `agentnotify-desktop`，产物 `target/release/agentnotify-desktop.exe`，交付时命名 `AgentNotify.exe`。无参数或仅 `--data-dir <root>` 打开 GUI；其余现有 CLI 命令保留；内部 `daemon` 运行后台，`launch-daemon` 保持稳定任务命令。新安装根 `AgentNotify.exe` 为唯一活动程序；既有安装保留同字节 `launcher.exe` 别名以保持 Hook/任务命令不变。安装记录 `runtime_layout` 缺省 legacy、新值 unified。单 EXE 安装以 `unified_signed_update_pending` 拒绝旧三文件签名更新格式；正式自动更新未开放。具体升级操作以 native 实际实现为准，使用新版单 EXE 和 Agent 辅助的受控流程，不提前编造命令。

## 执行步骤

- [x] **1. 统一原生入口。** 将 GUI、现有 CLI 与 daemon 编译进同一 EXE；校验无参数启动和带参数调用分别进入正确分支，后台不打开前台窗口。
- [x] **2. 保留安装兼容。** 审核旧 launcher/安装记录的继续使用与新安装路径；任何现场升级前明确回退点，保持设置和 Hook 字节不变。
- [x] **3. 改交付。** 打包脚本只复制统一 EXE；说明、迁移、Agent 提示词、构建信息、哈希与许可证另入说明 ZIP。运行程序不依赖说明 ZIP 或脚本。
- [x] **4. 同步文档。** 公开入口说明只介绍 AgentNotify 自身；跨机迁移重新建立系统凭据和本机绑定，不复制信任/队列。旧多 EXE 交付命令不得伪装为当前接口。
- [x] **5. 最少验证。** native 统一执行必要构建；包装执行者解析脚本并检查 EXE/说明 ZIP 内容和哈希，不重复构建、不启动真实通知。
- [x] **6. 独立验收。** desktop_accept 核对入口、安装兼容、打包依赖和文档一致性；问题修正后再核验，不设固定轮数。未验证项明确保留。

## 重点复核

- 从任意工作目录、含空格路径启动；无参数打开界面，有 Hook 参数时不打开界面。
- 用户关闭 GUI 不应改变后台通知配置；同文件后台与界面可为不同进程。
- 旧安装的 launcher 与清单哈希机制不能因单 EXE 改造失效，也不能跳过所有权校验。
- 发布文件不得悄悄落地三套运行程序或依赖缺失的相邻文件。
- 真实邮件投递结果不得由编译、解析 Hook 或 SMTP 测试替代。

## 独立未决问题：当前 Codex 通知

用户仍报告正常会话通知不正常，本项保持未解决。独立核验使用新的独立 app-server 执行 alpha `hooks/list`，当前解析的五项 Hook 均已信任且哈希一致；这只能证明磁盘配置的当前解析/信任，不证明原宿主已经加载或自然任务事件已触发。原宿主加载与自然触发待验证。本轮不重启 Codex、不改信任、不主动发信，不以用户曾收到邮件作为通过依据，也不阻断单 EXE 源码与包装改造。

## 本轮交付证据

- native 报告统一 release 构建成功，运行文件 SHA256 为 `837bd9b7d62e70b4abc654a668888fc1bf9b70581ff503d167a73225183742a2`；包装前后实际比对一致。
- 仅执行 `scripts/package.ps1 -SkipBuild`，交付目录 `target/packages/AgentNotify-0.1.0-windows-x64-unsigned-dev-20261102-171033/` 只有 `AgentNotify.exe` 与 `AgentNotify-docs.zip`。说明 ZIP SHA256 为 `197af9f47444054ec3871ef0e5fd22f7eab56a1228cbb7c3851c1d7858c5edf0`。
- 包装执行者仅只读校验：10 项白名单文本、9 项文件摘要完整、路径安全、无脚本/程序/运行配置/队列、独立运行文件摘要与构建信息一致。未启动交付 EXE、安装或发信。
- 最终 171033 / 837BD9 产物与本机受控升级已由独立验收确认通过，范围限本轮统一入口、安装兼容、实际本机运行链和交付；不代表自然通知或全部平台验证通过。

最终产物包含 Diagnostics 卸载说明改为随附说明的 UI 修正；此前 170445/F0980 产物保留为历史，不作为最终交付。171033 包增量校验确认七份公开说明字节不变，运行文件与元数据摘要已更新。

## 本机升级收尾与保留待办

按主控汇总的独立验收和 native 实施回报：统一入口、legacy/unified 安装兼容、最终编译、现场保全后的本机受控升级与最终产物独立验收已完成。当前 doctor 报告 running、issues 为空；用户最新 config.json、host.json 和客户端 Hook 原始字节保持不变。计划任务后台链独立于 GUI，现有稳定兼容入口保留。此处是协作方证据归档，包装执行者未额外运行或部署程序。

仍待验证：

- 自然 Codex 会话邮件通知仍未解决；待用户自行重开客户端后实测，不由本轮代码构建、Hook 解析或既往收件推定通过。
- 17:13 曾有一次后台退出，原因未知；已恢复运行，不把恢复状态当作根因已修复。
- 新电脑首次安装、系统重启/登录恢复及 macOS/Linux 实机验证尚未完成。

本次仅更新计划归档；计划未纳入说明 ZIP，不重新打包最终交付物。
