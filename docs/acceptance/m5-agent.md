# M5 Agent 协助包交付证据

任务 09 实现固定六成员目录包：`task.json`、`doctor.json`、`plan.json`、`compatibility.json`、`START_HERE.md`、`MANIFEST.json`。`MANIFEST.json` 不自校验，标记 `local-diagnostic-bundle` 与 `publisher_signature=not_applicable`；逐成员长度和 SHA-256 仅供一致性检查。缺少安全预演时 `plan.status=unavailable`，不编造步骤。

导出仅使用受限任务枚举、实例 ID、白名单故障码与状态、路径别名。原始配置字节、日志、环境变量、任务正文、错误原文、凭据和邮箱地址均不进入包。提示词在本机显示必要绝对路径并先预览；隐藏路径时提示用户附包，上传包始终只含别名。五个场景卡共用固定主协议，包内授权声明不构成当前用户授权，默认禁止试邮件。Agent 回报需本机重新核验，UI 不依据自述更新机器状态。

验证范围：离线 `cargo check -p agentnotify-agent-support -p agentnotify-cli -p agentnotify-desktop` 返回 0；`pnpm build`（含 `vue-tsc --noEmit` 与 Vite）返回 0；离线 `cargo build -p agentnotify-cli -p agentnotify-desktop --release` 返回 0。Windows 产物 `target/release/agentnotify.exe` SHA-256 为 `A60CEAE02EE5E0584EBD250608126F3D9ED2409EC91326156E64784FF76C4DD5`，`target/release/agentnotify-desktop.exe` 为 `FD8DA7F0DA655CA6961A5253B7097254DB574F3A9204EF6FC82F05BFA7EDAF4A`。按当前用户要求未新增/运行自动测试，未运行实际安装、Hook、SMTP 或客户端接入；本记录不是产品验收。
