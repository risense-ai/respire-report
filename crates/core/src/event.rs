//! 统一通知、生命周期和共同来源证据；这里不识别任何厂商字段。
use crate::{config::WatchdogMode, contracts::InputError};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

macro_rules! contract_enum {
    ($(#[$meta:meta])* pub enum $name:ident { $($item:ident),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
        #[serde(rename_all="snake_case")]
        pub enum $name { $($item),+ }
    }
}
pub(crate) use contract_enum;
contract_enum! { /// 六种通知语义；响应结束不代表业务验收成功。
pub enum EventKind { ResponseFinished, AttentionRequired, ExecutionFailed, Interrupted, TaskCompleted, Stalled } }
contract_enum! { /// 完成证据；普通Stop只能使用ResponseStop。
pub enum CompletionBasis { ResponseStop, ExplicitTaskEvent, ExplicitCallerReport, None } }
contract_enum! { /// 执行主体范围；未知主体不得借父会话聚合或取消其他主体。
pub enum ActorScope { Main, Identified, Unknown } }
contract_enum! { /// 上游代理角色；Unknown不能推断成内部后台。
pub enum AgentKind { Main, Subagent, Background, Unknown } }
contract_enum! { /// 上游剩余后台任务信息；Pending/Unknown不表示全部结束。
pub enum BackgroundState { None, Pending, Unknown } }
contract_enum! { /// 受控来源分类；来源标签本身不是证明。
pub enum SourceClass { User, Subagent, InternalBackground, Unknown } }
contract_enum! { /// 分类证据强度；Generic只能生成CallerReported。
pub enum OriginEvidence { VerifiedAdapter, CallerReported, Unknown } }
contract_enum! { /// 后台结果证据；Unknown/Other不可默认静默。
pub enum BackgroundResult { Success, Other, Unknown } }
contract_enum! { /// 生命周期不产生完成通知；SessionClosed仅关闭会话活动。
pub enum LifecycleKind { Started, Progress, Resumed, SessionClosed } }
impl Default for ActorScope {
    fn default() -> Self {
        Self::Unknown
    }
}
impl Default for AgentKind {
    fn default() -> Self {
        Self::Unknown
    }
}
impl Default for BackgroundState {
    fn default() -> Self {
        Self::Unknown
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
/// 通知和纯生命周期共用的一份证据；诊断原因不参与信任判定。
pub struct SourceEvidence {
    pub source_class: SourceClass,
    pub classification_reason: String,
    pub origin_evidence: OriginEvidence,
    pub background_result: BackgroundResult,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
/// 看门狗计时证据，只能由核心计时器生成；疑似状态不是卡死证明。
pub struct WatchdogDetail {
    pub mode: WatchdogMode,
    pub threshold_seconds: u32,
    #[ts(type = "number")]
    #[serde(with = "crate::contracts::safe_u64")]
    #[schemars(with = "u64", range(max = 9007199254740991_u64))]
    pub elapsed_seconds: u64,
    pub last_observed_at: DateTime<Utc>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
/// 中立通知；正文是短暂输入，落盘前必须经过privacy并由存储层加密。
pub struct NormalizedEventV1 {
    #[schemars(range(min = 1, max = 1))]
    pub schema_version: u8,
    /// 已登记适配器标签；不作为可信来源证明。
    pub adapter_id: String,
    pub instance_id: Uuid,
    /// 上游可证稳定事件ID；缺失保持null，不用时间伪造。
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub turn_id: Option<String>,
    #[serde(default)]
    pub parent_session_id: Option<String>,
    #[serde(default)]
    pub agent_kind: AgentKind,
    #[serde(default)]
    pub actor_id: Option<String>,
    #[serde(default)]
    pub actor_scope: ActorScope,
    pub kind: EventKind,
    pub completion_basis: CompletionBasis,
    #[serde(default)]
    pub source_time: Option<DateTime<Utc>>,
    /// 由本机接收上下文提供，不能由外部输入控制TTL。
    pub received_at: DateTime<Utc>,
    #[serde(default)]
    pub project_label: Option<String>,
    #[serde(default)]
    pub session_label: Option<String>,
    #[serde(default)]
    pub question: Option<String>,
    #[serde(default)]
    pub answer: Option<String>,
    #[serde(default)]
    pub background_state: BackgroundState,
    /// 仅适配器有官方依据时设置；generic禁止外部声明跳转链接。
    #[serde(default)]
    pub resume_url: Option<String>,
    #[serde(default)]
    pub watchdog_detail: Option<WatchdogDetail>,
    /// 接收上下文注入的安全密钥可用性，不属于外部JSON/Schema/TS协议。
    #[serde(skip)]
    #[ts(skip)]
    pub content_key_available: bool,
}
impl NormalizedEventV1 {
    /// 展示只表达已有证据，不将响应结束或调用方报告升级为业务成功。
    pub fn display_kind(&self) -> &'static str {
        match self.kind {
            EventKind::ResponseFinished => "本轮处理结束",
            EventKind::TaskCompleted
                if self.completion_basis == CompletionBasis::ExplicitCallerReport =>
            {
                "调用方报告完成"
            }
            EventKind::TaskCompleted => "收到明确任务完成事件",
            EventKind::AttentionRequired => "需要处理",
            EventKind::ExecutionFailed => "执行失败",
            EventKind::Interrupted => "本轮已中断",
            EventKind::Stalled => match self.watchdog_detail.as_ref().map(|d| d.mode) {
                Some(WatchdogMode::Inactivity) => "疑似长时间无进展",
                _ => "本轮运行时间较长，尚未收到结束事件",
            },
        }
    }
    pub fn project_title(&self) -> &str {
        self.project_label
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("项目未提供")
    }
    pub fn session_title(&self) -> &str {
        self.session_label
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("会话未提供")
    }
    /// 校验语义和UTF-8字节预算，身份超限拒绝而不是截断。
    pub fn validate(&self) -> Result<(), InputError> {
        if self.schema_version != 1 {
            return Err(InputError::new("unsupported_schema"));
        }
        crate::contracts::validate_identity(&self.adapter_id)?;
        for id in [
            &self.event_id,
            &self.session_id,
            &self.turn_id,
            &self.parent_session_id,
            &self.actor_id,
        ]
        .into_iter()
        .flatten()
        {
            crate::contracts::validate_identity(id)?;
        }
        if self.actor_scope == ActorScope::Identified
            && (self.actor_id.is_none() || self.session_id.is_none())
        {
            return Err(InputError::new("invalid_identity"));
        }
        for label in [&self.project_label, &self.session_label]
            .into_iter()
            .flatten()
        {
            if label.len() > 512 || label.contains(['\r', '\n']) {
                return Err(InputError::new("invalid_label"));
            }
        }
        let valid = match self.kind {
            EventKind::TaskCompleted => matches!(
                self.completion_basis,
                CompletionBasis::ExplicitTaskEvent | CompletionBasis::ExplicitCallerReport
            ),
            EventKind::ResponseFinished => self.completion_basis == CompletionBasis::ResponseStop,
            _ => self.completion_basis == CompletionBasis::None,
        };
        if !valid {
            return Err(InputError::new("invalid_completion"));
        }
        if (self.kind == EventKind::Stalled) != self.watchdog_detail.is_some() {
            return Err(InputError::new("invalid_watchdog"));
        }
        if let Some(d) = &self.watchdog_detail {
            crate::contracts::validate_safe_integer(d.elapsed_seconds)?;
            if !(60..=86400).contains(&d.threshold_seconds)
                || d.elapsed_seconds < u64::from(d.threshold_seconds)
            {
                return Err(InputError::new("invalid_watchdog"));
            }
        }
        // 无官方scheme注册表的M1不接受跳转，避免把模型文字变成可执行链接。
        if self.resume_url.is_some() {
            return Err(InputError::new("unsupported_resume_url"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
/// 生命周期的actor字段与通知使用同一枚举，不生成虚构轮次。
pub struct LifecycleSignal {
    pub instance_id: Uuid,
    pub session_id: String,
    pub actor_id: Option<String>,
    pub actor_scope: ActorScope,
    pub turn_id: Option<String>,
    pub upstream_id: Option<String>,
    pub kind: LifecycleKind,
    pub received_at: DateTime<Utc>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
/// 一个接收单元的通知、生命周期及共同证据，不允许双份来源漂移。
pub struct AdapterOutput {
    pub notification: Option<NormalizedEventV1>,
    pub lifecycle: Option<LifecycleSignal>,
    pub source_evidence: SourceEvidence,
}
impl AdapterOutput {
    /// 包含无正文元数据预算；正文关闭/截断由privacy独立处理。
    pub fn validate(&self) -> Result<(), InputError> {
        if self.notification.is_none() && self.lifecycle.is_none() {
            return Err(InputError::new("empty_output"));
        }
        if let Some(event) = &self.notification {
            event.validate()?;
        }
        if let Some(signal) = &self.lifecycle {
            crate::contracts::validate_identity(&signal.session_id)?;
            for id in [&signal.actor_id, &signal.turn_id, &signal.upstream_id]
                .into_iter()
                .flatten()
            {
                crate::contracts::validate_identity(id)?;
            }
            if signal.actor_scope == ActorScope::Identified && signal.actor_id.is_none() {
                return Err(InputError::new("invalid_identity"));
            }
            if let Some(event) = &self.notification {
                if event.instance_id != signal.instance_id
                    || event.session_id.as_deref() != Some(&signal.session_id)
                    || event.actor_id != signal.actor_id
                    || event.actor_scope != signal.actor_scope
                    || event.turn_id != signal.turn_id
                {
                    return Err(InputError::new("context_mismatch"));
                }
            }
        }
        let mut metadata = self.clone();
        if let Some(e) = &mut metadata.notification {
            e.question = None;
            e.answer = None;
        }
        if serde_json::to_vec(&metadata)
            .map_err(|_| InputError::new("invalid_input"))?
            .len()
            > 8192
        {
            return Err(InputError::new("metadata_too_large"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
/// DB和spool唯一共同输入；序号为空必须显示order_unknown诊断。
pub struct IngressEnvelopeV1 {
    #[schemars(range(min = 1, max = 1))]
    pub schema_version: u8,
    pub ingress_id: Uuid,
    pub received_at: DateTime<Utc>,
    #[ts(type = "number | null")]
    #[serde(default, with = "crate::contracts::safe_optional_u64")]
    #[schemars(with = "Option<u64>", range(max = 9007199254740991_u64))]
    pub per_instance_sequence: Option<u64>,
    /// 接收时本机离席策略的抑制决定；true不可因晚导入或改配置复活。旧spool缺失按false兼容。
    #[serde(default)]
    pub presence_suppressed: bool,
    pub adapter_output: AdapterOutput,
}
impl IngressEnvelopeV1 {
    pub fn diagnostics(&self) -> Vec<&'static str> {
        if self.per_instance_sequence.is_none() {
            vec!["order_unknown"]
        } else {
            vec![]
        }
    }
    pub fn validate(&self) -> Result<(), InputError> {
        if self.schema_version != 1 {
            return Err(InputError::new("unsupported_schema"));
        }
        if let Some(sequence) = self.per_instance_sequence {
            crate::contracts::validate_safe_integer(sequence)?;
        }
        self.adapter_output.validate()?;
        // 接收单元统一使用本机时间，不允许通知和生命周期各自控制排序或TTL。
        if self
            .adapter_output
            .notification
            .as_ref()
            .is_some_and(|e| e.received_at != self.received_at)
            || self
                .adapter_output
                .lifecycle
                .as_ref()
                .is_some_and(|s| s.received_at != self.received_at)
        {
            return Err(InputError::new("context_mismatch"));
        }
        let mut metadata = self.clone();
        if let Some(event) = &mut metadata.adapter_output.notification {
            event.question = None;
            event.answer = None;
        }
        if serde_json::to_vec(&metadata)
            .map_err(|_| InputError::new("invalid_input"))?
            .len()
            > 8192
        {
            return Err(InputError::new("metadata_too_large"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
/// 后续generation/取消/聚合共用键；Unknown按接收单元隔离。
pub enum ActorKey {
    Main {
        instance_id: Uuid,
        session_id: String,
    },
    Identified {
        instance_id: Uuid,
        session_id: String,
        actor_id: String,
    },
    Unknown {
        ingress_id: Uuid,
    },
}
impl ActorKey {
    pub fn from_event(event: &NormalizedEventV1, ingress_id: Uuid) -> Self {
        match (event.actor_scope, &event.session_id, &event.actor_id) {
            (ActorScope::Main, Some(s), _) => Self::Main {
                instance_id: event.instance_id,
                session_id: s.clone(),
            },
            (ActorScope::Identified, Some(s), Some(a)) => Self::Identified {
                instance_id: event.instance_id,
                session_id: s.clone(),
                actor_id: a.clone(),
            },
            _ => Self::Unknown { ingress_id },
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum TurnKey {
    Upstream(String),
    Generation(
        #[ts(type = "number")]
        #[serde(with = "crate::contracts::safe_u64")]
        #[schemars(with = "u64", range(max = 9007199254740991_u64))]
        u64,
    ),
}
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, TS)]
/// 仅可靠actor及可证轮次/generation用于看门狗；调用前必须校验。
pub struct ActivityKey {
    pub actor: ActorKey,
    pub turn: TurnKey,
}
