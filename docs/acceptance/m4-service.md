# M4-A 独立后台与平台接通

状态：实现交付，Windows 编译完成；等待独立静态审查及用户实测。日期：2026-11-02。
用户要求“减少测试，实现功能后我来测试”优先于原任务单的TDD/回归步骤。本任务没有新增测试、没有运行测试、没有实际注册/启动服务、没有读真实凭据或发送邮件。

## 实现范围

- Windows 当前用户 LocalAppData 路径发现，严格ACL/所有者/reparse/本地固定ACL卷验证；根及祖先/子目录句柄持有期间禁止删除共享。陌生目录权限不安全时拒绝接管，不修复其ACL。不防同一受信用户恶意进程或管理员。
- 独立 `agentnotifyd.exe` 长期持有同一 Queue、Policy、sender 文件锁。GUI生命周期不参与后台进程管理；真实退出GUI行为仍待用户验证。
- 当前用户受限命名管道，拒绝远程客户端；仅 Health/Stop/Pause/Ingest 命令。中心接收及离线spool共享 ingress ID，ACK超时不能变成新的通知身份。离线客户端只写spool，不用自己的Policy runtime建立活动连续性；wake失败不撤销持久事件。
- 每200ms继续导入spool，包括上一轮remaining=0；15秒逐实例flush_digests/evaluate_due，60秒保留期维护。发送线程单封领取，持锁调用deliver_lease；SMTP不占用队列写事务，停止等待在途发送保存状态，不按进程名强杀。
- 新daemon runtime先挂起旧活动；spool重放保持保守挂起。Windows OS boot明确为unknown，独立连续运行证明来自电源回调epoch与双运行时间/UTC比较。Policy领取和DATA前通过中立RuntimeClockEvidence即时采样，避免SMTP线程沿用主循环旧快照。
- Windows用户计划任务使用本机Task Scheduler COM、当前用户interactive token、LUA、登录触发、无限运行时限及IGNORE_NEW。稳定launcher等待版本目录daemon；操作先校验安装身份、二进制hash、任务owner/DACL/principal/action/trigger。重复install不覆盖陌生文件或任务；uninstall保留数据及凭据。PowerShell输入/输出均UTF-8，路径通过JSON stdin传递。

## 实际编译证据

进程级设置CARGO_HOME/RUSTUP_HOME使用既有工具链，未修改全局PATH或工具链文件。

| 实际命令 | 结果 |
|---|---|
| `scripts/dev.ps1 check -p agentnotify-platform -p agentnotify-cli -p agentnotifyd` | 首次发现Windows句柄类型与常量导入问题；修正后exit 0 |
| PowerShell Parser.ParseFile(service_windows.ps1) | 语法解析无错误；不是服务运行验证 |
| `scripts/dev.ps1 build --release --locked -p agentnotify-cli -p agentnotifyd` | 初次及预审三项整改后增量构建均exit 0；最后12.53秒 |

最终构建文件（未运行、未签名、构建输出不入库）：

| 文件 | 字节 | SHA256 |
|---|---:|---|
| `target/release/agentnotify.exe` | 4623872 | C9A9811CCD33705C2E682EE57BF5CE64C9E2171E28075BF0C0E22A41DA1A2ABF |
| `target/release/agentnotifyd.exe` | 6585856 | 2DE6ED2CBAAAB55D44B6C92F8BC2DE8C130E4A202FCEE0E33854E694A84DFC18 |

## 接口与后续任务边界

- `crates/platform/src/runtime.rs`：HostSettings (`host.json`)包含可选content_key引用、DeliverySettings和已登记实例能力。DeliverySettings字段为host/port/username/tls/oauth/from/recipients/credential引用；不含密码或token。`config.json`仍使用既有ConfigV1。当前启动读取；配置保存冲突控制/CAS、安全reload及history/诊断读取由任务08接续，不能假称热更新完成。
- `crates/platform/src/ipc.rs`：Request::Health返回既有HealthReport；Pause只改变当前运行暂停状态；Stop优雅结束；Ingest返回真实EnqueueOutcome。暂停通知与停止后台分离，尚未提供永久暂停写入API。
- `apps/cli/src/host_commands.rs`：service install/start/stop/status/uninstall、doctor --json、notifications pause/resume、emit --stdin、generic Hook；厂商Hook解析仍归任务07。
- `crates/platform/src/service.rs`：ServiceManager、wake、wake_detached、launch；status不创建新数据根。安装操作由用户明确运行命令触发，本轮未执行。
- macOS/Linux仅路径发现；原生目录安全、用户服务与凭据后端未完成，返回pending/unsupported，不纳入Windows编译结论。
- SMTP预览/显式试信、UI配置、账号授权、历史页尚属后续任务。没有真实OAuth发布client_id、签名或实机收件证据。

## 用户实测清单（全部未执行）

1. 中文/空格用户路径的安全目录及计划任务输入输出；陌生同名任务、错误ACL、junction和文件替换均应拒绝。
2. 显式install/start后doctor正常；重复后台不能取得sender锁；窗口关闭后台保持运行；stop等待在途结束，uninstall只删本工具任务。
3. daemon离线时generic事件spool持久化、wake失败不丢；重启后持续导入；摘要到期发出、3秒/10秒聚合与真实邮件由用户明确配置后验证。
4. 重启、sleep/hibernate/自动恢复时旧活动挂起；恢复与SMTP DATA门槛竞争不发送旧stalled；下一可靠实时事件才能恢复计时。
5. 真实客户端信任、真实邮箱收件、登录恢复及最低Windows版本兼容，均未取得证据。

本记录不是独立验收通过结论，编译结果也不替代Windows平台与业务验收。
