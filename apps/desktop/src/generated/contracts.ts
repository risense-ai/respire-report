// 自动生成：cargo run -p agentnotify-core --example export_contracts；请勿手工编辑。
// 时间为UTC RFC3339；字节预算及跨字段语义由Rust运行时校验。
export type EventKind = "response_finished" | "attention_required" | "execution_failed" | "interrupted" | "task_completed" | "stalled";
export type CompletionBasis = "response_stop" | "explicit_task_event" | "explicit_caller_report" | "none";
export type ActorScope = "main" | "identified" | "unknown";
export type AgentKind = "main" | "subagent" | "background" | "unknown";
export type BackgroundState = "none" | "pending" | "unknown";
export type SourceClass = "user" | "subagent" | "internal_background" | "unknown";
export type OriginEvidence = "verified_adapter" | "caller_reported" | "unknown";
export type BackgroundResult = "success" | "other" | "unknown";
export type LifecycleKind = "started" | "progress" | "resumed" | "session_closed";
export type WatchdogMode = "elapsed_without_finish" | "inactivity";
export type QuietBehavior = "skip" | "digest";
export type PresenceMode = "always" | "away_only";
export type PresenceConfig = { mode: PresenceMode,
/**
 * 最近输入距今的秒数；锁屏无需等到该阈值。
 */
idle_seconds: number, };
export type SourceEvidence = { source_class: SourceClass, classification_reason: string, origin_evidence: OriginEvidence, background_result: BackgroundResult, };
export type WatchdogDetail = { mode: WatchdogMode, threshold_seconds: number, elapsed_seconds: number, last_observed_at: string, };
export type NormalizedEventV1 = { schema_version: number,
/**
 * 已登记适配器标签；不作为可信来源证明。
 */
adapter_id: string, instance_id: string,
/**
 * 上游可证稳定事件ID；缺失保持null，不用时间伪造。
 */
event_id: string | null, session_id: string | null, turn_id: string | null, parent_session_id: string | null, agent_kind: AgentKind, actor_id: string | null, actor_scope: ActorScope, kind: EventKind, completion_basis: CompletionBasis, source_time: string | null,
/**
 * 由本机接收上下文提供，不能由外部输入控制TTL。
 */
received_at: string, project_label: string | null, session_label: string | null, question: string | null, answer: string | null, background_state: BackgroundState,
/**
 * 仅适配器有官方依据时设置；generic禁止外部声明跳转链接。
 */
resume_url: string | null, watchdog_detail: WatchdogDetail | null, };
export type LifecycleSignal = { instance_id: string, session_id: string, actor_id: string | null, actor_scope: ActorScope, turn_id: string | null, upstream_id: string | null, kind: LifecycleKind, received_at: string, };
export type AdapterOutput = { notification: NormalizedEventV1 | null, lifecycle: LifecycleSignal | null, source_evidence: SourceEvidence, };
export type IngressEnvelopeV1 = { schema_version: number, ingress_id: string, received_at: string, per_instance_sequence: number | null,
/**
 * 接收时本机离席策略的抑制决定；true不可因晚导入或改配置复活。旧spool缺失按false兼容。
 */
presence_suppressed: boolean, adapter_output: AdapterOutput, };
export type ActorKey = { "scope": "main", instance_id: string, session_id: string, } | { "scope": "identified", instance_id: string, session_id: string, actor_id: string, } | { "scope": "unknown", ingress_id: string, };
export type TurnKey = { "kind": "upstream", "value": string } | { "kind": "generation", "value": number };
export type ActivityKey = { actor: ActorKey, turn: TurnKey, };
export type QuietConfig = { enabled: boolean, start: string, end: string, timezone: string, behavior: QuietBehavior, };
export type WatchdogConfig = { enabled: boolean, mode: WatchdogMode, threshold_seconds: number, };
export type AdapterCapabilities = { reliable_started: boolean, reliable_progress: boolean, reliable_actor: boolean, reliable_turn_or_generation: boolean, };
export type NotificationRules = {
/**
 * 正常结束聚合；关闭时仍等待3秒。
 */
aggregate_finishes: boolean,
/**
 * 已核验内部后台且成功证据同时成立才允许过滤。
 */
suppress_internal_success: boolean,
/**
 * 子代理默认保留，不依据正文猜测来源。
 */
notify_subagents: boolean,
/**
 * 等待输入/批准默认关闭。
 */
notify_attention: boolean, };
export type ConfigV1 = { schema_version: number, enabled: boolean, include_question: boolean, include_answer: boolean, quiet: QuietConfig, metadata_retention_days: number, queue_ttl_hours: number, watchdog: WatchdogConfig, policy: NotificationRules, presence: PresenceConfig, };
export type InputError = { code: string, };
export type IntegrationStatus = "not_detected" | "detected" | "unsupported_version" | "research_pending" | "configured" | "trust_pending" | "reload_pending" | "probe_passed" | "live_verified" | "degraded";
export type QueueStatus = "queued" | "leased" | "sent" | "suppressed" | "digest_pending" | "retry_wait" | "auth_blocked" | "delivery_unknown" | "expired" | "permanent_failed";
export type CommandError = { code: string, message: string, };
export type CommandReport<T> = { schema_version: number, ok: boolean, data: T | null, error: CommandError | null, };
export type EnqueueOutcome = { "status": "queued", id: string, } | { "status": "duplicate", id: string, } | { "status": "spooled", id: string, };
export type QueueErrorCode = "invalid_input" | "storage_unavailable" | "storage_failed" | "admission_timeout" | "queue_full" | "spool_full" | "body_protection_unavailable" | "unsupported_schema";
export type QueueError = { code: QueueErrorCode, };
export type PolicyDecision = { "decision": "notify", not_before: string, } | { "decision": "suppress", reason: string, } | { "decision": "digest", window_key: string, } | { "decision": "defer", until: string, };
export type SendStage = "prepared" | "connecting" | "envelope" | "data_started" | "accepted";
export type DeliveryOutcome = { "outcome": "accepted", message_id: string, } | { "outcome": "retryable", code: string, } | { "outcome": "auth_blocked", code: string, } | { "outcome": "permanent_failure", code: string, } | { "outcome": "uncertain", stage: SendStage, };
export type IntegrationTransitionError = { code: string, from: IntegrationStatus, to: IntegrationStatus, };
export type DaemonStatus = "running" | "stopped" | "unavailable" | "unknown";
export type CredentialStatus = "ready" | "missing" | "locked" | "access_denied" | "unavailable" | "unknown";
export type QueueCounts = { queued: number, leased: number, sent: number, suppressed: number, digest_pending: number, retry_wait: number, auth_blocked: number, delivery_unknown: number, expired: number, permanent_failed: number, };
export type IntegrationHealth = { instance_id: string, adapter_id: string, status: IntegrationStatus, };
export type HealthIssue = { code: string, message: string, };
export type HealthReport = { schema_version: number, daemon: DaemonStatus, queue_counts: QueueCounts, credential_status: CredentialStatus, integrations: Array<IntegrationHealth>, issues: Array<HealthIssue>, };
export type CredentialRef = { id: string, };
export type CredentialErrorCode = "missing" | "locked" | "access_denied" | "unavailable" | "write_failed" | "readback_failed";
export type CredentialError = { code: CredentialErrorCode, };
