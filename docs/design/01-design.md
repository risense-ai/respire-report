# AgentNotify 通用任务邮件通知工具：完整设计

版本：2026-11-02 / 产品契约更新。AgentNotify 是工作名称，正式名称和发布者标识在首次发布前确定。

本文件记录产品目标与设计契约；已有实现以源码和对应验收证据为准，未执行的平台或运行验证不因文档存在而视为完成。迁移专指 AgentNotify 从 A 机器迁至 B 机器。

## 1. 目标、范围与成功标准

用户安装一次，只需配置发送邮箱及授权码或官方邮箱登录、接收邮箱，以及免打扰等偏好。工具自动检测已安装的 AI 客户端、适配路径、配置接入、持久化通知、发送邮件并提供诊断。无法自动处理的环节，由工具生成一段完整提示词和脱敏执行包，用户复制给自己使用的 Agent 即可继续处理。

用户已确定的范围：

- 首版仅处理本机 Windows、macOS、Linux 上运行的客户端。远程服务器、WSL、容器、云端任务接收和手机端后续再考虑。
- Windows 先实施和验证；macOS/Linux 从架构、接口和代码组织上纳入，实机测试明确列为待办，不以交叉编译冒充通过。
- Codex、Claude、WorkBuddy 是接入目标。首批已找到正式事件依据的是 Codex 和 Claude Code；普通 Claude 聊天客户端与 WorkBuddy 桌面版不可由名称相近产品推定支持。
- 保留用户少操作的目标；邮箱私密授权、客户端要求的信任、系统权限等不能由程序或 Agent 伪造。

首版 Windows 发布门槛：一台干净的受支持 Windows 实机完成安装、邮箱配置、Codex/Claude Code 接入、GUI 关闭后通知、重启登录恢复、免打扰、故障恢复、升级回滚和卸载保留数据验证；至少一组真实邮箱投递有用户明确授权和实际收件证据。适配器没有真实客户端证据时，仅显示“已实现，待实测”，不能进入“已验证兼容”清单。

## 2. 已有基础与方案选择

AgentNotify 采用独立的 Rust 核心、工具适配器和桌面设置界面；去重、隐私清理、免打扰与后台来源识别均由本产品自身维护。

| 方案 | 优点 | 代价与取舍 |
|---|---|---|
| Rust 核心 + Tauri 2 + Vue 3 | CLI/后台不依赖 Python；跨平台业务规则统一；设置界面易维护 | 需要重写核心、做真实跨平台验证；用户已选此方案 |
| 打包 Python + Tauri/Vue | 能较快复用一部分逻辑 | Windows 专用部分仍要移植，双运行时与分发更复杂；不作为正式路线 |
| 云端集中通知 | 可覆盖设备关机后的远程任务 | 引入账号、服务运维和隐私边界，超出本机首版范围 |

选型：Rust Cargo workspace、Tauri 2、Vue 3 + TypeScript + Vite、SQLite、本机系统凭据库、SMTP。精确依赖版本在 M0 兼容性小实验后锁定到 `Cargo.lock`、前端锁文件和 `rust-toolchain.toml`，不使用浮动版本交付。候选库为 serde、clap、rusqlite、lettre、keyring、tokio；必须验证目标平台 feature 和维护状况后再定，不把候选名当作已验收依赖。

## 3. 用户流程与页面

### 3.1 首次使用

1. 安装程序检查操作系统、架构、系统 WebView 及受支持客户端。用户无需安装 Rust、Node.js 或 Python；开发工具链只属于开发机器。
2. 邮箱页填写发件邮箱、接收邮箱。按服务商预设填 SMTP 主机/端口/TLS，授权码在本机安全输入框输入；OAuth 邮箱打开官方授权页面。
3. 选择通知偏好。首次默认不包含提问和回答正文，可单独开启；迁往新机器时保留可携带偏好并展示平台能力差异。
4. 接入页显示检测到的工具实例、版本、配置目录和支持状态。用户选择“接入”，程序先预演，再合并属于自己的配置，不覆盖其他 Hook。
5. 如客户端需要信任或重载，展示精确说明和状态，不替用户写信任记录。必要的系统权限由系统标准交互获得。
6. 用户点击“发送测试邮件”才发送明确标记的测试邮件；收件确认作为独立步骤。单纯保存配置不发信。

页面：总览、邮箱设置、工具接入、通知规则、通知记录、诊断与 Agent 协助。界面不展示内部工作流角色、模型协作信息或复杂技术术语。高级参数折叠；错误用“原因、已完成项、下一步”表达。

### 3.2 日常运行

GUI 只负责设置和状态。独立后台随当前用户登录运行，负责队列和重试；GUI 的“关闭窗口”不等于停用通知。“暂停通知”“退出界面”“停止后台”是不同操作，必须明确表达。

Hook 快速落盘后返回，不等待 SMTP。后台未运行时，Hook 可通过平台启动接口尝试唤醒同用户后台；无法唤醒时保留待处理记录并诊断。完全退出 AI 工具不再产生新事件；关机、休眠或停止后台期间不能承诺实时邮件，恢复后按 TTL 和免打扰规则处理。

## 4. 架构与进程职责

Windows 本轮采用单运行文件 `AgentNotify.exe`：GUI、CLI 与后台由同一编译产物按参数分派，可作为不同进程运行，不自解压三程序。下面列出的组件名表示代码职责，不再表示三个交付 EXE。说明 ZIP 单独分发，不包含脚本或程序；安装数据和兼容 launcher 由程序管理。

```text
AI 客户端正式事件
  -> 客户端适配器（读取 stdin，识别实例与事件）
  -> Rust 入队器（校验、去重、来源分类、快速持久化）
  -> SQLite 队列
  -> 独立 Rust 后台（策略、调度、邮件构建、投递）
  -> 用户邮箱服务商 -> 收件邮箱

Vue/Tauri 设置界面 -> 受控 Rust 命令 -> 配置、系统凭据库、接入管理
诊断与失败状态 -> 脱敏 Agent 协助包 + 一键复制提示词
```

- `agentnotify`：CLI，可无图形环境运行；包括 hook、doctor、integration、service、test、agent-bundle 命令。
- `agentnotifyd`：单用户单实例后台，不托管 AI 任务，不启动模型，不承担业务验收；处理事件驱动的可选看门狗，SMTP 凭据仅在需要投递时从系统凭据库获取。
- `AgentNotify`：Tauri 桌面程序；不把 SMTP 授权码注入网页存储、日志、启动参数或导出的诊断包。
- 安装器：安装已编译二进制与 UI 资源、校验包、注册用户级后台入口；不默认以系统管理员身份运行所有后续操作。
- 首版不提供公网 HTTP 接收服务。GUI 到核心使用受控 Tauri 命令；CLI/后台共享持久队列，唤醒不依赖公开端口。后续 IPC 若有新增必须保持当前用户访问边界。

## 5. 工具适配与能力分级

每个实例由 `adapter_id + instance_id` 标识，支持同机多个安装/配置目录。配置路径采用显式选择、受支持环境变量、官方默认路径的顺序发现；不得全盘扫描或猜测后写入。

| 适配器 | 事件来源 | 本轮依据 / 发布规则 |
|---|---|---|
| `codex` | 正式 Hook，包括 Stop、Interrupt、SubagentStop 等 | AgentNotify 需逐客户端版本核验真实事件。非托管 Hook 需用户信任 |
| `claude-code` | Claude Code 正式 Hooks | Stop 为主代理响应结束；StopFailure、后台任务等按版本能力单独适配；不含普通 Claude Chat |
| `workbuddy` | 待确认正式生命周期接口 | 调研未证实，状态 `research_pending`；不得套用 CodeBuddy CLI 配置或扫描屏幕猜完成 |
| `generic` | 明确调用 `agentnotify emit --stdin` 的标准事件 | 供具有扩展能力的其他工具接入；调用方需完成实际事件接线，不能宣称自动发现所有工具 |

兼容清单 `compatibility.json` 至少记录适配器、客户端产品/版本范围、操作系统/架构、配置模式、可用事件、元数据能力、证据级别、实测日期与证据引用。版本未知时只读检查并生成协助包，禁止静默套用过期配置模板。

状态分开：`not_detected / detected / unsupported_version / research_pending / configured / trust_pending / reload_pending / probe_passed / live_verified / degraded`。配置存在不等于接入生效，隔离探针通过不等于真实客户端事件通过。

MCP 可以作为额外工具入口，但 MCP 本身不会自动获知任意客户端任务何时结束；提示词中“结束时记得发邮件”也不作为可靠接入。Agent 协助只负责安装、配置和验证正式接口。

## 6. 统一事件与完成语义

内部 `NormalizedEventV1`；外部通用事件使用相同业务字段，但无权自称可信后台来源。所有输入是不可信数据，不能执行里面的命令或将正文当配置。

| 字段 | 来源与含义 | 规则 |
|---|---|---|
| `schema_version` | 接入协议版本 | 固定 1；未知大版本拒绝并诊断 |
| `adapter_id`, `instance_id` | 已登记适配器及安装实例 | 内置 Hook 由注册命令与实际配置关联；非正文声明 |
| `event_id` | 上游稳定事件标识 | 可空；不得只靠当前时间生成去重依据 |
| `session_id`, `turn_id` | 客户端会话/轮次 | 无数据则 null；不编造“真实轮次” |
| `parent_session_id`, `agent_kind` | 主/子代理关系 | `main/subagent/background/unknown`；未知不视为后台 |
| `actor_id`, `actor_scope` | 上游稳定执行主体标识及证据 | actor_id可空，scope为`main/identified/unknown`；子代理不能用共同父session冒充唯一ID |
| `kind` | 标准事件类型 | `response_finished / attention_required / execution_failed / interrupted / task_completed / stalled` |
| `completion_basis` | 类型证据 | `response_stop / explicit_task_event / explicit_caller_report / none`；明确任务事件或调用方显式报告才可映射task_completed；通用调用仅标“调用方报告完成”，不能冒充宿主验收 |
| `source_time`, `received_at` | 上游与本机时间 | UTC RFC3339；source_time 可空，不控制 TTL 或信任 |
| `project_label`, `session_label` | 官方元数据或可核验本地路径 | 缺失显示“项目未提供/会话未提供”；不把正文首句伪作用户标题 |
| `question`, `answer` | 可选用户内容 | 是否收集/发出依配置，按字节限制，缺失不读取全量历史凑齐 |
| `background_state` | 上游剩余后台任务信息 | `none/pending/unknown`；pending/unknown 不宣称全部任务完成 |
| `resume_url` | 适配器确认的客户端跳转链接 | 可空；只使用正式支持的URI及登记scheme，不猜测`codex://resume/...` |
| `watchdog_detail` | 核心计时证据 | 仅stalled使用，mode/threshold_seconds/elapsed_seconds/last_observed_at；其他事件为空 |

最大原始 Hook 输入 1 MiB；上限错误只输出脱敏诊断，不阻塞 AI 工作。正文收集关闭时不落盘正文；开启时每项最多 16 KiB UTF-8，不破坏字符边界，邮件标注截断。Respire 协议判定使用截断前输入，继续严格遵循其 UTF-16/空白规则，不能用截断后的字符串匹配。

客户端ID字段各最多256个UTF-8字节，标签各最多512字节，instance_id为本机登记UUID；不截断ID造成碰撞。无正文元数据序列化上限8 KiB，超限返回明确输入错误。内部生命周期使用 `LifecycleSignal { instance_id, session_id, actor_id: Option<String>, actor_scope, turn_id: Option<String>, upstream_id: Option<String>, kind: Started|Progress|Resumed|SessionClosed, received_at }`；适配器返回 `AdapterOutput { notification: Option<NormalizedEventV1>, lifecycle: Option<LifecycleSignal>, source_evidence: SourceEvidence }`。生命周期信号用于generation、延迟取消与看门狗，不产生完成邮件，不把开始/进展伪装成结束事件。SessionClosed是明确的instance/session范围终止信号，仅关闭该会话活动记录，不将所有actor视为成功完成。

`SourceEvidence { source_class: user|subagent|internal_background|unknown, classification_reason: String, origin_evidence: verified_adapter|caller_reported|unknown, background_result: success|other|unknown }`由适配器侧受控分类器生成，作为通知和纯生命周期的共同上下文随Envelope持久化。来源字段不在notification另存一份，避免漂移。core只消费枚举，不识别原生字段或协议正文；reason仅用于诊断显示。纯Started可据verified_adapter/internal_background跳过看门狗；结束通知必须同时具备verified_adapter/internal_background/success才允许默认静默，后台错误和未知结果仍保留。外部generic输入不可自行填verified_adapter或伪造该证据，generic适配器固定caller_reported并拒绝保留字段。

统一执行主体key：可靠主代理使用instance/session/main；子代理使用instance/session/上游actor_id。actor未知时不跨事件聚合、不取消其他事件、不按父session/turn永久去重；仅在可证稳定event_id相等时去重，不用相同正文推断同一主体。generation、延迟取消、聚合和生命周期均使用同一主体key，主代理Progress不能取消子代理结束，子代理A也不能取消B。

`response_finished` 邮件名称为“本轮处理结束”，不承诺业务成功。等待输入/批准与失败/中断只有获得对应客户端可靠事件时才启用；未支持类型在 UI 置灰说明。子代理结束独立显示，默认保留，可关闭或聚合；不得因为是 JSON 或子代理就一概过滤。

Codex Stop 可以被其他 Hook 继续任务，Claude Stop 时也可能有后台任务。内置适配器将结束事件延迟 3 秒待发送，期间同轮继续或新进展可取消；这只是降低噪声，不能证明未来不会继续，邮件文案仍限定为响应结束。临时无法获得最终状态时，按已观察事件发送，不改称“全部完成”。

同会话10秒聚合启用时，实际最早发送时刻为 `max(最后结束事件接收时间+3秒, 本组首条事件接收时间+10秒)`；聚合关闭时仍保留3秒等待。不得合并不同session/instance/agent标识；聚合只降低邮件数量，不改变每个事件的原始状态证据。

## 7. 过滤、去重与策略

### 7.1 后台过滤

顺序：客户端/实例识别 -> 来源证据 -> 协议匹配 -> 分类 -> 用户策略。Respire 成功回执和提炼输出只有在可核验插件来源与严格协议同时成立时静默；插件路径本身不是对恶意同用户进程的安全认证，只用作避免误分类的运行证据。未知来源、错误、未知 schema 均保留，不因带 facts/result 等字段过滤。

过滤插件规则版本化；规则缺失时保守通知，记录 `unknown_background_protocol`。诊断只记录原因码和必要 ID，不记录正文、提炼 facts 或授权信息。规则升级必须包含普通同形 JSON、子代理、错误回执、Unicode 边界回归。

### 7.2 去重

- 优先 `adapter_id + instance_id + event_id`。
- 无 event_id 时使用稳定 instance/session/actor/turn/kind；一个可靠识别主体轮次的重复结束只通知一次。
- turn_id 缺失：在入队事务中按同一可靠actor key维护本地generation；只有官方开始事件才递增，重入开始按可用上游标识去重。没有可靠开始事件但actor已知时，仅以 actor key/kind/正文摘要作10秒短窗精确去重；actor未知时只能按可证稳定event_id去重。不得永久合并所有未知轮次，也不把短窗策略标为可靠轮次去重。
- SQLite 唯一索引和事务保证并发入队；不同客户端、不同实例、不同子代理不得互相误去重。

### 7.3 默认规则（新装）

- 开启响应结束通知；失败/中断默认开启，前提是对应适配器支持；待批准/等待输入默认关闭，可单独开启。
- 子代理通知默认开启，以延续当前需求；可按客户端关闭。可信内部后台成功输出默认过滤。
- 免打扰默认 23:00–07:30，使用系统本地时区，可选择固定 IANA 时区；支持跨午夜，起止相同表示全天免打扰且 UI 必须明确提示。UTC 存储，按当前时区规则判断，不用固定小时差处理夏令时。
- 默认免打扰期间直接跳过、不补发；可选“结束后摘要”，仅存计数与必要标签。摘要按实际静默区间实例生成唯一 key，夏令时重复时间不重复发送，跳过时间采用下一个实际有效时刻。
- 同时检查事件接收时和发送时的免打扰状态：任一命中时不发送即时邮件。已排队邮件若发送时进入静默区间，按当前 skip/digest 设置处理；修改规则不复活已经跳过的事件。
- 首版“离席才通知”非通用默认；Windows 提供空闲/锁屏可选能力，mac/Linux未验证时明确禁用，不静默降为始终通知。
- 看门狗新装默认关闭；用户开启后默认`elapsed_without_finish`、1800秒，只有适配器提供可靠活动事件时才可选择`inactivity`。两个模式文案与语义分别定义于§17，不能迁移时不提示就把“无进展”改成“总耗时超时”。
- 按相同实例/会话 10 秒聚合正常结束消息；不同会话不混淆。错误邮件默认独立；全局每分钟上限 10 封，超出以一分钟摘要排队，重试不绕过配额。

## 8. 队列、恢复与投递语义

SQLite WAL + busy timeout 1 秒；入队目标小于 300ms，预算上限 2 秒（需 Windows 实测）。数据库短暂不可写时，将规范化且已按隐私规则处理的事件写到当前用户专用 spool，随机文件名、临时文件原子改名；后台重新导入并按同一幂等键去重。每个 spool <= 64 KiB，总量 <= 16 MiB；目录不跟随符号链接，拒绝其他用户可写目标；无法持久化即记录有界错误，不假装接受。

spool采用版本化二进制封装：固定头、最多8 KiB元数据JSON、可选正文AEAD密文和nonce/tag；正文不作base64或嵌套JSON膨胀。两项正文合计最多32 KiB，剩余空间容纳封装；最终仍检查64 KiB总长。格式损坏或未知版本隔离，不执行其中内容；无安全内容密钥时只可接收无正文事件，并向用户显示正文未采集的原因。

DB与spool的共同输入是 `IngressEnvelopeV1 { ingress_id: UUID, received_at, per_instance_sequence: Option<u64>, adapter_output: AdapterOutput }`，生命周期和通知必须作为一个接收单元持久化，不能只保留邮件事件。每实例序号在当前用户入站admission互斥下由独立小型序号文件原子递增，不依赖业务DB；写入者持有此互斥直到DB提交或spool原子落盘，后台重放也遵循同一admission协议。

正常重放按序号、入站ID去重，事务记录checkpoint；已提交部分不再应用Started/Progress，迟到生命周期不能追溯重写已处理通知的身份。无法可靠分配序号时，可用UUID写入标记order_unknown的应急spool，恢复后不推测generation、不跨事件取消，仅按可证event_id去重，并在health展示顺序证据降级；未写入任何介质时明确失败。上游本身乱序且无可靠序列时也遵循此保守降级，不宣称已恢复真实执行顺序。

同实例一旦有未消费spool积压，后续接收继续写spool，不能因DB恢复而让较晚事件先应用生命周期/进入发送。后台按序号短批次导入，批次结束释放admission锁；批次预算不超过100ms，无法满足时保留积压让Hook快速落盘。确认无更早积压后才在互斥内切回直接DB模式；发送器仅消费已按序导入并通过checkpoint的事件。缺序号但序号分配已提交且无对应事件时记录gap，不凭缺口无限等待；隔离并走order_unknown保守规则，不跨缺口推断generation或取消。

Hook 对宿主永远不返回阻止/继续执行指令；适配器输出严格遵守该宿主协议。入队失败在本地 health 显示，退出/输出方式不能阻塞任务；通用 CLI 可用非零码告知调用方失败。

状态：`queued -> leased -> sent`，分支 `suppressed / digest_pending / retry_wait / auth_blocked / delivery_unknown / expired / permanent_failed`。领取采用事务与 60 秒租约；后台同用户单实例锁，异常退出可恢复未开始投递的租约。SMTP 网络操作不持有数据库写事务。

重试策略：明确未发送的网络/4xx临时失败按 30秒、2分钟、10分钟、30分钟退避并加最多20%随机抖动；最多初次加4次尝试，最长保留24小时。认证错误进入 auth_blocked，用户修复后重排且不越过 TTL；永久5xx进入 permanent_failed。恢复后重新检查免打扰、开关及TTL。所有计时使用受控时钟接口，测试无需真实等待。

SMTP DATA 后连接丢失或进程崩溃存在“服务商可能已收信，但本机未确认”的窗口。持久化发送阶段，将此类事件标记 delivery_unknown，默认不自动重发，允许用户显式重试并提醒可能重复；稳定 Message-ID 只能辅助关联，不能保证收件端去重。不承诺端到端 exactly-once。

发送阶段记录 `prepared/connecting/envelope/data_started/accepted`。必须在开始DATA前持久化data_started；恢复时data_started且未记录accepted的租约保守归为delivery_unknown。SMTP库必须能暴露所需阶段或提供可审计封装；M0若证明不能实现，不得继续用普通“发送失败就重试”替代。

sent 只表示 SMTP 服务商接受；“用户确认收到”是另一证据字段，不将服务商接受当作收件箱已投递。

新装正文默认不收集。开启后仅为投递暂存，终态立即清除，未完成记录最长24小时；元数据默认保留30天。SQLite/WAL 不作为安全擦除保证；含正文模式使用当前用户系统凭据库保管的内容密钥对正文和spool敏感字段加密，终态删除记录并按保留期做数据库维护。不以删除正文列宣称磁盘不可恢复。无可用安全存储时默认仅无正文事件，不退回明文密钥。

## 9. 配置、凭据与邮箱接入

配置分三类：可导出非敏感设置；只在本机维护的实例路径和服务注册；系统凭据库中的授权码、OAuth token及内容密钥。真实邮箱地址属用户数据，诊断默认掩码；用户主动导出配置时需说明包含邮箱地址，凭据永不导出。

新装关键配置：`enabled=true`；`include_question=false`；`include_answer=false`；`quiet.enabled=true`；`quiet.start=23:00`；`quiet.end=07:30`；`quiet.timezone=system`；`quiet.behavior=skip`；`metadata_retention_days=30`；`queue_ttl_hours=24`。JSON 配置带 schema_version，原子保存与版本检查；Rust 模型导出 JSON Schema/TS 类型，防止 UI、DB、CLI字段漂移。

看门狗配置：`watchdog.enabled=false`；`watchdog.mode=elapsed_without_finish`；`watchdog.threshold_seconds=1800`，有效范围60–86400秒；tick固定15秒。适配器能力与上述开关共同决定可用性，不能仅打开UI开关就假定客户端已提供开始/进展事件。

邮箱服务商预设是版本化的资料，安装时联网更新必须校验发布者签名；MX 查询不能可靠推导SMTP发送服务器。常见 QQ/163/Gmail 应用密码方式按服务商条件支持；Outlook.com 等要求现代认证时走 OAuth。企业邮箱提供高级 SMTP 主机/端口/TLS/用户名选项，不声称所有邮箱永远只需三项。

SMTP只允许隐式TLS或STARTTLS，不回退明文认证，不关闭证书验证；正文以纯文本为首版，所有标题拒绝CR/LF注入，不自动附带文件。链接仅允许受支持安全scheme，邮件不会执行模型输出中的命令。

OAuth采用系统浏览器、Authorization Code + PKCE、state/nonce与短生命周期回调；桌面程序是公共客户端，不内置可被当作保密凭据的client secret。发布者注册应用/client ID、权限及服务商审核是开发交付责任，不转嫁普通用户；未取得注册或被企业策略阻止时显示能力不可用，不能改成索要账号密码。刷新token只在系统凭据库。

Windows 使用当前用户 Credential Manager/适当DPAPI封装；macOS使用Keychain；Linux使用Secret Service等受支持凭据后端。库接口统一但平台行为必须分别验证。Linux没有可用且已解锁的安全存储时暂停邮箱认证，展示启用凭据库的 Agent 协助路径；不自动安装未知守护程序、不明文保存授权码。后台取凭据失败记 auth_blocked，不弹出循环提示。

实现必须固定真实平台后端；测试mock只能存在测试构建，不得作为生产fallback。保存凭据后由核心完成不回显的读回验证，区分缺失、锁定、拒绝访问和后端不可用，成功后才允许开启投递。keyring不同主版本的后端配置方式不同，按锁定版本验证，不能照搬其他版本feature。

## 10. 平台、安装、启动与发行

| 平台目标 | 首版工程安排 | 实测状态 |
|---|---|---|
| Windows 10 22H2/Windows 11 x64 | 首个原生安装器、用户级后台、完整接入验证；实际最低版本以依赖与实测收窄 | 本轮未实测新产品；实施时先记录当前机器OS/build |
| macOS 13+ arm64/x64 | 平台模块和构建配置、Keychain、LaunchAgent、签名/公证方案 | 待办；没有Mac实测不得标已支持 |
| Ubuntu 24.04 LTS x64 | Linux首个明确目标，Secret Service、systemd --user、WebKitGTK依赖 | 待办；其他发行版/桌面环境后续纳入 |

这是一份目标矩阵，不等于现成兼容承诺。“支持Linux”不等于所有发行版、无桌面环境、所有CPU架构。无GUI核心可单独构建，完整桌面安装仍有系统WebView依赖；Windows安装器可处理WebView2，Linux依赖按正式包管理方式处理，不能把开发依赖全部要求终端用户安装。

后台启动：Windows当前用户登录任务；macOS LaunchAgent；Linux systemd用户服务。未提供这些能力的环境返回不支持原因和Agent方案，不默默使用高权限系统服务。程序路径和数据路径独立；稳定Launcher路径供Hook调用，版本二进制放版本目录，升级原子切换，避免每次升级改Hook命令导致信任重置。

非敏感配置和数据遵循平台目录，使用平台API发现，不拼接用户名字：Windows LocalAppData/AgentNotify；macOS Application Support/AgentNotify；Linux XDG config/data/state对应目录。密钥不放这些目录。

安装包各平台分别构建、校验、签名。更新清单使用内置发布公钥验证签名与版本约束，HTTPS和SHA256校验不能替代真实性校验；Tauri更新签名与Windows/macOS操作系统代码签名是两项不同工作。密钥只存在发布环境，不进仓库或前端资产。离线安装提供可信来源包和校验说明。

自动更新按包格式分别设计：Windows NSIS/MSI、macOS app归档、Linux AppImage按Tauri支持范围验证；deb/rpm走各自包管理渠道，不承诺复用AppImage更新流程。首版没有可靠签名发布渠道时可仅提供签名安装包手动更新，UI明确自动更新未开放，不接受未签名替代包。

升级先停止本工具发送器、等待在途投递结束或标记不确定、备份配置/数据库、执行兼容迁移、健康检查后切换。失败恢复旧二进制及一致数据快照，避免旧版本读取新schema；不自动重启Codex/Claude等客户端。更新只重启本工具组件，GUI提示进行中状态。

具体顺序：升级器通过admission互斥设置持久maintenance标记，然后停止发送器并完成在途状态记录，再用SQLite备份API取得一致快照。标记期间所有Hook（含LifecycleSignal）仅写兼容的IngressEnvelopeV1 spool，不写业务DB；旧GUI写配置同时被拒绝。完成迁移、切换和健康检查后，在admission互斥下导入积压，再开放正常入队；过程中新事件继续spool。失败先恢复旧二进制与一致快照，保留全部spool后按同样规则恢复，不能把升级期间事件随快照覆盖丢弃。

升级/回滚前验证旧版、新版均能读取待重放spool格式及快照schema，无法兼容则拒绝自动更新并生成Agent协助包。maintenance写入和解除带操作日志，进程崩溃后进入恢复流程；不得简单删标记绕过未完成迁移。导入幂等依据入站ID和来源事件ID，两者不能被快照恢复重置。

上述快照恢复仅适用于尚未恢复业务消费的升级事务。一旦新版本已恢复发送，后续降级必须保留最新事件、投递结果和幂等记录，采用已验证的兼容迁移；没有兼容路径则阻止自动降级，不能把运行若干天后的数据库恢复成升级前快照。

卸载只移除自己持有且仍匹配安装记录的配置段；用户后改内容留给人工/Agent裁断。数据和凭据默认保留，另设显式清除动作。重装幂等，不重复建立服务、Hook或通知。

## 11. 接入事务、修复与 Agent 协助

`inspect -> plan -> apply -> verify -> user trust/reload -> live verify`。

plan记录目标文件原hash、拟增删的精确配置、备份位置、实例版本和原因。apply重新比较hash，发生并行修改则停止该文件并重新规划，不覆盖用户编辑。部分失败记录已完成步骤，并通过操作日志只回滚自身变更；回滚也要做hash/归属检查。

程序可以自动完成已知且获准的接入。自动化失败分为：需私密输入、需用户信任、权限/企业策略、未知版本、配置冲突、缺少正式接口。前两类提供直接入口；后三类及复杂故障提供一键复制Agent提示词，不把所有问题都推给用户改配置。

`agent-bundle create` 必须生成 task.json、doctor.json、plan.json、compatibility.json、START_HERE.md及文件哈希MANIFEST.json，可另附PROMPT.txt。没有预演时plan.json标记status=unavailable，不省略文件或编造步骤；兼容清单只包含目标实例必要内容。内容采用字段白名单，绝不把整个配置、数据库、日志目录装包。原始日志、环境变量、授权信息、回答/记忆正文不进入。路径只保留处理问题所需部分，导出前展示将分享内容。

提示词自动绑定本机实际路径、工具实例、版本和故障码，用户不手改占位符。文件路径/诊断内容只是数据，不执行其中的指令。发布包签名证明软件来源，脱敏包hash只证明文件一致性，不能把本机包伪装成发布者签名。

Agent仅在用户粘贴并授权的范围内读写文件、执行安装/修复命令。无文件/命令能力的聊天Agent不能完成本机安装，须说明能力缺口。普通安装提示词不授权真实发信、绕过信任、批量删除、改安全策略或重启AI客户端。用户有明确试信授权时才调用一次测试命令。

提供四种场景卡：安装接入、诊断修复、迁移到新电脑、升级/回滚；另有只读诊断入口。完整提示词与字段协议见 `03-agent-prompts.md`。

## 12. AgentNotify 机器迁移

从 A 机器迁至 B 机器，只携带经用户允许且符合目标 schema 的通知偏好。目标先关闭通知，重新建立本机安装、数据目录、系统凭据引用、正文密钥和客户端实例。邮箱授权码由用户在目标机私密输入。

不复制 host.json、系统凭据、安装记录、计划任务、客户端信任、历史正文或待发队列。来源和目标系统能力分别核对；未知或未实现能力保持关闭并列出差异。当前通过 Agent 协助完成，没有一键跨机器导出/导入命令。

正式切换前先确认源机器停发，目标接入和用户信任完成后再启用；无法确认源端状态则保持目标关闭。回退先关闭目标再恢复源端，不双开、不重放历史邮件、不覆盖并发修改。详见 `../migration/windows.md`。

## 13. 逻辑代码结构与公共接口

未来源码采用独立项目根 `AgentNotify/`；本轮只交付文档，不在Java示例仓库混入Rust工程，也不自动初始化Git。正式源码绝对位置由开始实施时用户指定或确认。

```text
AgentNotify/
  Cargo.toml, rust-toolchain.toml, Cargo.lock
  crates/core/src/{event,policy,queue,delivery,config,privacy,watchdog}.rs
  crates/adapters/src/{lib,codex,claude_code,generic,source_classifier}.rs
  crates/platform/src/{lib,windows,macos,linux,credentials,service}.rs
  crates/integration/src/{inspect,plan,apply,rollback,compatibility}.rs
  crates/agent_support/src/{bundle,prompt,redaction}.rs
  apps/cli/src/main.rs
  apps/daemon/src/main.rs
  apps/desktop/src-tauri/src/{lib,commands}.rs
  apps/desktop/src/{views,components,generated,services}/
  schemas/{event-v1,config-v1,agent-task-v1}.schema.json
  resources/{providers,compatibility}/
  tests/{fixtures,contract,integration,platform}/
  packaging/{windows,macos,linux}/
  docs/{design,plans,agent,compatibility,acceptance}/
```

接口约定：`Adapter::normalize(raw: &[u8], context: &AdapterContext) -> Result<AdapterOutput, AdapterError>`；`Ingress::accept(output: AdapterOutput) -> Result<EnqueueOutcome, QueueError>`负责admission/序号/DB或spool；`Queue::ingest(envelope: IngressEnvelopeV1) -> Result<EnqueueOutcome, QueueError>`负责事务内生命周期与通知；`Policy::decide(event: &NormalizedEventV1, evidence: &SourceEvidence, config: &ConfigV1, now: DateTime<Utc>) -> PolicyDecision`；`Watchdog::observe(tx: &Transaction, envelope: &IngressEnvelopeV1) -> Result<(), WatchdogError>`；`Watchdog::evaluate_due(tx: &Transaction, clock: &ClockSnapshot) -> Result<Vec<NormalizedEventV1>, WatchdogError>`；`Transport::send(mail: &PreparedMail, credentials: &CredentialHandle) -> DeliveryOutcome`；`IntegrationManager::plan(request: IntegrationRequest) -> IntegrationPlan`；`apply(plan: &IntegrationPlan) -> ApplyReport`；`rollback(operation_id: &str) -> RollbackReport`。类型字段以Schema/生成文件为单一契约，不在UI各自猜测。

规划CLI（尚未实现）：`agentnotify doctor --json`、`integration inspect|plan|apply|rollback`、`hook <adapter> --instance-id <id>`、`emit --stdin`、`service install|start|status`、`agent-bundle create`、`test --dry-run`、`test --send`。敏感参数只能经本机安全输入/API引用，禁止`--password`。命令最终参数在M1协议任务固定，并同步提示词和帮助。

## 14. 验证、风险与发布门禁

测试层次：纯逻辑单元/属性测试 -> 协议和文件样本测试 -> 隔离文件/临时数据库/模拟SMTP -> Windows真实客户端/重启/邮箱 -> macOS/Linux实机待办。CI编译只是构建证据，模拟SMTP只证明程序行为。

关键场景：普通同形JSON不误过滤；子代理/主代理区分；可信Respire两阶段输出；未知来源保留；重复Hook与并发会话；UTF-16和UTF-8边界；时区/DST/跨夜；数据库锁与磁盘满；断网、认证失效、SMTP模糊结果；后台崩溃与重复启动；配置并发修改；升级中断；恶意标题/符号链接/路径含中文空格；Agent包泄露及提示词注入。

每项实测记录日期、OS/build/架构、客户端版本、软件版本/hash、步骤、预期、实际、证据、剩余限制。实际邮件仅记录必要消息ID和收件确认，不把授权码或业务正文加入验收包。

发布阻塞：未解决凭据泄漏、正常通知误过滤、重复接入双发、Hook影响AI正常工作、损坏他人配置、未知邮件结果自动狂重试。非阻塞待办：用户已允许延后的macOS/Linux实机验证；但相关平台不得宣称正式验收完成。WorkBuddy研究未完成可以单列待办，不能将整个多工具目标标全部完成。

## 15. 实施顺序与开放事项

M0工程/依赖/兼容小实验 -> M1事件与契约 -> M2持久队列与策略 -> M3凭据和邮件 -> M4工具接入/后台 -> M5设置界面/Agent协助 -> M6迁移/发行/Windows独立验收。macOS/Linux平台实现与构建可以并行，但实机验收单列M7待办。详细任务见 `02-implementation-plan.md`。

尚需进入实施时落实：正式源码目录和发布身份；服务商开发者OAuth应用注册；签名证书/发布渠道；实际测试邮箱及发送授权；WorkBuddy正式接入接口；mac/Linux测试机器。以上不妨碍本轮设计交付，不能在实现阶段被静默假定为已具备。

## 16. 证据与参考

访问日期：2026-10-30。外部文档可能更新，实现时按版本复核。

- Codex Hooks与信任：https://learn.chatgpt.com/docs/hooks （Stop是事件，不是业务成功证明；transcript不保证稳定协议。）
- Claude Code Hooks：https://code.claude.com/docs/en/hooks （主响应结束、子代理、错误和后台任务语义分别处理。）
- CodeBuddy CLI Hooks：https://www.codebuddy.cn/docs/cli/hooks （仅用于区分产品，不作为WorkBuddy桌面版支持证据。）
- WorkBuddy文档入口：https://www.codebuddy.cn/docs/workbuddy/ （本轮未取得可证明完成Hook的资料，保留未证实状态。）
- Tauri：https://v2.tauri.app/start/ ；系统前提：https://v2.tauri.app/start/prerequisites/ ；更新签名：https://v2.tauri.app/plugin/updater/
- 系统WebView：https://v2.tauri.app/reference/webview-versions/ ；Windows安装：https://v2.tauri.app/distribute/windows-installer/ ；平台签名：https://v2.tauri.app/distribute/sign/windows/ 、https://v2.tauri.app/distribute/sign/macos/
- Rust凭据接口：https://docs.rs/keyring/latest/keyring/ ；Linux后端：https://docs.rs/zbus-secret-service-keyring-store/latest/zbus_secret_service_keyring_store/ （实现时以锁定版为准，须排除生产mock后端。）
- Gmail应用密码：https://support.google.com/accounts/answer/185833
- Outlook.com邮件认证设置：https://support.microsoft.com/en-us/outlook/pop-imap-and-smtp-settings-for-outlook-com
- 产品实现依据：本仓库 Rust/Tauri/Vue 源码、配置 schema、兼容表和逐项验收证据；开发参考不构成发布或运行验收。

## 17. 中立事件边界与事件驱动看门狗（本轮建议评估后纳入）

### 17.1 采纳的模块边界

通知核心只接收版本化的中立契约及本工具自己的配置/队列；**不得读取任何客户端的私有数据库、转录文件或原生Hook字段**。具体包括不得在core/SMTP/GUI/看门狗中出现对`state_5.sqlite`、客户端`sessions/*.jsonl`或Codex专用事件名的解析。core不能依赖adapters；adapters依赖中立类型并由CLI组合调用，CI用依赖检查和禁止私有路径/字段的架构测试守住边界。

已接入的 Codex 完成监听按原实例 ID、规范化数据目录及本地数据库查询契约检查兼容性。Codex 更换版本或程序路径后，只要这些条件及现有 Hook 归属仍有效，沿用原绑定和去重游标。接入页的重新检测只读校验现有绑定。查询契约变更时暂停监听并报告诊断，保留游标供后续修复。新安装或重配原生 Hook 仍按 Hook 兼容表验证。

谁的私有格式，由谁的适配器处理。元数据补全优先正式Hook字段；确需读私有库/转录时，必须是对应适配器中独立、只读、版本受限、可关闭的metadata provider，设时间/大小预算。读取失败只使project/title/message缺失，不阻断通知、不回写客户端库，不要求普通用户提供数据库权限。

适配器包含标准化、实例识别、能力声明、安装/验证映射和可选元数据provider；来源分类规则属于适配器侧。核心只根据已经分级的证据执行通用策略，不能硬编码`source == codex`或RespireJSON结构。未来稳定协议下新增适配器应无需修改SMTP、队列和通用策略；“20行即可完成任何新工具接入”不是交付承诺，配置/trust/版本兼容/测试同样属于适配成本。

外部建议的`source/project/title/message/url`分别对应adapter_id/project_label/session_label/answer/resume_url；source是来源标签，不是可据以静默过滤的可信身份。可保留工具别名用于显示，但核心协议仍有schema_version、实例、轮次、actor与稳定事件ID。`status`与`event`不设可互相矛盾的重复枚举：任务语义由kind/completion_basis表达，投递状态由队列表单独记录。

### 17.2 中立事件映射

| 建议事件 | 采用方式 | 重要边界 |
|---|---|---|
| task_started | LifecycleSignal.Started | 表示本轮开始，只有可靠身份时建立活动记录 |
| task_done | NormalizedEventV1.response_finished | 普通Stop只表示响应结束，不能直接映射业务成功 |
| task_interrupted | interrupted | 可靠事件存在才支持；同时关闭对应活动记录 |
| session_end | LifecycleSignal.SessionClosed | 清理本会话计时，不把会话退出解释为任务成功 |
| stalled | NormalizedEventV1.stalled | 看门狗的疑似状态，basis=none，附判断模式与阈值，不标error/completed |
| 活动/心跳（建议补充） | LifecycleSignal.Progress | 仅可靠事件更新last_progress；不扫描私有转录来猜活动 |

§6已统一定义`stalled`及可选`watchdog_detail { mode, threshold_seconds, elapsed_seconds, last_observed_at }`，均来自核心计时而非客户端任意文字。

### 17.3 持久状态与判定

活动状态存入本工具`activity_state`表，key为可靠instance/session/actor/turn或generation，记录started_at、last_progress_at、last_observed_sequence、boot_id、状态和本轮是否已告警。内存仅作为缓存，不能作为唯一事实来源。

- `elapsed_without_finish`：可靠Started后超过阈值仍无对应结束，文案“本轮运行时间较长，尚未收到结束事件”。长任务可能正常，不称卡死。
- `inactivity`：适配器明确有可靠进展事件时，从last_progress_at计时，文案“疑似长时间无进展”。它仍是提示，不是故障证明。若客户端只有开始/结束，这个模式不可选。
- 每15秒检查到期记录；每个可靠轮次最多生成一次看门狗事件。继续进展可撤销尚未发送的告警，但同轮不反复报警；新轮次才重新计数。
- 正常响应结束、明确失败/中断只关闭自己的actor/轮次计时，不影响同会话其他子代理；SessionClosed关闭同一instance/session内全部actor活动计时，其他instance/session不受影响。客户端仍有后台工作时，仅关闭普通结束事件明确指向的actor轮次，不标整个任务完成。
- 原子事务中检查活动仍有效、读取最新checkpoint、写告警幂等ID及入队；发送前再检查关闭/进展状态。已收到并持久化但仍在spool的较早结束事件优先导入，不允许到期判断越过该实例积压。真正迟到、在邮件发出后才收到的结束无法撤回邮件，展示其观察边界。
- 后台重启、系统休眠恢复、系统时间跳变或入站顺序不可靠时，原计时进入`suspended_unknown`；有新可靠Progress/Resumed才能恢复，必要时建立新计时起点。只有Started/Ended的客户端保持挂起至新一轮，不在恢复瞬间批量告警。实现结合单调时钟、UTC、boot_id及平台恢复通知，不能将睡眠时长直接当卡死。
- 无可靠Started/actor/轮次或generation时不启用看门狗，UI说明缺少能力。可信内部后台任务不建看门狗；普通用户和子代理按各自通知配置处理。

看门狗产生的stalled经过同一队列、免打扰、TTL、限频和正文隐私策略；默认只含项目/会话标签、模式及时间，不截取任务正文。独立daemon退出时暂停检测，恢复后按上面规则处理，不能承诺“完全不运行后台仍能定时提醒”。

### 17.4 验收增补

必须验证：正常长任务有Progress时不触发inactivity；只有start/end时只可启用elapsed提示；两个相同父会话的子代理相互独立；未提供actor不猜测；超时与Stop同时发生最多一个正确状态；DB锁期间Started/Stop均重放后不误告警；后台重启/睡眠恢复不群发；SessionClosed不发送完成信；增加一个虚构测试适配器仅修改适配器/注册/兼容清单即可通过核心合同测试。
