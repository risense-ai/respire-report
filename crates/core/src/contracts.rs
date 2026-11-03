//! 公共命令报告、严格状态枚举及通用中立协议入口。
use crate::event::*;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::io::Read;
use ts_rs::TS;
use uuid::Uuid;
pub const MAX_RAW_BYTES: usize = 1024 * 1024;
pub const MAX_METADATA_BYTES: usize = 8192;
pub const MAX_ID_BYTES: usize = 256;
pub const MAX_LABEL_BYTES: usize = 512;
pub const MAX_BODY_BYTES: usize = 16384;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
/// 脱敏输入错误；不携带原始JSON、正文、凭据或解析器错误文字。
pub struct InputError {
    pub code: String,
}
impl InputError {
    pub fn new(code: &str) -> Self {
        Self { code: code.into() }
    }
}
/// 外部输入先检查UTF-8字节长度，再解析，身份超限不截断。
pub fn validate_identity(value: &str) -> Result<(), InputError> {
    if value.is_empty() || value.len() > MAX_ID_BYTES || value.chars().any(char::is_control) {
        return Err(InputError::new("invalid_identity"));
    }
    Ok(())
}
/// 只多读一个字节用于识别超限，不读取无限stdin/文件。
pub fn read_bounded(reader: impl Read) -> Result<Vec<u8>, InputError> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_RAW_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| InputError::new("input_unavailable"))?;
    if bytes.len() > MAX_RAW_BYTES {
        return Err(InputError::new("input_too_large"));
    }
    Ok(bytes)
}
#[derive(Debug, Clone)]
/// 由本机接收边界注入；外部JSON不能替换实例、时间或密钥可用性。
pub struct IngressContext {
    pub instance_id: Uuid,
    pub received_at: DateTime<Utc>,
    pub content_key_available: bool,
}
/// 解析唯一中立generic协议；无原生字段识别、来源分类或客户端扫描。
pub fn parse_generic(raw: &[u8], context: &IngressContext) -> Result<AdapterOutput, InputError> {
    // 1. 首先限制原始读取预算，并识别版本；不将serde错误文字返回给调用方。
    if raw.len() > MAX_RAW_BYTES {
        return Err(InputError::new("input_too_large"));
    }
    let mut value: serde_json::Value =
        serde_json::from_slice(raw).map_err(|_| InputError::new("invalid_input"))?;
    let obj = value
        .as_object_mut()
        .ok_or_else(|| InputError::new("invalid_input"))?;
    if obj.get("schema_version").and_then(|v| v.as_u64()) != Some(1) {
        return Err(InputError::new("unsupported_schema"));
    }
    // 2. 保留字段直接拒绝，绝不将外部标签当成受信分类或本机计时证据。
    const RESERVED: &[&str] = &[
        "source_evidence",
        "source_class",
        "origin_evidence",
        "classification_reason",
        "background_result",
        "received_at",
        "watchdog_detail",
        "content_key_available",
        "lifecycle",
        "per_instance_sequence",
        "ingress_id",
        "source",
    ];
    if RESERVED.iter().any(|k| obj.contains_key(*k)) {
        return Err(InputError::new("reserved_field"));
    }
    let instance: Uuid = serde_json::from_value(
        obj.get("instance_id")
            .cloned()
            .ok_or_else(|| InputError::new("invalid_identity"))?,
    )
    .map_err(|_| InputError::new("invalid_identity"))?;
    if instance != context.instance_id {
        return Err(InputError::new("context_mismatch"));
    }
    // 接收时间来自当前用户入站上下文，仅用于本机排序及TTL。
    obj.insert(
        "received_at".into(),
        serde_json::to_value(context.received_at).map_err(|_| InputError::new("invalid_input"))?,
    );
    let mut event: NormalizedEventV1 =
        serde_json::from_value(value).map_err(|_| InputError::new("invalid_input"))?;
    event.content_key_available = context.content_key_available;
    // generic的显式完成只能表述为调用方报告；不能自称官方任务事件或看门狗计时。
    if event.kind == EventKind::TaskCompleted
        && event.completion_basis != CompletionBasis::ExplicitCallerReport
        || event.kind == EventKind::Stalled
    {
        return Err(InputError::new("invalid_completion"));
    }
    // 3. 共同证据固定caller_reported/unknown，绝不由外部source或正文提升信任。
    let output = AdapterOutput {
        notification: Some(event),
        lifecycle: None,
        source_evidence: SourceEvidence {
            source_class: SourceClass::Unknown,
            classification_reason: "generic_caller_report".into(),
            origin_evidence: OriginEvidence::CallerReported,
            background_result: BackgroundResult::Unknown,
        },
    };
    output.validate()?;
    Ok(output)
}
contract_enum! { /// 接入状态描述证据阶段，配置存在不代表真实接入通过。
pub enum IntegrationStatus { NotDetected, Detected, UnsupportedVersion, ResearchPending, Configured, TrustPending, ReloadPending, ProbePassed, LiveVerified, Degraded } }
contract_enum! { /// 投递状态；Sent仅表示SMTP接受，收件确认是独立证据。
pub enum QueueStatus { Queued, Leased, Sent, Suppressed, DigestPending, RetryWait, AuthBlocked, DeliveryUnknown, Expired, PermanentFailed } }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
/// 结构化命令错误；message必须来自受控诊断，不回显参数/正文。
pub struct CommandError {
    pub code: String,
    pub message: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct CommandReport<T> {
    #[schemars(range(min = 1, max = 1))]
    pub schema_version: u8,
    pub ok: bool,
    pub data: Option<T>,
    pub error: Option<CommandError>,
}
impl<T> CommandReport<T> {
    pub fn success(data: T) -> Self {
        Self {
            schema_version: 1,
            ok: true,
            data: Some(data),
            error: None,
        }
    }
    pub fn failure(code: &str, message: &str, data: Option<T>) -> Self {
        Self {
            schema_version: 1,
            ok: false,
            data,
            error: Some(CommandError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
/// 仅真正持久化后返回；三者id都指接收单元而非邮件。
pub enum EnqueueOutcome {
    Queued { id: Uuid },
    Duplicate { id: Uuid },
    Spooled { id: Uuid },
}
/// 队列依赖的中立入口，admission实现由后续任务注入。
pub trait Ingress {
    fn accept(&self, output: AdapterOutput) -> Result<EnqueueOutcome, QueueError>;
}
/// JavaScript/JSON跨Rust与UI的无损整数上限；序号、generation和经过秒数共用。
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
/// 数字构造入口及序列化边界共用检查，避免先舍入再比较身份/顺序。
pub fn validate_safe_integer(value: u64) -> Result<(), InputError> {
    if value > MAX_SAFE_INTEGER {
        Err(InputError::new("integer_out_of_range"))
    } else {
        Ok(())
    }
}
/// 保持计划的u64 Rust类型，同时在JSON双向边界拒绝JS不能无损表示的值。
pub mod safe_u64 {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        super::validate_safe_integer(*value)
            .map_err(|_| serde::ser::Error::custom("integer_out_of_range"))?;
        value.serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        let value = u64::deserialize(deserializer)?;
        super::validate_safe_integer(value)
            .map_err(|_| serde::de::Error::custom("integer_out_of_range"))?;
        Ok(value)
    }
}
/// 可空序号保留null语义；有值时执行同一JSON安全整数校验。
pub mod safe_optional_u64 {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer>(value: &Option<u64>, serializer: S) -> Result<S::Ok, S::Error> {
        if let Some(value) = value {
            super::validate_safe_integer(*value)
                .map_err(|_| serde::ser::Error::custom("integer_out_of_range"))?;
        }
        value.serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<u64>, D::Error> {
        let value = Option::<u64>::deserialize(deserializer)?;
        if let Some(value) = value {
            super::validate_safe_integer(value)
                .map_err(|_| serde::de::Error::custom("integer_out_of_range"))?;
        }
        Ok(value)
    }
}
contract_enum! { /// 入站/队列失败，区别于外部输入错误；不得携带原文、路径或存储错误详情。
pub enum QueueErrorCode { InvalidInput, StorageUnavailable, StorageFailed, AdmissionTimeout, QueueFull, SpoolFull, BodyProtectionUnavailable, UnsupportedSchema } }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct QueueError {
    pub code: QueueErrorCode,
}
/// DB与spool由后续实现接通，本契约不声称事件已持久化。
pub trait Queue {
    fn ingest(&self, envelope: IngressEnvelopeV1) -> Result<EnqueueOutcome, QueueError>;
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "decision", rename_all = "snake_case", deny_unknown_fields)]
/// 策略只返回中立决策；证据必须由独立参数传入，不从来源标签推断。
pub enum PolicyDecision {
    Notify { not_before: DateTime<Utc> },
    Suppress { reason: String },
    Digest { window_key: String },
    Defer { until: DateTime<Utc> },
}
pub trait Policy {
    fn decide(
        &self,
        event: &NormalizedEventV1,
        evidence: &SourceEvidence,
        config: &crate::config::ConfigV1,
        now: DateTime<Utc>,
    ) -> PolicyDecision;
}
contract_enum! { /// DATA前必须持久化DataStarted；不能凭高层发送结果猜测阶段。
pub enum SendStage { Prepared, Connecting, Envelope, DataStarted, Accepted } }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
/// Accepted仅表示SMTP接受，Uncertain默认不可自动重发，不隐含收件人确认。
pub enum DeliveryOutcome {
    Accepted { message_id: String },
    Retryable { code: String },
    AuthBlocked { code: String },
    PermanentFailure { code: String },
    Uncertain { stage: SendStage },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
/// 统一拒绝非法升级；报告只含受控状态，不携带客户端配置。
pub struct IntegrationTransitionError {
    pub code: String,
    pub from: IntegrationStatus,
    pub to: IntegrationStatus,
}
impl IntegrationStatus {
    /// 只校验允许的证据阶段，调用方仍须取得实际证据；本函数不会执行探针或改配置。
    pub fn transition_to(self, next: Self) -> Result<Self, IntegrationTransitionError> {
        use IntegrationStatus::*;
        let allowed = self == next
            || matches!(
                (self, next),
                (NotDetected, Detected)
                    | (
                        Detected,
                        NotDetected | UnsupportedVersion | ResearchPending | Configured
                    )
                    | (UnsupportedVersion | ResearchPending, NotDetected | Detected)
                    | (
                        Configured,
                        NotDetected
                            | Detected
                            | TrustPending
                            | ReloadPending
                            | ProbePassed
                            | Degraded
                    )
                    | (
                        TrustPending,
                        NotDetected | Detected | Configured | ReloadPending | Degraded
                    )
                    | (
                        ReloadPending,
                        NotDetected | Detected | Configured | Degraded
                    )
                    | (
                        ProbePassed,
                        NotDetected | Detected | Configured | LiveVerified | Degraded
                    )
                    | (LiveVerified, NotDetected | Detected | Configured | Degraded)
                    | (Degraded, NotDetected | Detected | Configured)
            );
        if allowed {
            Ok(next)
        } else {
            Err(IntegrationTransitionError {
                code: "invalid_integration_transition".into(),
                from: self,
                to: next,
            })
        }
    }
}
contract_enum! { /// 健康报告仅描述后台观察结果，不混同GUI窗口状态。
pub enum DaemonStatus { Running, Stopped, Unavailable, Unknown } }
contract_enum! { /// 凭据状态为白名单，不导出账号、授权码或后端错误详情。
pub enum CredentialStatus { Ready, Missing, Locked, AccessDenied, Unavailable, Unknown } }
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
/// 十种队列状态对应的数量，u32可被UI无损表示，未知状态不能混入字典。
pub struct QueueCounts {
    pub queued: u32,
    pub leased: u32,
    pub sent: u32,
    pub suppressed: u32,
    pub digest_pending: u32,
    pub retry_wait: u32,
    pub auth_blocked: u32,
    pub delivery_unknown: u32,
    pub expired: u32,
    pub permanent_failed: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct IntegrationHealth {
    pub instance_id: Uuid,
    pub adapter_id: String,
    pub status: IntegrationStatus,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
/// 只准使用脱敏受控文案，禁止装载原始日志、配置或凭据。
pub struct HealthIssue {
    pub code: String,
    pub message: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
/// doctor及UI共用白名单DTO；本轮没有把未实现后端伪装为健康。
pub struct HealthReport {
    #[schemars(range(min = 1, max = 1))]
    pub schema_version: u8,
    pub daemon: DaemonStatus,
    pub queue_counts: QueueCounts,
    pub credential_status: CredentialStatus,
    pub integrations: Vec<IntegrationHealth>,
    pub issues: Vec<HealthIssue>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
/// 本机安全存储的随机条目引用，不包含邮箱地址或任何秘密值。
pub struct CredentialRef {
    pub id: Uuid,
}
contract_enum! { /// 平台凭据错误只返回受控原因，不输出系统错误详情或秘密。
pub enum CredentialErrorCode { Missing, Locked, AccessDenied, Unavailable, WriteFailed, ReadbackFailed } }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct CredentialError {
    pub code: CredentialErrorCode,
}
/// 秘密输入只在内存受限持有；不实现Debug/Serialize/Clone，释放时清零缓冲区。
pub struct SecretInput {
    bytes: zeroize::Zeroizing<Vec<u8>>,
}
impl SecretInput {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes: zeroize::Zeroizing::new(bytes),
        }
    }
    /// 只有凭据/传输边界显式借用；调用方不得把借用值复制到日志或配置。
    pub fn with_secret<T>(&self, consumer: impl FnOnce(&[u8]) -> T) -> T {
        consumer(&self.bytes)
    }
}
/// 生产系统安全存储取回的秘密句柄；不实现Debug/Serialize/Clone，不存在明文文件后备。
pub struct CredentialHandle {
    secret: SecretInput,
    revision: Option<CredentialRevision>,
}
/// 安全存储的不可序列化版本凭据，仅用于刷新后的条件替换；不进入配置、Schema或日志。
/// 后端必须使有效秘密每次发布获得不同版本，删除后旧版本不能再创建该引用。
#[derive(PartialEq, Eq)]
pub struct CredentialRevision([u8; 32]);
impl CredentialRevision {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}
impl CredentialHandle {
    pub fn from_secret(secret: SecretInput) -> Self {
        Self {
            secret,
            revision: None,
        }
    }
    /// 只有支持原子条件替换的安全存储后端才能为读取结果附加版本。
    pub fn from_versioned_secret(secret: SecretInput, revision: CredentialRevision) -> Self {
        Self {
            secret,
            revision: Some(revision),
        }
    }
    pub fn revision(&self) -> Option<&CredentialRevision> {
        self.revision.as_ref()
    }
    pub fn with_secret<T>(&self, consumer: impl FnOnce(&[u8]) -> T) -> T {
        self.secret.with_secret(consumer)
    }
}
/// 原生实现由platform注入；get/put/delete都不得在配置、日志或参数中暴露秘密。
pub trait CredentialStore {
    fn get(&self, reference: &CredentialRef) -> Result<CredentialHandle, CredentialError>;
    fn put(&self, reference: &CredentialRef, input: SecretInput) -> Result<(), CredentialError>;
    fn delete(&self, reference: &CredentialRef) -> Result<(), CredentialError>;
    /// 仅当引用仍存在且版本与读取快照相同才替换；false表示已删除/变化，绝不能创建。
    /// 检查和发布须由同一跨进程临界区保护；默认失败关闭，禁止用get+put模拟CAS。
    fn replace_if_revision(
        &self,
        _reference: &CredentialRef,
        _expected: &CredentialRevision,
        _input: SecretInput,
    ) -> Result<bool, CredentialError> {
        Err(CredentialError {
            code: CredentialErrorCode::Unavailable,
        })
    }
}
/// 仅瞬态传输输入，不实现Debug/Serialize；正文受控借用且释放清零，不进入诊断DTO。
pub struct PreparedMail {
    pub message_id: String,
    pub from: String,
    pub recipients: Vec<String>,
    pub subject: String,
    pub body: SecretInput,
    /// 可选HTML替代正文，同属瞬态秘密输入，不进入诊断或持久DTO。
    pub html_body: Option<SecretInput>,
}
/// 只声明中立接口；真实TLS/SMTP及阶段持久化由后续任务实现。
pub trait Transport {
    fn send(&self, mail: &PreparedMail, credentials: &CredentialHandle) -> DeliveryOutcome;
}
/// 由同一套Rust模型确定性导出Schema和TS；生成器和漂移检查共用此入口。
pub fn generated_artifacts() -> Vec<(&'static str, String)> {
    use crate::config::*;
    let mut generator = schemars::SchemaGenerator::default();
    // 公共类型即使不是入站字段，也以同一Rust定义加入$defs，避免Schema/TS各自猜测。
    let _ = generator.subschema_for::<QueueError>();
    let _ = generator.subschema_for::<PolicyDecision>();
    let _ = generator.subschema_for::<DeliveryOutcome>();
    let _ = generator.subschema_for::<HealthReport>();
    let _ = generator.subschema_for::<IntegrationTransitionError>();
    let _ = generator.subschema_for::<ActorKey>();
    let _ = generator.subschema_for::<ActivityKey>();
    let _ = generator.subschema_for::<QueueStatus>();
    let _ = generator.subschema_for::<CredentialRef>();
    let _ = generator.subschema_for::<CredentialError>();
    let _ = generator.subschema_for::<EnqueueOutcome>();
    let _ = generator.subschema_for::<CommandReport<HealthReport>>();
    let event_schema =
        serde_json::to_string_pretty(&generator.into_root_schema_for::<IngressEnvelopeV1>())
            .expect("serializable schema")
            + "\n";
    let config_schema = serde_json::to_string_pretty(&schemars::schema_for!(ConfigV1))
        .expect("serializable schema")
        + "\n";
    let declarations = [
        EventKind::decl(),
        CompletionBasis::decl(),
        ActorScope::decl(),
        AgentKind::decl(),
        BackgroundState::decl(),
        SourceClass::decl(),
        OriginEvidence::decl(),
        BackgroundResult::decl(),
        LifecycleKind::decl(),
        WatchdogMode::decl(),
        QuietBehavior::decl(),
        PresenceMode::decl(),
        PresenceConfig::decl(),
        SourceEvidence::decl(),
        WatchdogDetail::decl(),
        NormalizedEventV1::decl(),
        LifecycleSignal::decl(),
        AdapterOutput::decl(),
        IngressEnvelopeV1::decl(),
        ActorKey::decl(),
        TurnKey::decl(),
        ActivityKey::decl(),
        QuietConfig::decl(),
        WatchdogConfig::decl(),
        AdapterCapabilities::decl(),
        NotificationRules::decl(),
        ConfigV1::decl(),
        InputError::decl(),
        IntegrationStatus::decl(),
        QueueStatus::decl(),
        CommandError::decl(),
        CommandReport::<String>::decl(),
        EnqueueOutcome::decl(),
        QueueErrorCode::decl(),
        QueueError::decl(),
        PolicyDecision::decl(),
        SendStage::decl(),
        DeliveryOutcome::decl(),
        IntegrationTransitionError::decl(),
        DaemonStatus::decl(),
        CredentialStatus::decl(),
        QueueCounts::decl(),
        IntegrationHealth::decl(),
        HealthIssue::decl(),
        HealthReport::decl(),
        CredentialRef::decl(),
        CredentialErrorCode::decl(),
        CredentialError::decl(),
    ];
    let mut ts = String::from(
        "// 自动生成：cargo run -p agentnotify-core --example export_contracts；请勿手工编辑。\n// 时间为UTC RFC3339；字节预算及跨字段语义由Rust运行时校验。\n",
    );
    for declaration in declarations {
        ts.push_str("export ");
        // ts-rs在doc注释前保留空格；确定性清理每行尾空白，生成文件不得手改。
        ts.push_str(
            &declaration
                .lines()
                .map(str::trim_end)
                .collect::<Vec<_>>()
                .join("\n"),
        );
        ts.push('\n');
    }
    vec![
        ("schemas/event-v1.schema.json", event_schema),
        ("schemas/config-v1.schema.json", config_schema),
        ("apps/desktop/src/generated/contracts.ts", ts),
    ]
}
