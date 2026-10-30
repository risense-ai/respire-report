# 把本文件交给 Codex 或其他 Agent

用户只需向 agent 说明：

> 请按这个迁移包里的 AGENTS.md 和 Codex邮件通知器使用与迁移说明.md，在本机安装 Codex 邮件通知工具。自动完成环境探测、路径适配、Hook 合并和隔离检查。缺少 Python 时可以安装用户级 Python。保留现有配置；邮箱信息再问我，授权码通过本机安全输入窗口填写。不要自动发送邮件或绕过 Hook 审核。

Agent 从这里开始：

1. 确认 Windows、当前登录用户、用户正在使用的 Codex 配置目录。
2. 读完整迁移说明。默认安装到 `%LOCALAPPDATA%\CodexNotifier`，不要把临时解压目录当长期安装目录。
3. 在解压后的本目录运行 `Install.ps1 -DryRun`。没有可用 Python 时，预演会停止并报告；在用户已授权安装的范围内，用 `Install.ps1 -InstallPython -CreateShortcut` 安装依赖并继续安装工具。
4. 核对计划后运行 `Install.ps1 -CreateShortcut`，自动复制必要文件并合并四类通知 Hook。
5. 运行安装目录中的两组隔离测试。收集用户的 SMTP 主机、端口/加密方式、发送/接收邮箱，配置完成后打开授权码安全输入窗口。
6. 引导用户审核新增/变更 Hook，按需重载客户端；如获发送授权，再做真实邮件验证。

运行环境为 Windows，Python 3.12 或以上（推荐 3.13）。只有包文件需要传到新电脑。Python 运行时、Codex 客户端、网络/企业权限不在包内；缺少 Python 时 agent 可联网安装。离线且没有 Python 时，需要另行提供官方 Python 安装介质。

首次迁移通常只剩用户填写邮箱信息、输入授权码、审核 Hook。若系统或企业策略阻止安装，agent 必须说明，不能承诺所有机器完全无人参与。
