# M6 Windows 开发包交付记录

本记录是实施者的实际证据交接，独立静态审查由主代理组织，功能验收由用户完成。未标为正式发行或整机验收通过。

## 本轮产出

- `scripts/package.ps1`：三个Windows原生exe的生产构建/开发ZIP，桌面资源内嵌，静态CRT编译选项；`Release`通道拒绝执行。
- `packaging/windows/`：Windows PowerShell5.1兼容Start/Install/Uninstall入口、START_HERE及Agent说明。WebView2缺失只提供官方安装路径，不自动安装；不需Python/Node/Rust/PowerShell7运行环境。
- `platform/update.rs`、`queue/maintenance.rs`：签名+版本+逐文件hash验证，维护日志/一致SQLite backup/停止发送器后在途收敛/兼容schema2与spool1/按序重放/恢复消费后禁止旧快照回滚。缺编译期发布公钥，真实更新不可激活。
- 稳定Launcher对应安装记录可原子切换版本，Hook通过归属验证的稳定入口，桌面选择已安装版本。预演未安装返回needs_install；不写客户端信任。
- GUI保存与CLI实际接入变更有维护门禁；预演外部版本探测不占admission，防止阻塞Hook。
- 卸载只移除仍匹配归属的本工具登录任务；数据、系统凭据、稳定入口和Hook默认保留，避免覆盖用户改动。

## 实际验证

用户明确减少测试。本轮未新增或运行tests，也未启动GUI/daemon、安装真实服务、读取真实凭据、发送邮件或重启客户端/系统。

- 前端`pnpm.cmd -C apps/desktop build`（vue-tsc+Vite），最终前端修改后exit0。
- 原生命令：`scripts/dev.ps1 build --release --offline --locked -p agentnotify-cli -p agentnotifyd -p agentnotify-desktop --features agentnotify-desktop/custom-protocol`，进程RUSTFLAGS=`-C target-feature=+crt-static`。最终独立审查整改后exit0，CLI增量编译3.21秒。此前首次使用错误包名agentnotify-daemon导致命令exit1，修正为实际agentnotifyd后完整构建exit0；未执行测试。
- `scripts/package.ps1 -Platform Windows -Channel Development -SkipBuild`在上述构建成功后打包，exit0。
- `dumpbin /dependents`只读检查三个exe：依赖Windows系统DLL/UCRT API，不依赖Python、Node、Rust、外置AgentNotify DLL或开发VC运行库DLL；桌面仍需WebView2 Runtime。这不是干净机器运行证据。
- 系统Windows PowerShell `5.1.26100.9818`的Parser读取两个最终用户ps1脚本，exit0；未执行脚本正文。
- ZIP目录只读清单包含三个exe及说明/脚本/许可证/散列；SHA256非发布者签名。
- 精确路径暂存与cached whitespace检查，无commit/push。

当前交付包：`target/packages/AgentNotify-0.1.0-windows-x64-unsigned-dev-20261102-110144.zip`。
SHA256：`28b1cb3450dac377a1280c70868fe6892f94fc8021eea12c3c2ef7e13119575e`。
此前110001包是整改前过程产物，不作为当前交付。

## 明确未决

真实客户端/收件、登录休眠、升级并发与中断、卸载/重装等由用户按START_HERE试用。没有正式公钥/私钥签名流程和OS证书，不能发布正式更新。当前仅同schema2/spool1兼容更新；跨schema迁移/消费后降级没有实现而拒绝。维护结束后台保留停止，由用户显式启动。内置客户端尚无可靠Started能力登记链，看门狗保持禁用。macOS/Linux原生服务、安装与实机支持仍待办。旧工具自动停发/切换仍需迁移协助，不允许普通追加Hook双发。

新JSON契约为本机更新日志/签名清单，不改业务事件DDL/TS/Schema；新增GUI可用性响应在bridge使用相同available/code字段。Java/Excel字段映射不适用。
