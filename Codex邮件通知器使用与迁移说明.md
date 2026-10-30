# Codex 邮件通知器：Agent 执行版迁移说明

归档版本日期：2026-10-30。仅适用于 Windows Python 初版，不是 AgentNotify 通用正式版或正式 release。面向 Windows 上的 Codex 或其他有文件和命令执行能力的 agent。用户授权迁移后，agent 完成技术操作；用户负责提供信息、私密输入授权码及客户端必要的审核。

## 1. 成功标准与职责

| 工作 | 责任方 | 完成证据 |
|---|---|---|
| 校验迁移包、探测/安装 Python | agent | 清单校验与实际解释器探测通过 |
| 选择安装目录、处理路径 | agent | 安装器 JSON 中的绝对路径 |
| 备份、复制程序、合并 Hook | agent | 安装器结果及重复运行无额外变更 |
| 填写 SMTP 主机、端口、加密、邮箱 | 用户提供，agent 配置 | 字段完整，不涉及授权码 |
| 保存授权码 | 用户在本机安全窗口输入 | 文件存在；不读取明文验证 |
| 审核新增/修改的 Hook | 用户 | Codex 中显示可信并加载 |
| 隔离回归检查 | agent | 两组测试通过，不发送邮件 |
| 真实邮件检查 | 用户明确授权后由 agent 协助 | 实际触发及收件确认 |

安装器不会登录邮箱、读取旧机凭据、自动信任 Hook、重启 Codex 或发送邮件。“文件安装成功”不等于“邮件链路已验收”。

## 2. 包内容与隐私

包内固定文件：

```text
AGENTS.md
START_HERE.md
README.md
Codex邮件通知器使用与迁移说明.md
Install.ps1
install_notifier.py
notifier.py
保存授权码.ps1
settings.example.json
tests/test_notifier.py
tests/test_install_notifier.py
manifest.sha256.json
```

`manifest.sha256.json` 记录其余 11 个文件的 SHA256。安装器校验固定清单和内容；清单用于检测缺件或损坏，不是发布者数字签名。

不包含真实 `settings.json`、`smtp.credential.xml`、`events.db`、日志、锁文件、备份、缓存、个人快捷方式或任何 Codex 登录/信任配置。不要整目录压缩旧安装目录。包内不带 Python、Codex 或 Respire；通知器不依赖安装 Respire，检测不到其来源时保留原通知策略。

## 3. 环境与路径选择

| 项目 | 规则 |
|---|---|
| 操作系统 | Windows，使用最终收邮件用户的普通账户；不要使用 SYSTEM 代用户保存凭据 |
| Python | 3.12 或以上，建议 3.13；必须实际导入 tkinter、sqlite3、ssl、ctypes 等依赖；无需 pip 安装第三方包 |
| 目标目录 | 用户指定优先，否则 `%LOCALAPPDATA%\CodexNotifier`；使用长期可写目录 |
| Codex 目录 | `-CodexHome` 显式指定优先，其次环境 `CODEX_HOME`，否则当前用户的 `.codex` |
| 路径适配 | 安装包使用相对文件名；运行时由参数、Python 启动器、PATH 和当前用户目录解析实际绝对路径，不依赖原机器盘符 |
| 网络 | 现有 Python 可离线安装本工具；缺少 Python 时需联网安装或预先提供官方安装介质；邮件发送还需 SMTP 网络访问 |

`Install.ps1` 探测显式 Python 路径、常见安装位置、Python 启动器和命令搜索结果，并实际检查可用性。不要求用户自己寻找或修改 Python 路径。

本版安装器支持空格、中文和括号路径；拒绝含双引号、换行、`%`、`!` 的路径及经过符号链接/目录联接重定向的目标。遇此情况，agent 选择普通用户目录或说明限制，不尝试绕过检查。

没有可用 Python 时，且用户已授权安装依赖，使用 `-InstallPython` 请求通过 WinGet 安装用户级 `Python.Python.3.13`。不永久修改 PowerShell 执行策略。WinGet 缺失、离线、软件源不可用或企业策略阻止时，agent 根据具体错误处理；必要时从 [Python 官方 Windows 下载页](https://www.python.org/downloads/windows/) 获取适合该架构的安装程序，核对发布者签名，再按当前官方参数安装。不要使用第三方转载包。无法继续时只报告真实阻塞，不要求用户手改所有路径。

## 4. Agent 的执行步骤

### A. 解压、预检、预演

解压到临时准备目录并读取本包入口。若收到 ZIP 的 `.sha256` 旁文件，可先核对归档哈希。以 PowerShell 变量完整传递空格、中文、括号路径，不拼接未经引用的命令。

```powershell
$packageDir = '实际解压目录\CodexNotifier'
$installScript = Join-Path $packageDir 'Install.ps1'
& $installScript -DryRun
```

若没有 Python，且获准安装：

```powershell
& $installScript -InstallPython -CreateShortcut
```

`-DryRun` 不写通知器、Hook 或安装 Python；没有可用 Python 时会停止并报告。`-InstallPython` 的实际执行会在校验包后安装依赖并继续安装工具，只在用户已授权迁移和安装依赖时使用。已安装成功无需重复执行 B 步，可直接继续 C 步。

已有可用 Python 时也可直接运行底层程序：

```powershell
$pythonExe = '已探测的绝对路径\python.exe'
& $pythonExe -X utf8 (Join-Path $packageDir 'install_notifier.py') --dry-run
```

使用非默认位置时，两次调用均传相同参数：

```powershell
$targetDir = '用户指定的长期目录\CodexNotifier'
$codexDir = '用户实际使用的 Codex 目录'
& $installScript -TargetDir $targetDir -PythonPath $pythonExe -CodexHome $codexDir -DryRun
```

预演输出 JSON 包含实际目标、Python、Codex/Hook 路径、待修改/保留项及缺少的用户信息。若包损坏、已有 `hooks.json` 不是有效预期结构或路径不安全，先处理明确错误；禁止直接覆盖原配置。

### B. 执行安装

```powershell
& $installScript -CreateShortcut
# 使用自定义路径时保留 A 步的参数：
# & $installScript -TargetDir $targetDir -PythonPath $pythonExe -CodexHome $codexDir -CreateShortcut
```

安装器自动备份受影响文件、复制固定文件、新建缺失的空白配置，并合并 `UserPromptSubmit`、`Stop`、`Interrupt`、`SessionEnd` 四类 Hook。已有 `settings.json`、凭据和其他 Hook 保留。已识别的旧通知器命令更新为新路径；未知包装命令不擅自删除，agent 应查看安装警告并处理歧义。

Hook 使用解释器、脚本和 Codex 目录的准确绝对路径；带 `--codex-home` 参数，让转录、会话标题及 Respire 来源判断使用同一目录。安装器不会修改全局环境变量或复制 Hook 信任记录。仅写某个 Codex 目录不会让客户端切换目录，agent 必须选客户端实际使用的目录。

重复安装应不重复添加 Hook；没有变更时不重写 Hook、不制造备份。安装目录内可创建 `通知设置.lnk`，不会自动开设置窗口，也不建立系统服务、开机项或定时任务。

### C. 隔离检查

使用安装器返回的 `python_path` 与 `target_dir`，不要回到源机器路径：

```powershell
& $pythonExe -B -X utf8 (Join-Path $targetDir 'tests\test_notifier.py')
& $pythonExe -B -X utf8 (Join-Path $targetDir 'tests\test_install_notifier.py')
```

测试使用临时目录、虚构配置和模拟发送，不读真实凭据、不改当前 Hook、不发邮件。再次运行安装预演，确认不会重复创建 Hook。存在未通过项时先整改，不能跳过后直接声称迁移完成。

### D. 一次收集非秘密配置

向用户集中询问缺失的：SMTP 主机、端口、加密方式（`ssl` 或 `starttls`）、发送邮箱、接收邮箱。若用户已提供则直接使用。通知默认 `auto`、离席阈值 300 秒、停滞阈值 1800 秒、记录保留 30 天、免打扰 23:00–07:30、包含本轮提问和回复；可按用户要求调整。

Agent 读取目标 `settings.json`，只合并用户明确提供的字段，保留其他设置；或打开 `notifier.py settings` 让用户填写。不得将授权码写入 settings、聊天、命令参数、日志或包清单。

### E. 安全保存授权码

邮箱配置完成后，agent 为用户打开目标目录中 `保存授权码.ps1` 的交互窗口。此窗口需要用户看到并输入，不能作为隐藏后台进程启动。用户只输入 SMTP 授权码，无需编辑脚本。脚本以安全输入读取并使用当前 Windows 用户的 DPAPI 保存。

```powershell
$credentialScript = Join-Path $targetDir '保存授权码.ps1'
Start-Process -FilePath 'powershell.exe' -WindowStyle Normal `
  -ArgumentList @('-NoProfile', '-NoExit', '-File', ('"' + $credentialScript + '"'))
```

旧电脑的 DPAPI 文件不能直接迁移使用；agent 只检查新文件是否存在，不解密或打印。若系统策略拒绝脚本，说明实际原因并遵循环境授权，不能长期降低执行策略。

### F. Hook 审核、加载与可选实发验收

引导用户在 Codex 的 `/hooks` 或当前版本相应界面审核新增/变更的四类命令 Hook。新建/变更 Hook 默认需要信任；安装脚本不能代写信任哈希。根据客户端实际情况重载，必要重启需得到用户同意，避免中断其他会话。[官方 Hook 文档](https://learn.chatgpt.com/docs/hooks)

实际邮件检查需用户明确授权。先记录当前模式和免打扰设置，必要时临时改为 `always` 且避开免打扰，触发一轮正常对话并确认收件，再恢复原设置。确认普通 JSON 与正常子代理仍遵循通知策略，Respire 标准成功回执及长对话分块提炼输出记为 `suppressed_respire`。不要仅凭“未收到邮件”认定后台过滤成功。

## 5. 运行行为与排障

工具只负责通知，不批准、拒绝或继续 Codex 操作。`Stop` 不是业务成功证明；无进展检测是启发式提示。

- `auto` 在完成事件发生当刻判断离席；当时在场的旧事件以后不会补发。
- 免打扰优先于 `always`，跨午夜时段包含开始、不包含结束，抑制的事件不补发。
- 已确认 Respire 双目录和插件清单来源后，才静默标准 `result/ids` 成功回执或 `facts/constraints` 分块提炼输出；普通 JSON、正常子代理、未知来源、失败/中断和异常输出保留原通知策略。识别使用原始 Hook 输出，在邮件正文截短之前完成。
- 分块提炼要求严格 JSON、仅 facts/constraints 两数组且各不超过 24 项、总长不超过 6000 个 UTF-16 单元；fact 仅有 text/status/sources，constraint 仅有 text/sources。text 非空且不超过 1500 个 UTF-16 单元，status 为 confirmed/proposed/uncertain，sources 非空且格式为 item-数字-part-数字。重复字段和附加错误字段不静默。通知器只能验证来源格式，不能确认其属于原分块，也不确认提炼事实或记忆落库。
- `suppressed_respire` 表示后台回执或分块提炼输出被静默；`ignored_respire_background` 表示不为后台提交建立停滞监测。来源缺失或插件布局变化时保留通知，不扩大屏蔽。
- `background_filter` 诊断只记录来源判定和输出种类：`receipt`、`extraction`、`unrecognized` 或 `not_checked`，不记录正文。仍收到后台邮件时，先对照事件时间查看此分类；已出现 `ignored_respire_background` 或 `suppressed_respire` 说明过滤代码已运行，不应把漏匹配直接归因于需要重启。
- 待发 Stop 可暂存回复，待发 Stop/Interrupt 可暂存本轮提问；发送、抑制或最终失败后清空正文。不要将事件库或日志放进迁移包。
- 程序每次新 Hook 启动读取当前脚本；已运行的 worker/watchdog 不自动重载。不要擅自结束现有进程。

诊断顺序：确认客户端配置目录 → Hook 是否已信任/加载 → 路径与 Python 实际可运行 → 通知模式/会话范围/免打扰 → `notifier.py status` 和诊断日志中的安全错误分类。状态命令不证明真实邮件已经送达。

## 6. 回退与交付

安装器输出备份路径。需要回退时，agent 先比较当前文件是否有安装后新增改动；只还原本次实际替换的工具文件和通知器 Hook 项，保留后来新增的其他配置。不盲目覆盖整份最新 hooks.json、不删除整个目录，也不删除用户凭据。

写入按单文件原子替换，不是整次安装的事务。磁盘空间、权限或进程占用等错误可能留下部分进度；失败时检查目标文件旁的 `.bak-时间标记` 备份和当前状态，再决定重试或回退，不把异常后的目录直接判成完整安装。

最终报告至少包含：实际 Python、目标目录、Codex 目录；修改与保留项；隔离测试结果；Hook 信任/加载状态；SMTP 信息与授权码是否已配置；实发是否获授权并验证。存在剩余动作就列出，不能将“安装通过”写成“完全迁移验收通过”。

参考：微软 [WinGet install](https://learn.microsoft.com/en-us/windows/package-manager/winget/install)、[Python Windows 下载](https://www.python.org/downloads/windows/)、[Codex Hooks](https://learn.chatgpt.com/docs/hooks)。
