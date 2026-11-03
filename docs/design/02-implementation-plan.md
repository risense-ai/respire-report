# AgentNotify Implementation Plan（可执行实施计划）

> **For agentic workers:** 延续本会话已明确启用的事务司委派与独立验收方式，实施时使用 `superpowers:subagent-driven-development` 逐任务推进，职责与文件范围明确交接；不重新询问是否采用委派。步骤用复选框记录，每任务交付独立核验与验收。不自动commit、push，不自行发真实邮件。本轮仅交付设计和计划，不启动实施。

**Goal:** 构建独立的 Rust 原生通知核心、用户后台和 Tauri/Vue 设置程序，先在 Windows 真实验证多工具接入、可靠入队、隐私保护和邮件投递。

**Architecture:** Hook 通过适配器快速写 SQLite 或安全 spool，独立后台完成策略调度和邮件投递；GUI 仅管理配置、凭据与状态。平台、客户端和邮件服务商以明确接口隔离，未知能力保守展示，复杂安装问题输出脱敏 Agent 协助包。

**Tech Stack:** Rust Cargo workspace、Tauri 2、Vue 3、TypeScript、Vite、SQLite；serde/clap/rusqlite/lettre/keyring/tokio 为候选依赖，精确版本和 features 在任务 01 验证后锁定。时间区、AEAD、Schema/TS 导出依赖同样在任务 01 决定，不以候选库名代替平台验证。

**Spec:** 同目录 `01-design.md`；Agent 提示词交付规范为 `03-agent-prompts.md`。产品文档存放于本仓库 `docs/`。本计划路径中的源码根统一写作独立 `AgentNotify/`；正式绝对位置须在开始实施前确认，不将 Rust 工程写入当前 Java 示例仓库，不自动初始化 Git。

## 全局约束

- 设计和计划同时交付，不要求先另等设计批准才编写本计划；源码、运行、安装、凭据访问和真实发信均不在本轮交付范围。
- 本文所有命令均为**规划命令，未执行**；命令在对应任务建立文件、工具链和脚本后才可运行。除明确说明外，工作目录为已确认的 `AgentNotify/`。
- 先记录目标目录、适用规则、已有文件及各所属仓库的 `git status --short --untracked-files=all`；目录未选定前不建源码树。只暂存本轮新建且检查无敏感数据的正式文件，按精确路径处理，保留他人修改与原有暂存，不自动提交。
- 客户端首版范围为本机 Codex、Claude Code 和 generic；WorkBuddy 为 `research_pending`，普通 Claude Chat 不等于 Claude Code。远程服务器、WSL、容器不在首版范围。
- 事件 schema_version=1；原始输入 <=1 MiB；source IDs 各 <=256 UTF-8 bytes，labels 各 <=512 bytes；instance_id 为 UUID；无正文元数据序列化 <=8 KiB；问题/回答各 <=16 KiB UTF-8，不破坏字符边界。身份字段超限拒绝，不截断成另一身份。
- 受控元数据 JSON 与正文二进制 AEAD 密文使用定长头、nonce/tag 封装，禁止将密文 base64 后嵌套 JSON 膨胀；spool 每项 <=64 KiB，总量 <=16 MiB。无安全内容密钥不收集正文，不降级明文。
- 新装 `enabled=true`、`include_question=false`、`include_answer=false`；`quiet.enabled=true`、23:00–07:30、timezone=system、behavior=skip；metadata_retention_days=30；queue_ttl_hours=24。迁移保留原选择并显示差异。
- 结束信号至少等待3秒；同一可靠实例/会话/actor的正常结束默认10秒聚合，最早发送时间为 `max(最后结束事件接收时间+3秒, 本组首条事件接收时间+10秒)`；关闭聚合仍保留3秒。错误独立，全局上限每分钟10封，重试也计入。
- 上述聚合实际使用同一可靠 actor 的最后结束时间；generation、取消、聚合的key均包含实例/会话/actor。未知actor不跨事件聚合或取消，只按可证稳定event_id去重，不能凭相同正文推断同一子代理。
- 核心只处理工具中立的 `IngressEnvelopeV1`，不依赖adapters、不扫描客户端私有数据库或会话目录；原生Hook字段及可选metadata查询仅在版本受控适配器内处理。`source`字符串只是标签，不是过滤证明；深链仅有官方支持依据时生成，否则为空。
- 可选watchdog默认关闭，阈值1800秒，默认模式 `elapsed_without_finish`；`inactivity` 要求可靠Progress能力。15秒tick、每轮仅一次stalled、状态持久化；无可靠Started能力禁用，重启/休眠活动未确认先 `suspended_unknown`，不在恢复瞬间群发。
- SQLite WAL、busy timeout 1 秒、入队预算上限 2 秒、目标 <300ms；租约 60 秒。明确未发送的临时失败按 30秒/2分钟/10分钟/30分钟并加最多20%抖动，初次加4次，TTL 24小时；DATA后结果不确定默认不重发。
- Windows 10 22H2/Windows 11 x64 为待验证目标；macOS 13+ arm64/x64、Ubuntu 24.04 LTS x64 为 M7 目标，最低版本按依赖与实机收窄。编译或 CI 通过不等于实机验收。
- 凭据只进当前用户系统安全存储；不提供 `--password`，不写网页存储、日志、诊断包、进程参数。真实授权码和真实邮箱验证只能由用户私密输入及明确授权。
- 不修改客户端信任数据库、不绕过系统权限、不重启 AI 客户端；程序关闭窗口不停止独立后台。测试默认使用隔离目录和模拟 SMTP，真实发送必须单独授权一次。
- 任务结束保存命令、返回码、实际结果和限制；未运行标“未执行”，受阻标“阻塞原因”，不把空证据或 mock 通过计入平台实测通过。

## 重点评审输入

1. 正文长得像 Respire JSON，但来源未知或实际是普通用户：必须保留通知；任务 04/07 测试同形 JSON、错误回执、规则缺失。
2. 无 turn_id/event_id/标题、开始信号重入或不提供开始信号：不编造轮次或标题、不永久吞掉后续响应；任务 02/03/04 验证 generation 与 10 秒降级去重。
3. DST 回拨/跳时、跨午夜、接收和发送跨免打扰边界、全天静默：按实际区间唯一摘要、不复活已跳过事件；任务 04 验证。
4. SMTP DATA 后断线或崩溃、凭据库被锁、磁盘满、数据库锁及双后台竞争：不宣称已接受、不重复自动发信、不退回明文；任务 03/05/06 验证。
5. 配置被他人修改、路径含中文空格/符号链接、源机器发送器在途、恶意 Agent 包文本：不损坏他人配置、不产生双发、不执行文本指令；任务 07/09/10/11 验证。

## 文件职责与公共契约

下面是待创建文件，不代表已经存在；每个任务仅拥有其 Files 所列范围。若实施时发现已有工程，先映射职责并调整精确路径，禁止覆盖现有实现。

| 目录/文件 | 职责与责任任务 |
|---|---|
| `Cargo.toml`、`Cargo.lock`、`rust-toolchain.toml`、`apps/desktop/package.json`、`pnpm-lock.yaml` | workspace 与锁定依赖，任务 01 |
| `crates/core/src/{event,config,contracts,privacy,clock}.rs`、`schemas/` | 单一数据模型、时间接口、Schema/TS，任务 02 |
| `crates/core/src/queue/{mod,ingress,sqlite,spool,lease}.rs`、`migrations/` | 原子入站、序号、持久化、幂等、恢复，任务 03 |
| `crates/core/src/policy/{mod,quiet,dedupe,aggregation,watchdog}.rs`、`crates/adapters/src/source_classifier.rs` | 中立策略/持久watchdog（04）与客户端来源分类（07），职责隔离 |
| `crates/core/src/delivery/{mod,smtp,oauth,mail}.rs`、`crates/platform/src/credentials.rs` | 投递、凭据、内容密钥，任务 05 |
| `crates/platform/src/{lib,paths,service,windows,macos,linux}.rs`、`apps/daemon/src/` | 路径、后台、锁、进程生命周期，任务 06 |
| `crates/adapters/src/`、`crates/integration/src/`、`resources/compatibility/` | 正式事件与配置事务，任务 07 |
| `apps/desktop/src/`、`apps/desktop/src-tauri/` | GUI、白名单命令，任务 08 |
| `crates/agent_support/src/`、`schemas/agent-task-v1.schema.json` | 脱敏包与可直接复制的提示词，任务 09 |
| `resources/prompts/migrate.md`、`docs/migration/` | 本产品机器迁移协助与切换边界，任务 10 |
| `packaging/`、`crates/platform/src/update.rs`、`docs/release/` | 安装、签名、更新、卸载，任务 11 |
| `tests/platform/`、`docs/acceptance/`、`docs/compatibility/` | Windows 实机证据与 mac/Linux 待办，任务 12/13 |

Cargo package 名固定为 `agentnotify-core`、`agentnotify-platform`、`agentnotify-adapters`、`agentnotify-integration`、`agentnotify-agent-support`、`agentnotify-cli`、`agentnotify-daemon`；二进制名为 `agentnotify`、`agentnotifyd`；桌面 package 为 `agentnotify-desktop`。前端测试脚本 `test:unit` 使用 Vitest，`test:ui` 使用 Playwright，均在任务 08 添加并锁版本。

公共类型归属：`core::contracts` 定义共用 DTO/状态；`core::event` 定义 `NormalizedEventV1/AdapterOutput/LifecycleSignal`；`core::config` 定义 `ConfigV1`。DTO 按 Rust 导出 Schema/TS，平台内部凭据类型禁止 Serialize。`DateTime<Utc>` 使用统一时间库；`Clock::now(&self)->DateTime<Utc>` 和 `Clock::monotonic(&self)->Duration` 分别提供业务时间与运行预算；测试注入 FakeClock，不 sleep 等待 24 小时。

- `AdapterOutput { source_evidence: SourceEvidence, notification: Option<NormalizedEventV1>, lifecycle: Option<LifecycleSignal> }`；`SourceEvidence { source_class: user|subagent|internal_background|unknown, classification_reason:String, origin_evidence:verified_adapter|caller_reported|unknown, background_result:success|other|unknown }` 是notification与lifecycle共用且只存一次的上下文，不在notification复制可矛盾字段；generic保留字段不可自填，固定caller_reported。只有verified_adapter+internal_background+success通知默认静默，错误/unknown保留；纯Started只要verified_adapter+internal_background即不建watchdog，不要求开始信号宣称执行成功。
- `LifecycleSignal { instance_id, session_id, actor_id: Option<String>, actor_scope: Main|Identified|Unknown, turn_id: Option<String>, upstream_id: Option<String>, kind: Started|Progress|Resumed|SessionClosed, received_at }`。生命周期信号自身不发邮件；SessionClosed以实例/会话为范围清理所有actor，不能群发完成。notification同样含actor_id/actor_scope，未知开始信号不自造generation。
- `IngressEnvelopeV1 { ingress_id: UUID, received_at, per_instance_sequence: Option<u64>, adapter_output: AdapterOutput }`；`Ingress::accept(output:AdapterOutput)->Result<EnqueueOutcome,QueueError>` 完成admission、序号、DB或spool；`Queue::ingest(envelope:IngressEnvelopeV1)->Result<EnqueueOutcome,QueueError>` 在同一事务应用生命周期、notification、checkpoint及幂等记录。`EnqueueOutcome=Queued{id}|Duplicate{id}|Spooled{id}` 中id为接收单元ingress_id，纯生命周期同样持久化但不创建邮件；未持久化只能返回Err。
- `ActorKey=Main{instance_id,session_id}|Identified{instance_id,session_id,actor_id}|Unknown{ingress_id}`；稳定event_id去重优先，Unknown不得共享generation或跨事件取消/聚合。`ActivityKey { actor:ActorKey, turn:TurnKey }` 只接受可靠actor及上游turn/可靠generation，Unknown不启动watchdog。
- `Watchdog::observe(tx:&Transaction, envelope:&IngressEnvelopeV1)->Result<(),WatchdogError>`、`Watchdog::evaluate_due(tx:&Transaction, clock:&ClockSnapshot)->Result<Vec<NormalizedEventV1>,WatchdogError>` 与队列事务共享activity_state；`ClockSnapshot { utc, monotonic, boot_id, resume_epoch }`。生成事件含稳定event_id、kind=stalled、completion_basis=none和 `watchdog_detail { mode, threshold_seconds, elapsed_seconds, last_observed_at }`；队列内部关联activity_key/revision，发送前原子复查，晚到结束可取消未发提醒。默认1800秒/15秒tick，每轮至多一次；两种模式均不承诺卡死或失败。
- `Policy::decide(event:&NormalizedEventV1, evidence:&SourceEvidence, config:&ConfigV1, now:DateTime<Utc>)->PolicyDecision`；`PolicyDecision=Notify{not_before}|Suppress{reason}|Digest{window_key}|Defer{until}`。共同SourceEvidence随接收单元保存并显式传入；接收时决策持久化，发送时重新检查，终态不复活。
- 队列状态 `queued/leased/sent/suppressed/digest_pending/retry_wait/auth_blocked/delivery_unknown/expired/permanent_failed`；`sent` 的 `recipient_confirmed_at` 为独立可空证据，不随 SMTP 接受自动填写。状态转换由核心统一校验，UI 不直接 UPDATE 数据库。
- `CredentialStore::get(&CredentialRef)->Result<CredentialHandle, CredentialError>`、`put(&CredentialRef, SecretInput)->Result<(), CredentialError>`、`delete(&CredentialRef)->Result<(), CredentialError>`；`CredentialHandle` 不可导出/打印，SecretInput 不进入通用日志。
- `Transport::send(mail:&PreparedMail, credentials:&CredentialHandle)->DeliveryOutcome`；`DeliveryOutcome=Accepted{message_id}|Retryable{code}|AuthBlocked{code}|PermanentFailure{code}|Uncertain{stage}`；`SendStage=Prepared|Connecting|Envelope|DataStarted|Accepted`，持久值 `prepared/connecting/envelope/data_started/accepted`，开始DATA前持久化data_started，保守恢复。
- `IntegrationManager::plan(request:IntegrationRequest)->IntegrationPlan`、`apply(plan:&IntegrationPlan)->ApplyReport`、`rollback(operation_id:&str)->RollbackReport`。计划含原 hash、精确差异、备份引用和实例版本；错误是报告中的结构化结果，不能假装全部完成。
- `HealthReport { schema_version, daemon, queue_counts, credential_status, integrations, issues }` 只含白名单字段；`IntegrationStatus` 严格使用设计中的十个状态，状态及允许转换由任务 02 固定。

## 任务 01（M0）：工具链、凭据、邮件与后台可行性门禁

**Files:** 创建根 workspace/锁文件、`docs/feasibility/{matrix,dependencies,provider-registration}.md`、`tests/feasibility/{credential_real,service_real,smtp_local}.rs`、`scripts/feasibility.ps1`；此任务只建立最小实验和依赖结构，不把实验代码当已验收产品。

**Interfaces:** Consumes：设计平台矩阵和本机实际 OS/build；Produces：`FeasibilityResult { probe, os_build, backend, dependency_version, features, actual, evidence, status }`，以及锁定工具链/依赖、每个平台支持后端清单。`status=passed|failed|blocked|not_run`，不以布尔值混淆未执行。

- [ ] **01.1 写门禁测试：** `credential_real_roundtrip` 要求运行后端枚举为原生系统后端、随机测试键写入/读取/删除一致；未连接真实服务应 FAIL/BLOCKED 而非退回 mock。`smtp_local_tls_probe` 验证受信测试 CA 成功、不受信证书/STARTTLS降级失败；`smtp_data_stage_observable` 必须证明库或可审计封装能在DATA前持久化data_started，不能用高层send只返回成功/失败来猜阶段。`service_gui_independence` 要求 GUI 不运行也能由用户启动入口创建独立进程。
- [ ] **01.2 先记录失败：** 规划命令 `pwsh -File scripts/feasibility.ps1 -Mode Baseline`；预期报告缺失能力或未实现探针，必须非通过；记录 Windows build、Rust/Node/pnpm/构建工具/WebView 可用性，不安装缺少组件来掩盖失败。
- [ ] **01.3 最小实验并锁定：** 在实施授权范围配置开发工具链，核实候选库 features；Windows 实测 Credential Manager/DPAPI 可选封装及用户任务启动；Linux 明确是否有已解锁 Secret Service，macOS 标待实机。候选 keyring 必须验证 native backend，禁止 mock feature 使正式凭据测试“通过”。发行者负责 OAuth 公共客户端注册、redirect URI、权限与审核，证书和更新公钥归属同时记录；未具备登记为阻塞项。
- [ ] **01.4 验证：** 规划命令 `pwsh -File scripts/feasibility.ps1 -Mode Verify -Platform Windows`；预期逐探针产出真实报告、清理随机测试键、无秘密日志；`cargo metadata --locked --format-version 1` 与 `pnpm --dir apps/desktop install --frozen-lockfile` 只核对依赖可解析，不当成功能通过。此阶段不发送真实邮箱。
- [ ] **01.5 独立验收：** 核验原生 backend 名、实际锁文件、最小支持 OS 与未运行平台清单；若无可用安全凭据或后台能力，停止相关实现承诺，先修订可行性结论。产物证据在 `docs/acceptance/m0/`，不存密码/token。

## 任务 02（M1）：统一类型、协议、CLI 和隐私边界

**Files:** 创建 `crates/core/src/{lib,event,config,contracts,privacy,clock}.rs`、`apps/cli/src/{main,commands,output}.rs`、`schemas/{event-v1,config-v1}.schema.json`、`crates/core/tests/contracts.rs`、`apps/cli/tests/protocol.rs`、`apps/desktop/src/generated/contracts.ts`。

**Interfaces:** Consumes：任务 01 锁定工具链；Produces：上节共用类型、`parse_generic(raw:&[u8], context:&IngressContext)->Result<AdapterOutput, InputError>`、`sanitize(event:NormalizedEventV1, config:&ConfigV1)->Result<SanitizedEvent, PrivacyError>`、`CommandReport<T>{schema_version,ok,data,error}`；`InputError` 固定错误码如 `input_too_large/unsupported_schema/invalid_identity/metadata_too_large`，不含正文。

- [ ] **02.1 写失败测试：** `schema_rejects_unknown_version`、`external_cannot_claim_trusted_source`、`utf8_limits_and_no_body_when_disabled`、`missing_titles_are_explicit`；断言 1 MiB 边界、ID/元数据上限、null session/turn、缺省标题、“本轮处理结束”与“调用方报告完成”文案。固定六种kind（含stalled）、四种completion_basis、actor/agent_kind/background_state/source_class枚举、十种接入状态及十种队列状态；stalled只能表达运行超时/疑似无进展，不能表示卡死。序号可空须order_unknown诊断；未知状态不得静默成功。
- [ ] **02.1a 固定完成语义断言：** `task_completed_requires_explicit_basis` 验证只有explicit_task_event或explicit_caller_report可使用task_completed；后者邮件始终写“调用方报告完成”，不表示客户端业务验收成功；普通Stop只能response_finished。
- [ ] **02.2 运行失败：** 规划命令 `cargo test -p agentnotify-core --test contracts`、`cargo test -p agentnotify-cli --test protocol`；预期缺失实现或断言失败，保留基线。
- [ ] **02.3 最小实现：** 生成单一Schema/TS，限制raw输入读取；配置原子保存并检查schema；正文关闭在落盘前清除，开启按UTF-8截断并标记。原始Respire协议判定在任务07的适配器内、截断前完成，核心只收到中立分类结果和受控证据枚举，不收原始协议字段。CLI只调适配器入口，不理解原生Hook字段；UI只使用中立DTO。添加watchdog.enabled=false/threshold_seconds=1800/mode=elapsed_without_finish及能力约束。CLI不提供敏感字符串参数。
- [ ] **02.4 固定 CLI 契约：** 实现帮助、参数解析、结构化错误和安全占位后端：`doctor --json`；`integration inspect --json`、`plan --request <json-file>`、`apply --plan <json-file>`、`verify --operation-id <id>`、`rollback --operation-id <id>`；`hook <adapter> --instance-id <uuid>`（stdin）；`emit --stdin`；`service install|start|stop|status|uninstall`；`agent-bundle create --task <json-file> --out <dir>`；`test --dry-run`、`test --send`。全局 `--data-dir <absolute-dir>` 仅接受当前用户安全目录；帮助明确此项不会绕过权限。通用命令退出码 0=成功、2=输入错误、3=需要用户动作、4=运行失败；Hook 使用宿主规定的非阻塞退出/输出，无 blocker/continue 指令。命令未实现须返回结构化 unavailable，禁止返回假成功。
- [ ] **02.5 通过并验收：** 重跑 02.2；规划命令 `cargo run -p agentnotify-cli -- --help` 和 `cargo run -p agentnotify-cli -- test --dry-run` 只能展示不发信预演，配置未完成则明确缺项。检查自动生成文件无手写漂移、stdout 不含正文/凭据；产出 `docs/acceptance/m1-contracts.md`。

## 任务 03（M2-A）：持久队列、spool、租约与恢复

**Files:** 创建 `crates/core/src/queue/{mod,ingress,sqlite,spool,lease}.rs`、`crates/core/migrations/001_queue.sql`、`crates/core/tests/{queue_recovery,spool_safety}.rs`。任务 05 通过接口提供真实加密，当前仅隔离测试 key fixture，不伪称系统密钥验收。

**Interfaces:** Consumes：`IngressEnvelopeV1/AdapterOutput/Clock/ActorKey`、任务04稳定幂等key和Watchdog事务接口；Produces：`Ingress::accept/Queue::ingest`、`lease_due(now,limit)->Result<Vec<Lease>,QueueError>`、`transition(id,expected,next)->Result<(),QueueError>`、`import_spool()->ImportReport`、`BodyProtector::seal/open` 接口；`Lease { id, expires_at, send_stage }`，spool与SQLite共用入站ID和事件幂等key。`AdmissionGate::enter()->AdmissionGuard`、`set_maintenance(enabled)->Result<(),QueueError>` 与平台文件锁/原子序号文件协作，生命周期与通知不可拆开提交。

- [ ] **03.1 写失败测试：** `concurrent_enqueue_single_row`、`separate_instances_not_deduped`、`locked_db_falls_back_to_spool`、`spool_full_is_error`、`disk_full_never_acknowledged`、`symlink_or_shared_directory_rejected`、`lease_recovery_respects_send_stage`；验证锁1秒后仍在2秒预算内处理，WAL/唯一索引/事务有效；重导spool并发不双入队；nonce/tag/头+密文总长 <=64 KiB，总配额 <=16 MiB。`lifecycle_and_event_atomic`、`locked_started_stop_replay_order`、`gap_degrades_without_infinite_wait`、`spool_backlog_prevents_db_overtake`、`unknown_order_never_cancels_other_event` 验证独立序号、checkpoint、迟到Started不改已有身份及重放不重复应用生命周期。故障注入覆盖序号文件、临时文件、fsync/rename、导入事务各断点。
- [ ] **03.2 运行失败：** 规划命令 `cargo test -p agentnotify-core --test queue_recovery`、`cargo test -p agentnotify-core --test spool_safety`；预期缺实现/安全断言失败。
- [ ] **03.3 最小实现：** 建状态机、持久发送阶段与租约；admission互斥下独立序号文件原子更新，持锁至整个envelope提交DB或spool；无序号时UUID应急spool标order_unknown，禁止推测generation/跨事件取消。积压期间同实例新事件继续spool；导入每批预算 <=100ms，释放锁让Hook快落盘，无更早积压才切DB。事务内应用生命周期、watchdog观察、notification和checkpoint；gap记录降级，不无限等待，不追溯重写已处理身份。SMTP不占写锁；未开始投递租约可恢复，data_started未确认转delivery_unknown。终态清正文，不宣称擦盘。
- [ ] **03.4 通过与验收：** 重跑 03.2；规划命令 `cargo test -p agentnotify-core --test queue_recovery state_transition_matrix` 验证十种状态允许/禁止转换、24小时过期、重启恢复、auth修复不越TTL。Windows入队性能预算的真实测量留任务12，不能以 FakeClock 断言替代。保存 `docs/acceptance/m2-queue.md`。

## 任务 04（M2-B）：中立过滤策略、去重、聚合与持久看门狗

**Files:** 创建 `crates/core/src/policy/{mod,quiet,dedupe,aggregation,watchdog}.rs`、`crates/core/migrations/002_activity_state.sql`、`crates/core/tests/{policy_time,classification,dedupe,watchdog}.rs`、`tests/fixtures/events/`；核心fixture仅中立事件/证据，不含客户端原始协议。

**Interfaces:** Consumes：中立事件/证据与 `LifecycleSignal/ActorKey/ClockSnapshot/ConfigV1`；Produces：`Policy::decide`、`dedupe_key(event:&NormalizedEventV1, generation:Option<u64>, window:&DedupeWindow)->DedupeKey`、`observe_lifecycle(signal)->LifecycleEffect`、`aggregation_window(event)->Option<AggregationKey>`、上节 `Watchdog::observe/evaluate_due`。Unknown actor的聚合返回None；核心只依据已分级的SourceEvidence执行通用规则，不解析raw。`LifecycleEffect` 可递增可靠generation或取消同actor轮次待发，SessionClosed只清对应实例/会话。

- [ ] **04.1 写失败测试：** 核心 `unknown_evidence_retained`、`internal_success_requires_verified_evidence`、`subagent_not_blanket_filtered` 验证未知来源保留、受信分类仍须满足用户规则，原始Respire协议用例由任务07负责。`pure_internal_started_has_evidence_and_no_watchdog`、`caller_reported_cannot_suppress` 验证纯生命周期也使用AdapterOutput的共同SourceEvidence；不从notification缺失推测内部背景。`generation_idempotent_start`、`known_actor_missing_turn_fallback_ten_seconds` 验证重复开始不加代、可靠actor无开始仅10秒精确去重；`unknown_actor_only_stable_event_id_deduped`、`parent_progress_cannot_cancel_child`、`siblings_do_not_merge` 防不同actor互吞。无稳定event_id的未知子代理宁可重复通知，不凭正文猜身份。
- [ ] **04.2 写时间测试：** `quiet_receive_or_send_suppresses`、`quiet_equal_means_all_day`、`dst_overlap_digest_once`、`dst_gap_next_valid_instant`、`system_zone_change_rechecks_pending`；固定 UTC fixture 配合 IANA `America/New_York` 的春秋 DST，时区数据版本入证据。验证跨午夜、修改规则不复活、摘要仅计数标签且以实际静默区间唯一key。`stop_progress_cancels`、`aggregate_wait_max_three_and_ten`、`retry_respects_global_ten_per_minute` 断言3秒/10秒关系、错误独立、超额一分钟摘要。
- [ ] **04.2a 写看门狗失败测试：** `long_job_with_progress_not_inactive`、`start_end_only_disables_inactivity`、`unknown_actor_no_watchdog`、`restart_sleep_clock_jump_suspend`、`new_progress_resumes_without_alert_storm`、`session_closed_clears_only_its_session`、`stop_timeout_race_atomic`、`spooled_stop_precedes_timeout`、`one_alert_per_turn_with_stable_id`；断言默认关闭/1800秒/15秒tick、受信内部后台不建activity。任务结束关闭指定actor轮次，不能把其他后台工作判完成；只有start/end的客户端恢复后保持挂起直到新轮。
- [ ] **04.3 运行失败：** 规划命令 `cargo test -p agentnotify-core --test classification`、`cargo test -p agentnotify-core --test dedupe`、`cargo test -p agentnotify-core --test policy_time`、`cargo test -p agentnotify-core --test watchdog`；预期目标断言失败。
- [ ] **04.4 最小实现：** 核心使用分级证据和用户规则，来源->协议->分类在适配器完成；采用真实时区规则和受控时钟，保存接收决定并发送复查。generation、3秒取消、聚合按可靠ActorKey；摘要key不用墙钟字符串唯一。activity_state持久化started_at/last_progress_at/sequence/boot_id/状态/告警标记；同事务observe、evaluate_due及幂等入队，发送前复核revision，积压实例先导入早先事件后判超时。重启/睡眠/时钟跳变/顺序未知进入suspended_unknown；真实迟到Stop无法撤回已发邮件，文案保留观察边界。stalled走同一免打扰/TTL/限频，默认不附正文。Windows离席能力按平台标记启用，mac/Linux未验证禁用。
- [ ] **04.5 通过与验收：** 重跑04.3；审阅用例数据与已定义协议期望，重点检查“未知保留”而非全部过滤。输出 `docs/acceptance/m2-policy.md`，列出协议规则版本、输入哈希和每类用例结果。

## 任务 05（M3）：SMTP、原生凭据、OAuth 与正文加密

**Files:** 创建 `crates/core/src/delivery/{mod,smtp,oauth,mail}.rs`、`crates/platform/src/credentials.rs`、`resources/providers/providers-v1.json`、`crates/core/tests/{smtp_faults,mail_privacy,oauth}.rs`、`crates/platform/tests/credentials_native.rs`。

**Interfaces:** Consumes：`Transport/DeliveryOutcome/SendStage/BodyProtector/CredentialRef/Clock`；Produces：原生 `CredentialStore`、`Transport::send`、`prepare_mail(event,config)->Result<PreparedMail,MailError>`、`OAuthSession::start(provider)->Result<AuthorizationRequest,OAuthError>`、`finish(callback)->Result<CredentialRef,OAuthError>`、`BodyProtector` AEAD 实现。构造Transport时注入 `SendJournal::record(delivery_id:&str, stage:SendStage)->Result<(),JournalError>`，关联当前租约；data_started记录必须提交成功才发送DATA。`AuthorizationRequest`只含授权URL/会话引用，无token；回调短生命周期、绑定state/nonce与PKCE。

- [ ] **05.1 写失败测试：** `smtp_4xx_backoff_bounded`、`smtp_5xx_permanent`、`auth_blocks_until_repaired`、`data_disconnect_unknown_no_retry`、`crash_after_data_unknown`；assert稳定Message-ID不被当去重保证、最多5次总尝试、TTL24h。`header_crlf_rejected`、`unsafe_scheme_rejected`、`plaintext_auth_forbidden`、`invalid_certificate_fails`、`terminal_body_removed`、`encrypted_spool_no_cleartext` 固定隐私行为。`stalled_send_rechecks_activity_revision`、`stalled_uses_same_quiet_ttl_rate_limit` 验证晚到结束/进展在SMTP开始前取消尚未发送提醒，且watchdog不绕过普通策略；邮件仅含中立标签和watchdog_detail。
- [ ] **05.2 运行失败：** 规划命令 `cargo test -p agentnotify-core --test smtp_faults`、`cargo test -p agentnotify-core --test mail_privacy`、`cargo test -p agentnotify-core --test oauth`；预期未实现断言失败，模拟SMTP禁止向公网转发。
- [ ] **05.3 最小实现：** 只支持TLS/STARTTLS；SendJournal的data_started提交成功后才允许发送DATA，`journal_failure_prevents_data` 用模拟服务器断言未收到DATA；accepted记录失败按已持久化data_started恢复为不确定，不自动重发。库不能提供所需阶段则M0判阻塞，不能猜普通send返回值。受控重试与显式重试分开，后者提示可能重复。原生取密钥失败进入auth_blocked，无明文后备；无安全正文密钥用无正文模式并说明。新装默认无正文，终态清暂存、未完成最长24h、元数据30天维护。
- [ ] **05.4 OAuth与服务商验证：** provider预设版本化并验签更新；QQ/163/Gmail应用密码条件写明，Outlook等现代认证走注册应用。测试state/nonce/PKCE错配、过期回调、刷新失败、撤销、企业拒绝；应用尚未注册则UI标 unavailable，不装假client ID，不索要邮箱登录密码。发布者完成注册是发布交付任务，不能让普通用户自建开发者应用。
- [ ] **05.5 通过与独立验收：** 重跑05.2；规划命令 `cargo test -p agentnotify-platform --test credentials_native -- --ignored --nocapture` 只在明确测试命名空间运行真实后端读写删除，不打印秘密；Windows真实通过与mac/Linux待实测分列。Linux无服务/被锁用例必须在真实目标环境证明拒绝保存，mock结果仅算单元证据。真实邮箱端到端留任务12，不因模拟SMTP通过勾选已收件。

## 任务 06（M4-A）：独立后台、平台路径与用户级启动

**Files:** 创建 `crates/platform/src/{lib,paths,service,windows,macos,linux}.rs`、`apps/daemon/src/{main,runner,health}.rs`、`crates/platform/tests/{service_contract,paths}.rs`、`apps/daemon/tests/lifecycle.rs`。

**Interfaces:** Consumes：`Queue/Policy/Transport/Clock/CredentialStore`；Produces：`PlatformPaths::discover()->Result<PlatformPaths,PlatformError>`、`ServiceManager::install/start/stop/status/uninstall()->ServiceReport`、`Daemon::run(shutdown:ShutdownSignal)->Result<(),DaemonError>`、`wake()->WakeOutcome`、`HealthReport`；`ServiceReport` 区分 installed/running/unsupported/failed，不混同GUI状态。

- [ ] **06.1 写失败测试：** `closing_gui_keeps_daemon`、`same_user_second_daemon_rejected`、`failed_wake_preserves_event`、`shutdown_preserves_uncertain_delivery`、`credentials_failure_no_prompt_loop`；路径样本覆盖中文、空格、非ASCII用户名、XDG，不能手拼用户名或跟随共享目录。`service_commands_idempotent` 验证只操作本工具入口。`watchdog_tick_requires_running_daemon`、`resume_epoch_suspends_old_activity` 验证15秒tick、boot_id/单调时钟/平台恢复通知联动，不把休眠时间解释为卡死。
- [ ] **06.2 运行失败：** 规划命令 `cargo test -p agentnotify-platform --test service_contract`、`cargo test -p agentnotify-platform --test paths`、`cargo test -p agentnotify-daemon --test lifecycle`；预期缺实现或断言失败。
- [ ] **06.3 最小实现：** Windows用户登录任务、macOS LaunchAgent、Linux systemd --user；环境不支持就报告，不改成高权限系统服务。通过稳定Launcher定位版本目录；不开放公网端口，GUI不持有daemon生命周期。单用户锁+租约防并发，网络不占写事务；暂停通知、退出界面、停止后台分别实现。
- [ ] **06.4 通过与验收：** 重跑06.2；规划命令 `cargo run -p agentnotify-cli -- service status` 默认只读，真实install/start须实施阶段的平台操作授权和隔离安装记录。Windows真实退出GUI、登录恢复留任务12；mac/Linux在任务13逐机验证。产出 `docs/acceptance/m4-service.md`，区分协议测试、实际用户服务证据。

## 任务 07（M4-B）：客户端适配器与可回滚接入事务

**Files:** 创建 `crates/adapters/src/{lib,codex,claude_code,generic,source_classifier,metadata}.rs`、`crates/integration/src/{lib,inspect,plan,apply,rollback,compatibility}.rs`、`resources/compatibility/compatibility.json`、`crates/adapters/tests/{normalize,source_classifier,architecture}.rs`、`crates/integration/tests/transaction.rs`、`tests/fixtures/{client-configs,respire}/`。旧协议回归样本只读脱敏提取，不拷贝真实facts/答案/授权信息。

**Interfaces:** Consumes：`AdapterOutput/SourceEvidence/Ingress` 与任务02 CLI DTO；Produces：`Adapter::normalize(raw:&[u8], context:&AdapterContext)->Result<AdapterOutput,AdapterError>`、`classify(raw:&[u8], proof:&OriginProof, rules:&RuleSet)->SourceEvidence`、`MetadataProvider::lookup(request:&MetadataRequest, budget:&LookupBudget)->Result<OptionalMetadata,MetadataError>`、`inspect()->InspectionReport`、`IntegrationManager::plan/apply/rollback`、`verify(operation_id)->VerificationReport`。`AdapterContext`绑定登记命令、实际配置实例、已支持版本，不接受正文伪造绑定；metadata provider只读、版本受限、可关闭、设时间/大小预算，超时仅缺标签，仍在Hook整体2秒预算内。

- [ ] **07.1 写失败测试：** `stop_is_response_not_business_success`、`background_pending_not_all_completed`、`unknown_version_read_only`、`workbuddy_research_pending`、`host_hook_never_blocks`、`generic_claim_not_trusted`；以当前官方各版本样本区分Stop/Interrupt/SubagentStop/StopFailure及可靠Started/Progress/SessionClosed能力，未证实事件不得注册。`same_shape_json_from_user_is_retained`、`respire_requires_source_and_exact_protocol`、`unknown_schema_error_and_missing_rules_retained`、`utf16_whitespace_protocol_before_truncation` 覆盖来源+严格协议、错误/未知schema保留、普通JSON/子代理不误滤；SourceEvidence覆盖纯生命周期，源路径证据不被表述为防恶意同用户认证。
- [ ] **07.1a 写架构失败测试：** `core_does_not_depend_on_adapters`、`private_fields_only_in_adapter`、`metadata_provider_failure_keeps_event`、`fixture_adapter_requires_zero_core_changes`。新增虚构测试适配器只改适配器/注册/兼容清单即复用核心；核心/SMTP/GUI/watchdog不得读state_5.sqlite、sessions/*.jsonl或解析原生Hook字段；深链无官方依据为空。可选metadata超时/未知版本/关闭不影响通知；不向普通用户索取客户端库权限。
- [ ] **07.2 写事务测试：** `preserve_unowned_hooks`、`apply_hash_conflict_stops`、`partial_rollback_owned_steps_only`、`rollback_user_edit_preserved`、`trust_pending_not_live_verified`、`multiple_instances_separate`、`path_quotes_chinese_spaces`；计划含hash/差异/备份/版本，二次apply幂等。兼容表含产品版本、OS/架构、事件、元数据、证据级别、日期/引用。
- [ ] **07.3 运行失败：** 规划命令 `cargo test -p agentnotify-adapters --test normalize`、`cargo test -p agentnotify-adapters --test source_classifier`、`cargo test -p agentnotify-adapters --test architecture`、`cargo test -p agentnotify-integration --test transaction`；预期未实现或保护断言失败。
- [ ] **07.4 最小实现：** 只按显式路径->支持环境变量->官方默认路径发现，不全盘扫描写入。按inspect/plan/apply/verify执行；信任/重载给用户原生入口，禁止写信任记录。隔离探针只能到probe_passed，真实事件才live_verified；未知版本生成协助包。WorkBuddy只登记研究状态，不能借CodeBuddy CLI模板冒充支持。
- [ ] **07.5 通过与验收：** 重跑07.3；规划命令 `cargo run -p agentnotify-cli -- integration inspect --json` 为只读演示；不得在当前机器擅自apply。核验配置备份和回滚的实际字节差异，产出 `docs/acceptance/m4-integrations.md`；真实客户端独立留任务12。

## 任务 08（M5-A）：Tauri/Vue 设置、状态与安全交互

**Files:** 创建 `apps/desktop/src/views/{Overview,Mail,Integrations,Rules,History,Diagnostics}.vue`、`src/services/bridge.ts`、`src/components/`、`src-tauri/src/{lib,commands}.rs`、`src-tauri/capabilities/default.json`、`tests/{unit,ui}/`；读取任务02生成类型，禁止手改生成文件。

**Interfaces:** Consumes：`ConfigV1/HealthReport/IntegrationPlan/ApplyReport` 与核心服务；Produces：受控命令 `get_health()->HealthReport`、`load_config()->ConfigV1`、`save_config(config)->ConfigSaveReport`、`store_mail_secret(input:SecretInput)->CredentialRef`、`plan_integration(request)->IntegrationPlan`、`apply_integration(plan_id)->ApplyReport`、`preview_test()->TestPreview`、`send_test(confirmation:ExplicitSendConfirmation)->TestReport`。SecretInput只短暂经受限命令传输到原生存储，不持久化在WebView。

- [ ] **08.1 写失败测试：** `save_does_not_send`、`test_requires_explicit_click`、`secrets_never_persist_or_echo`、`close_pause_stop_are_distinct`、`unsupported_event_disabled_with_reason`、`default_privacy_and_quiet_visible`、`config_conflict_not_overwritten`、`smtp_accepted_not_received`；UI覆盖十种接入/队列状态、未知标题、全天静默提示、auth阻塞和delivery_unknown的显式重试提示。`watchdog_disabled_without_started`、`inactivity_requires_progress_capability`、`elapsed_not_labeled_stuck` 覆盖默认关闭、1800秒设置、挂起状态和两种模式含义；不展示原生Hook字段。
- [ ] **08.2 运行失败：** 规划命令 `pnpm --dir apps/desktop test:unit --run`、`pnpm --dir apps/desktop test:ui`；预期目标测试失败，UI后端使用无公网发信能力的测试驱动。
- [ ] **08.3 最小实现：** 六页面只展示用户需要的信息；配置差异、集成预演与实际权限提示；高级SMTP折叠；敏感输入不进localStorage/sessionStorage/调试日志。Tauri权限精确白名单，不暴露通用任意shell/文件写入命令。已授权用户点击test才通过后台发送一次测试。
- [ ] **08.4 通过与验收：** 重跑08.2，规划命令 `cargo test -p agentnotify-desktop` 检查命令参数校验；人工检查键盘焦点、缩放、窄窗口、错误“原因/已完成/下一步”。UI模拟不能替代Tauri原生、真实凭据和后台验证；保存截图及 `docs/acceptance/m5-ui.md`，截图不得出现真实授权码。

## 任务 09（M5-B）：脱敏 Agent 包、提示词与能力边界

**Files:** 创建 `crates/agent_support/src/{lib,bundle,prompt,redaction}.rs`、`schemas/agent-task-v1.schema.json`、`resources/prompts/{install,repair,migrate,upgrade,readonly}.md`、`crates/agent_support/tests/{bundle,prompt}.rs`；模板以交付文档 `03-agent-prompts.md` 为依据。

**Interfaces:** Consumes：`HealthReport/InspectionReport/IntegrationPlan`；Produces：`create_bundle(request:&AgentTaskV1, destination:&Path)->Result<BundleManifest,BundleError>`、`render_prompt(task:&AgentTaskV1, manifest:&BundleManifest)->Result<String,PromptError>`；`AgentTaskV1 { schema_version, scenario:install|repair|migrate_machine|audit|upgrade_restore, mode:design_review|read_only|apply_authorized, target_instances, allowed_changes, intent_source, execution_budget, test_send, constraints }`；`TestSendPolicy { allowed:false(默认), authorization_source, recipient_ref, max_attempts, expires_at, consumed }`。匹配03文档，路径仅是范围数据，不能自动授权；兼容证据等级用not_run/static_reviewed/automated_checked/host_verified。

- [ ] **09.1 写失败测试：** `whitelist_excludes_tokens_bodies_env_logs`、`email_masked_by_default`、`paths_bound_no_unresolved_placeholder`、`manifest_detects_tamper`、`untrusted_text_not_instructions`、`chat_without_local_tools_reports_gap`；用token/密码/事实正文诱饵断言整个包不含这些内容。`six_required_members_even_without_plan`、`archive_traversal_duplicate_symlink_rejected`、`path_privacy_preview_and_aliases`、`expired_bundle_rejected`、`test_authorization_single_use`、`agent_claim_requires_local_recheck` 补齐包结构、信任与结果回收。四场景+只读入口均含边界：不绕过信任、不批删、不改安全策略、不重启AI客户端、不自行试信。
- [ ] **09.2 运行失败：** 规划命令 `cargo test -p agentnotify-agent-support --test bundle`、`cargo test -p agentnotify-agent-support --test prompt`；预期缺实现或泄漏/占位符断言失败。
- [ ] **09.3 最小实现：** 按白名单生成六必需文件 `task.json/doctor.json/plan.json/compatibility.json/START_HERE.md/MANIFEST.json`，可另附PROMPT.txt；无预演则plan.status=unavailable及原因，不缺文件/编步骤。MANIFEST不自校验自身，标kind=local-diagnostic-bundle、publisher_signature=not_applicable，检查成员路径/长度/哈希/schema/有效期；不打包全量配置/DB/log。复制前预览必要绝对路径，上传证据默认路径别名；隐藏路径改当前目录发现，不暗带原路径。模板变量缺失失败，用户不补占位符。包hash只标一致性；授权从当前请求/可信UI受限一次记录核验，包内true不算授权。Agent回报须本机重新检查后更新机器状态。
- [ ] **09.4 通过与验收：** 重跑09.2；规划命令 `cargo run -p agentnotify-cli -- agent-bundle create --task tests/fixtures/agent-task-install.json --out target/agent-bundle-fixture` 使用脱敏fixture输出。核验提示词可直接复制、没有隐藏命令，实际Agent辅助安装另获范围授权；产出 `docs/acceptance/m5-agent.md`。

## 任务 10（M6-A）：AgentNotify 机器迁移协助与双发防护

**当前交付：** `resources/prompts/migrate.md`、`docs/migration/windows.md`、`Scenario::MigrateMachine` 及 UI 的“迁移到新电脑”协助入口。此阶段提供 Agent 协助，不提供自动跨机器导出/导入命令。

**契约：** 可携带偏好须满足目标 config schema；目标先 disabled，重建本机安装/凭据引用/正文密钥/客户端实例/路径。秘密、host.json、安装记录、计划任务、信任、历史正文和待发队列不复制。客户端未知能力保持禁用。

- [ ] **10.1 源端准备：** 明确 A/B、目标平台、通知偏好及允许交接的非秘密字段；本机诊断包不充当配置备份。
- [ ] **10.2 目标端准备：** 使用来源已确认的对应平台包，安全安装并由用户私密填写邮箱授权码；缺访问能力的一端交接，不冒称完成。
- [ ] **10.3 正式切换：** 源端先停发并确认在途状态，目标端正式接入与用户信任后启用；不能确认源机停发则目标继续关闭。
- [ ] **10.4 运行验收：** 在获授权的两台机器验证偏好、单一发送端、客户端真实事件和回退。回退先关闭目标再恢复源端，不重放历史，不覆盖并发修改。未实际跨机执行前保留待办。

## 任务 11（M6-B）：安装、签名更新、回滚与卸载

**Files:** 创建 `packaging/{windows,macos,linux}/`、`crates/platform/src/update.rs`、`crates/platform/tests/update_rollback.rs`、`docs/release/{signing,release-checklist,offline-install}.md`、`scripts/package.ps1`；构建脚本不内置私钥。

**Interfaces:** Consumes：稳定Launcher、`ServiceManager/Queue SendStage/AdmissionGate`、配置/DB/spool版本；Produces：`verify_release(manifest:&SignedManifest)->Result<VerifiedRelease,UpdateError>`、`UpdateManager::prepare/apply/rollback()->UpdateReport`、`UninstallManager::plan/apply()->UninstallReport`；`VerifiedRelease`须同时通过签名、版本约束、包hash，不用HTTPS/hash替代签名。`UpdateJournal { maintenance, pre_snapshot, old_version, new_version, schema_compatibility, spool_compatibility, consumption_resumed }` 持久标明是否允许快照回滚。

- [ ] **11.1 写失败测试：** `tampered_manifest_rejected`、`wrong_publisher_key_rejected`、`rollback_restores_matching_db_snapshot`、`interrupted_upgrade_recoverable`、`inflight_marked_uncertain_before_switch`、`uninstall_preserves_user_changed_hooks_and_data`、`reinstall_idempotent`。`maintenance_spools_lifecycle_and_notification`、`upgrade_concurrent_hooks_not_lost_or_replayed_twice`、`old_gui_write_rejected_during_maintenance`、`incompatible_spool_blocks_update`、`post_resume_downgrade_never_restores_stale_snapshot` 覆盖升级并发/失败恢复/恢复发送后的降级。验证二进制与数据快照一致；Launcher稳定，不改Hook字符串引发信任重置。
- [ ] **11.2 运行失败：** 规划命令 `cargo test -p agentnotify-platform --test update_rollback`；预期未实现或安全/恢复断言失败。
- [ ] **11.3 最小实现：** 各平台独立构建包与依赖声明，Windows处理WebView2，终端用户无需开发工具。更新先验证两版schema/spool兼容，再在admission互斥设置持久maintenance，所有Hook含生命周期只写v1spool并拒绝旧GUI配置写；随后停发送器、等待/标不确定、SQLite备份API快照、迁移/切换/健康检查，全程不让Hook写业务DB。失败恢复一致旧快照但保留全部spool；成功或回滚都先按序导入再开放消费，新事件继续spool不插队。恢复发送后禁止再还原升级前快照，降级须保留最新事件/投递/幂等并有验证过的兼容迁移，否则阻止自动降级。系统签名与更新签名分别记录，私钥只在发布环境；卸载默认保留数据凭据，仅移除仍匹配归属的配置。
- [ ] **11.4 通过与验收：** 重跑11.2；规划命令 `pwsh -File scripts/package.ps1 -Platform Windows -Channel Test` 产出隔离测试包；无正式证书只标未签名测试构建，不发布为正式包。核验offline安装说明、签名/散列、恢复日志；mac/Linux产包不自动升格为支持。输出 `docs/acceptance/m6-packaging.md`。

## 任务 12（M6-C）：Windows 干净实机与真实邮箱独立验收

**Files:** 创建 `tests/platform/windows/acceptance.ps1`、`docs/acceptance/windows/{matrix,runbook,results}.md`、`docs/compatibility/windows.md`；此任务不负责替实现者掩盖失败，也不回写“已通过”空模板。

**Interfaces:** Consumes：任务01–11交付的版本hash、签名包、配置/接入状态、明确的测试邮箱和发送授权；Produces：`AcceptanceRecord { date, os_build, arch, client_versions, binary_hash, steps, expected, actual, evidence, limitations, verdict }` 与仅由真实证据提升的兼容清单。

- [ ] **12.1 先写验收用例/失败条件：** 干净目标Windows安装；私密邮箱配置；保存不发信；Codex和Claude Code各自真实事件；GUI关闭仍通知；注销/重启登录恢复；免打扰/DST逻辑结果复核；断网/凭据失效/DB锁/磁盘满；重复Hook/两个后台；中文空格路径；升级中断恢复且期间新Started/Stop不丢不重；恢复发送后降级不丢最新记录；卸载默认保留数据；A/B机器切换不双发。watchdog默认关闭，能力具备再测试1800秒/可控缩短测试配置、进展/Stop竞争、重启/休眠不群发。任何缺证据行初始not_run，不得预填PASS。
- [ ] **12.2 运行前置失败门禁：** 规划命令 `pwsh -File tests/platform/windows/acceptance.ps1 -Mode Preflight`；缺客户端版本、签名包、测试邮箱或当前发送授权则输出缺项并停相应真实用例，不安装/改Hook/发信。确认Windows最低目标是否需要收窄；干净测试机优先避免影响用户正在使用的 AgentNotify 实例。
- [ ] **12.3 执行授权用例：** 安装与接入按明确预演执行，用户完成信任/必要重载；记录原生服务进程与真实Hook事件关联。真实发信仅在用户明确允许本次测试后执行一次 `agentnotify test --send`；用户确认收件并记消息ID/时间，不收集密码或业务正文。后续真实Hook会发邮件的测试批次也需明确授权数量/范围，不能靠一次试信授权无限发送。
- [ ] **12.4 性能与故障证据：** 规划命令 `pwsh -File tests/platform/windows/acceptance.ps1 -Mode Observe` 只采集本工具脱敏状态，不主动发信；记录空闲/并发/DB锁/spool回退的实际耗时分布、最大值与原始脱敏样本。正常入队目标<300ms、所有受控场景预算上限2秒；Hook无阻止/继续输出、AI任务正常推进。SMTP模糊结果默认不重试，修复auth后不过TTL。
- [ ] **12.5 独立判定：** 每项写预期/实际/证据，收件确认与SMTP accepted分列；至少一组明确授权真实收件证据是发布门禁。正常通知误过滤、泄漏、双发、Hook影响宿主、他人配置损坏、未知结果狂重试均阻塞。未通过返回对应责任任务整改，再只复核受影响范围；不能由作者自行声称独立验收通过。只有真实客户端/版本组合可标 `live_verified`。

## 任务 13（M7）：macOS/Linux 实机待办与兼容承诺收口

**Files:** 创建 `tests/platform/{macos,linux}/`、`docs/acceptance/{macos,linux}/matrix.md`、`docs/compatibility/{macos,linux,workbuddy-research}.md`。任务状态初始为未完成；不能因为Windows交付而自动打勾。

**Interfaces:** Consumes：共用核心与平台实现、任务12验收结构；Produces：按OS/build/架构记录的 `AcceptanceRecord`，以及公开兼容能力（`built_only/pending_real_machine/live_verified/unsupported`）。WorkBuddy产生正式接口研究记录，有依据后另开实现任务，当前只保持research_pending。

- [ ] **13.1 先列失败门禁：** macOS13+ arm64/x64分别验证Keychain访问/锁定、LaunchAgent、签名公证、GUI关闭后台、安装卸载；Ubuntu24.04 x64验证Secret Service已解锁/锁定/缺服务、systemd用户服务、WebKitGTK、XDG和会话退出恢复。无机器/权限/签名证据即 `blocked/not_run`，不填通过。
- [ ] **13.2 CI只做前置检查：** 规划命令 `cargo test --workspace --locked` 在各自原生runner执行逻辑/隔离测试；`pnpm --dir apps/desktop test:unit --run` 验证UI逻辑。预期给构建/测试证据，真实凭据测试若未启用则明确SKIPPED；不能把整个任务标PASS。
- [ ] **13.3 实机最小实施与验证：** 对各平台补齐其服务/凭据/安装器缺口，然后运行 `cargo test -p agentnotify-platform --test credentials_native -- --ignored --nocapture` 与平台验收手册；Linux缺Secret Service必须显示auth_blocked和Agent启用方案，不安装未知服务、不明文回退。OAuth回调/真实邮箱/客户端信任按平台重新授权和测试，不能继承Windows证明。
- [ ] **13.4 独立验收或保持待办：** 逐架构记录安装、关闭GUI、登录恢复、故障恢复、更新/卸载和收件证据；不满足则保留“待实测”，不妨碍已满足门禁的Windows单平台发行。WorkBuddy仅正式生命周期接口和实际版本证据齐备后才能从research_pending升级；CodeBuddy名称相近、占位配置或Agent提示词均不构成支持证据。

## 依赖与并行边界

`01 -> 02 -> {03,04} -> 05 -> 06 -> 07 -> {08,09} -> 10 -> 11 -> 12` 是保守执行次序；13的平台编译准备可提前，实机验收依赖对应平台完整实现。03/04先以02接口契约和去重测试fixture对齐，03不独立重写策略；最终必须联合验证。

沿用本会话已授权的委派方式，由主控按依赖分派：02接口冻结后可并行03队列与04策略；05凭据与06平台模块只有精确分开文件后并行，`platform/src/lib.rs`/公共类型指定单一负责人。07稳定DTO后08 UI与09 Agent包可并行；10迁移与11安装器须共享唯一Launcher/归属记录契约后再分工。12不得和待验收版本的持续改动并行，冻结版本hash；13不得覆盖Windows结果。所有工作者保留他人修改，公共文件变更先协调，不回退别人的工作；执行者不自授独立验收通过。

## 需求 → 任务 → 验收证据

| 设计要求 | 责任任务 | 通过所需证据 |
|---|---|---|
| 原生核心、锁版本、无需用户安装开发工具 | 01、11、12 | 原生后端实验、锁文件、干净Windows安装记录 |
| 统一语义、开始/进展与缺失metadata降级 | 02、04、07 | Schema/CLI契约测试、generation/10秒样本、真实客户端事件 |
| 不误滤Respire同形JSON、Unicode边界 | 04、07 | 来源+协议组合矩阵、UTF-16/UTF-8回归与脱敏样本hash |
| 三秒取消、十秒聚合、配额/摘要 | 04、05 | FakeClock断言、重试计入配额、独立错误与同会话分组 |
| 中立JSON边界、适配器私有metadata隔离 | 02、07 | 禁止依赖/字段检查、metadata失败降级、虚构适配器零核心改动 |
| actor隔离与生命周期原子入站/顺序降级 | 02、03、04、07 | 主/子代理隔离、完整envelope重放、gap与积压不插队测试 |
| 持久watchdog、能力限制、恢复与竞态 | 03、04、05、06、08、12 | Started/Progress模式、1800秒/15秒tick、Stop竞态、重启休眠不群发 |
| 跨夜/DST/全天静默/接收与发送复查 | 04、12 | UTC/IANA固定fixture、唯一实际区间key、真实规则配置证据 |
| WAL/并发/租约/spool/磁盘与路径安全 | 03、06、12 | 故障注入、真实安全目录/进程锁、Windows耗时样本 |
| 十种队列状态、TTL/重试/SMTP uncertain | 03、05、12 | 转换矩阵、模拟SMTP断点、用户显式重试与收件分离证据 |
| 正文默认关闭、AEAD/原生凭据/保留期 | 02、03、05 | 无明文诱饵测试、原生backend实测、终态清理/保留期测试 |
| OAuth、服务商预设、TLS/证书/标题安全 | 01、05、12 | 发布者注册状态、模拟安全测试、获准服务商实际收件证据 |
| 独立后台与登录恢复、能力不足不提权 | 06、12、13 | GUI退出后进程/投递、登录恢复、真实平台服务证据 |
| 多实例、接入状态、hash事务/信任 | 02、07、12 | 并发改配置与回滚测试、真实trust/reload/live_verified记录 |
| 六页面、受限Tauri命令、操作意图明确 | 08、12 | UI/命令测试、截图、原生窗口/后台独立证据 |
| 可直接复制Agent包、脱敏、范围授权 | 09 | 四场景+只读fixture、无占位符/无秘密/注入测试 |
| 机器迁移不双发、不搬运在途/历史队列 | 10、12 | 精确Hook归属、切换回滚/故障fixture、获准真实迁移证据 |
| 签名包/更新真实性/升级并发与降级 | 03、11、12、13 | admission维护gate、期间事件不丢不重、恢复发送后不还原旧快照、各平台签名/安装证据 |
| mac/Linux及WorkBuddy不夸大支持 | 07、13 | 待办矩阵、实机缺口、正式接口资料和兼容表证据级别 |

## 交付门禁与未决事项

- 正式源码目录尚未确定：开始实施前确认，不自动建仓或迁入demoAll。当前文档交付不受此阻塞。
- OAuth应用注册、发布身份、签名证书/更新发布渠道由发布者落实；缺失时阻塞相应正式能力/发行，不能以伪造配置填空。开发期间模拟与本地实验可独立推进。
- 真实邮箱及发送授权、当前客户端版本与信任动作、Windows干净测试机需在实际验证前落实；授权不足只运行不发信的检查。
- macOS/Linux实机为明确未完成待办；WorkBuddy正式事件研究未完成。发布说明只承诺已完成独立验收的平台/客户端组合。
- 计划要求每个任务独立证据和复核；最终验收还须确认所有证据指向同一交付版本。编译成功、UI截图、模拟SMTP、其他程序运行经验均不能代替新Rust产品真实验收。
- 本轮输出为设计与计划；上述复选框初始均未勾选，未创建产品代码、未安装运行、未发送邮件、未提交推送。完成本计划后交主控统一核验，与设计/提示词一致后发布到指定文档目录。

## 2026-11-02 单 EXE 交付调整

用户已选择一个 AgentNotify.exe 直接运行；GUI/CLI/daemon 合并为同一链接产物，说明单独 ZIP。当前分工、兼容约束、最少构建与独立验收见 `../superpowers/plans/2026-11-02-single-exe.md`。旧任务中的多程序包装不再作为本轮发布要求；真实客户端通知问题另列待自然事件验证。
