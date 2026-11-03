# M5-A 桌面设置交付证据

## 实现范围

- Windows Tauri 2 / Vue 3 设置窗口：总览、邮箱设置、工具接入、通知规则、通知记录、诊断六页。左侧导航和持续后台状态条；窄窗口、键盘焦点、减少动态效果由样式处理。
- 邮箱主流程为发件邮箱、授权码、收件邮箱；高级 SMTP 折叠。授权码只在显式保存命令中暂时传入 Rust，写入当前用户系统安全存储；现有授权码不返回页面。Outlook/Microsoft 365 缺发布者 OAuth 注册资料，界面明确禁用。
- `config.json` 和 `host.json` 保存前核对文件散列版本，并在当前用户安全目录中加锁。保存后仅通过本工具 ServiceManager 优雅停止、启动独立后台；报告区分“文件已保存”和“后台已采用”。窗口关闭不触发后台停止。未安装后台时设置保留并显示待生效。
- 集成预演通过同目录 `agentnotify.exe` 执行已限定命令，WebView 仅接收操作编号、散列、事件与状态；原客户端配置和备份字节不返回页面。应用接入需单独点击，真实信任/重载与客户端事件仍需用户确认。
- 通知记录使用只读 SQLite 查询，最多 40 条，仅显示状态、类型、时间、任务标签；诊断仅统计错误代码。Agent 协助包依赖任务09，当前明确不可用。
- 测试邮件先预演并绑定当前邮箱配置散列，再由用户单独点击发送；单次确认 120 秒失效。发送阶段落入本机 `test-send-state.json`，SMTP 接受和收件箱确认分开展示，结果不确定时不自动重发。

## 实际验证

| 项目 | 结果 | 范围 |
|---|---|---|
| `pnpm build` | exit 0 | `vue-tsc --noEmit` 与 Vite 生产资源构建；规则页 Proxy 克隆修正后重建 |
| `scripts/dev.ps1 build -p agentnotify-desktop -p agentnotify-cli -p agentnotifyd --release` | exit 0 | Windows MSVC release 三程序编译；未运行程序 |
| `scripts/dev.ps1 build -p agentnotify-desktop --release` | exit 0，40.11 秒 | 规则页修正后的前端资源嵌入 exe；`build.rs` 跟踪 `dist/` 变动 |

可运行文件（同目录）：`target/release/agentnotify-desktop.exe`（SHA256 `55301382CFFBE998E7B8E91F3F502ACF67B305E3B4DB5AEB6C6D8D42CE751CCD`）、`target/release/agentnotify.exe`（`EEDBB6A8DD7C0F7B8433B8146483BC143AC1F35EE2938B3402328FBEF283BB06`）、`target/release/agentnotifyd.exe`（`FB867E6EB1DFB6639B7D0F76EDA3C4B5E4FAF25C5B0444544291F2C2D51C8783`）。均为未签名开发构建；未制作安装包或验证运行环境的 WebView2 依赖。

未新增或运行测试；未启动桌面/后台、注册系统任务、应用真实客户端接入、读取真实凭据或发送真实邮件。没有 GUI 截图、收件箱确认和 Windows 实际操作验收；macOS/Linux 实机仍待办。

## 用户手动检查建议

1. 将 `agentnotify-desktop.exe`、`agentnotify.exe`、`agentnotifyd.exe` 放在同一目录，打开设置窗口，检查六页、键盘焦点与窗口缩放。
2. 使用自己的邮箱授权码私密输入并保存，检查显示“已保存/后台已采用”的准确状态；关闭窗口后检查独立后台继续运行。
3. 仅在愿意产生真实测试邮件时，先预览收件地址，再点击“发送一次测试邮件”，随后自行核对收件箱。
4. 仅在愿意修改客户端配置时，使用支持表中的准确版本预演，再单独点击应用；按页面提示自行完成信任或重载并观察真实事件。
5. 检查结果不确定、认证受阻、全天静默、未知任务标题等页面提示；不要把 SMTP 接受当作收件成功。
