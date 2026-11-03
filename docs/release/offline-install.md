# Windows 单 EXE 与离线使用

交付运行文件为 `AgentNotify.exe`，设置界面、CLI 和后台入口编译在同一个程序中，桌面静态资源嵌入程序；不自解压三个运行程序。双击直接打开设置。说明材料单独放在 `AgentNotify-docs.zip`，不包含二进制或启动脚本，也不是运行依赖。

单交付 EXE 不等于零运行数据或单进程。安装后仍有程序管理的版本文件、安装记录、配置、队列与当前用户登录任务；凭据保存在系统安全存储。已有安装可保留同一程序的稳定兼容入口与版本回退文件，不能为了“只留一个文件”删除它们。安装与客户端 Hook 的稳定路径不随下载文件位置改变。

默认入口应安全发现并复用用户 profile 下已有 AgentNotify 安装，多个安装需明确选择；不得为访问另一个目录而放宽 ACL。程序和 Agent 的所有入口必须指向同一数据目录。新环境通过程序重新安装、配置邮箱、选择客户端并接入，再预览并确认测试邮件；不需要源机器的迁移包，不复制源机器的运行目录或系统凭据。全新未配置实例可在总览选择通过安全校验的自定义数据目录，已有安装不隐式迁移；独立首次使用提示词可以直接复制给 Agent。排障包仅按需生成，路径以界面返回为准。

运行端无需 Python、Node、Rust 或 PowerShell 7。Windows 后台管理使用系统 PowerShell；GUI 需要 WebView2 Runtime。离线机器另行准备微软官方 Evergreen x64 Standalone Installer，核对来源与微软签名后安装。没有自动下载或已安装证明时不宣称运行时就绪。

参考：[WebView2 分发说明](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/distribution)与[官方下载](https://developer.microsoft.com/microsoft-edge/webview2/#download-section)。安装 Edge 浏览器不等于已满足 WebView2 Runtime 前提。

卸载入口位于“诊断 → 卸载 AgentNotify”，必须先预览范围再最终确认。默认只保留邮箱、通知偏好和相应授权凭据，历史队列与旧接入状态不保留；可另选清除本工具配置与凭据。未知备份或无法确认归属的文件保留并列入预览，有冲突或在途任务时阻止执行。独立清理启动不等于完成，按返回路径核对最终报告 `completed` / `incomplete`；不影响其他客户端，不递归清空不明文件。

当前是未签名开发构建。说明包的运行文件 SHA256 用于比对同次交付的 `AgentNotify.exe`，不证明发布者身份；说明包自身校验和另附。不得关闭系统安全策略或把开发密钥当正式信任根。macOS/Linux 待办。

维护者使用锁定 Rust/MSVC/Node/pnpm 工具链。`scripts/package.ps1 -Platform Windows -Channel Development` 负责构建并交付；只有已完成对应生产构建才使用 `-SkipBuild`。构建和打包成功不代表自然客户端事件或真实收件通过。`Release` 仍拒绝执行，直到正式信任根、签名与发布流程具备。

说明 ZIP 解压后，`meta/RUNTIME-SHA256.txt` 指向单独交付的 `AgentNotify.exe`，核对时选择该 EXE 的实际位置；它不是 ZIP 内文件列表。`meta/SHA256SUMS.txt` 则仅校验说明材料，路径相对说明解压根目录，包含 `meta/BUILD-INFO.json` 和运行文件哈希说明，不包含清单自身。发布脚本输出 EXE 与说明 ZIP 各自的 SHA256。

卸载验证范围：当前完成实现与隔离安全检查，完整 Windows 卸载及重新安装尚未实机验证。未知文件、备份和最初下载的程序文件会保留；仅在归属核对通过时自动清理当前用户桌面上本工具的精确快捷方式。不要据此手动删除其他文件。
