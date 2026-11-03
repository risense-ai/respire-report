# M4-B 客户端适配与接入事务证据

日期：2026-11-02。实现范围：Windows x64 的 Codex、Claude Code、generic；WorkBuddy 仅 research_pending。此文记录源码和必要编译结果，**没有真实客户端加载、信任、Hook触发、客户端配置写入或邮件投递验收**。

## 可调用入口与保守语义

- `agentnotify integration inspect --json` 只读发现官方默认/环境变量路径；已登记实例还会核对当前配置中本工具 handler。未绑定版本只显示 detected/not_detected。WorkBuddy 始终 research_pending。
- `integration plan --request <json-file>` 接收 schema_version=1、adapter_id、instance_id、product_version、config_path、client_executable、agentnotify_executable 和 Codex 的 codex_home。只接受显式绝对路径、无符号链接、兼容清单中的精确 Windows x64 版本；对用户指定客户端二进制执行有界 `--version`，并核对当前 agentnotify 可执行文件。计划存于平台安全数据目录 `integrations/<operation-id>.json`，CLI仅返回哈希、事件和备份路径摘要，不输出原配置。
- `integration apply --plan <plan-file>` 只接受本安全目录生成的计划。配置文件 hash 在备份前及写入前复核；同一数据目录全局事务文件锁串行本工具 apply/rollback。只追加本工具 handler，保留其他 hook 键与数组；发生版本、现有绑定或 hash 冲突即拒绝。原字节备份位于安全数据目录。应用后 `verify --operation-id <id>` 核对文件哈希、登记与各事件的具体 handler；Codex最多是 trust_pending，Claude Code最多是 reload_pending，均不是 live_verified。用户通过客户端原生入口完成信任/重载。
- `integration rollback --operation-id <id>` 仅在配置仍与本计划的 after hash 一致时恢复备份或移除本轮创建的文件；恢复/删除前再核对一次。用户后续编辑导致冲突时保留文件，返回 `rollback_user_edit_preserved`，须重新检查并制定新回滚计划。未改客户端信任记录。
- `agentnotify hook codex|claude_code --instance-id <uuid>` 读取本机登记的实例/版本/配置/命令，按兼容事件解析后沿任务06队列持久化。Hook正常 stdout 为空，失败只写脱敏 stderr 且退出0；通用 `emit --stdin` 固定 caller_reported。CLI接入命令最终调用 `IntegrationManager`，UI后续可复用其公开方法与 serde DTO。

Codex Stop、SubagentStop及Claude Stop、SubagentStop映射 `response_finished/response_stop`，不称业务成功。Codex Interrupt 映射 interrupted；Claude StopFailure 映射 execution_failed；SessionEnd 仅生命周期关闭，SubagentStart 仅有可靠 agent_id 时形成独立 actor。Claude Stop 缺少 background_tasks/session_crons 保持 unknown，非空为 pending；不把未支持事件注册为进展。无官方深链依据时 resume_url 为空。可选 metadata 接口默认关闭，缺标签仍通知；核心不读客户端私库、转录或原生字段。

## Respire来源与协议

Codex Stop 才检查进程环境中的 `PLUGIN_ROOT`、`PLUGIN_DATA` 与登记的 `CODEX_HOME`：路径必须对应 `plugins/cache/rsrsai/respire/<version>` 和 `plugins/data/respire-rsrsai`，manifest 的 name/version 必须吻合。再在正文截断前按严格、全层拒绝重复键的 JSON 协议检查：回执仅有 `result,ids`，result 为 stored/already-stored/nothing-to-store/read-only 且 ids 是 UUID 数组，already-stored 非空；提炼仅有 facts/constraints，全文不超过6000 UTF-16码元，各数组最多24，条目严格限定 text/sources/(facts的status)，text非 ECMAScript trim 空且不超过1500 UTF-16码元，来源编码为 `item-[0-9]+-part-[0-9]+`。只有双重条件成立才标 `internal_background/success` 交给核心策略；普通同形 JSON、未知来源、失败、未知 schema、子代理均保留。路径证据用于避免误分类，不是抵御同用户恶意进程的认证。

## 兼容证据与限制

| 产品 | 清单版本 | 证据级别 | 当前结论 |
| --- | --- | --- | --- |
| Codex Desktop随包CLI | 0.166.0-alpha.5.1 | 本机公开二进制/schema + 正式文档 | 代码适配，未实际加载/信任 |
| Codex插件helper CLI | 0.165.0-alpha.1.2 | 本机公开二进制/schema + 正式文档 | 代码适配，不能代表Desktop运行实例 |
| Claude Code | 2.1.295 | 正式文档版本界限；本机PATH未发现客户端 | 代码适配，未本机验证 |
| generic | schema_version=1 | 本工具中立协议 | 需调用方实际接线 |
| WorkBuddy | 未确认 | 正式生命周期契约不足 | research_pending，不apply |

依据：[Codex Hooks](https://learn.chatgpt.com/docs/hooks)、[Claude Code Hooks](https://code.claude.com/docs/en/hooks)、本项目 `resources/compatibility/compatibility.json`。兼容清单只表示可计划的精确版本和证据等级，**不是客户端实测通过清单**。旧版脱敏协议依据为独立发布副本 `respire-report-python/notifier.py` 对应的已审查源码；未读取真实 facts、会话正文、凭据或信任库。

构建检查：`cargo check -p agentnotify-cli --offline` 退出码0，覆盖新适配器、集成crate及CLI类型连接。按用户要求未新增/运行测试。没有执行 inspect/plan/apply/rollback 实际事务，也没有启动客户端。程序级 Hook 全程2秒截止目前尚未证实，stdin/磁盘阻塞及客户端超时必须由用户实测；退出0本身不能保证宿主未超时。Windows实际加载、Codex信任、Claude重载、真实事件和邮件，以及 macOS/Linux 实机均待后续验收。
