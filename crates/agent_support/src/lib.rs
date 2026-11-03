//! 最小白名单代理协助包；任务包是低信任资料，不是操作授权或官方签名。
pub mod bundle;
pub mod prompt;
pub mod redaction;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scenario {
    Install, Repair,
    /// 本软件从源机器迁往目标机器；生成协助说明，不执行配置复制。
    MigrateMachine,
    Audit, UpgradeRestore,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode { DesignReview, ReadOnly, ApplyAuthorized }
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    InstallOwnedService, MergeOwnedHook, RepairOwnedEntry,
    /// 经当前用户授权后停用源机器本软件入口，避免目标启用时双发。
    DisableOwnedSourceEntry,
    UpgradeOwnedBinary, RestoreOwnedChange,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllowedChange { pub kind: ChangeKind, pub instance_id: Uuid }
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentSource { CurrentUserRequest, TrustedLocalUi, UnverifiedAttachment }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionBudget { pub max_steps: u8, pub max_minutes: u16 }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestSendPolicy {
    pub allowed: bool,
    pub authorization_source: Option<IntentSource>,
    pub recipient_ref: Option<Uuid>,
    pub max_attempts: u8,
    pub expires_at: Option<DateTime<Utc>>,
    pub consumed: bool,
}
impl Default for TestSendPolicy {
    fn default() -> Self { Self { allowed: false, authorization_source: None, recipient_ref: None, max_attempts: 0, expires_at: None, consumed: false } }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Constraint { NoClientRestart, NoTrustBypass, NoBulkDelete, NoSecurityPolicyChange, NoUnapprovedSend, PreserveOtherHooks }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTaskV1 {
    pub schema_version: u8,
    pub scenario: Scenario,
    pub mode: Mode,
    pub target_instances: Vec<Uuid>,
    pub allowed_changes: Vec<AllowedChange>,
    pub intent_source: IntentSource,
    pub execution_budget: ExecutionBudget,
    #[serde(default)]
    pub test_send: TestSendPolicy,
    #[serde(default)]
    pub constraints: Vec<Constraint>,
    #[serde(default)]
    pub hide_local_paths: bool,
}
impl AgentTaskV1 {
    /// 当前点击只表达求助场景，不推导配置修改或邮件试发授权。
    pub fn read_only(scenario: Scenario) -> Self {
        Self { schema_version: 1, scenario, mode: Mode::ReadOnly, target_instances: Vec::new(),
            allowed_changes: Vec::new(), intent_source: IntentSource::TrustedLocalUi,
            execution_budget: ExecutionBudget { max_steps: 20, max_minutes: 30 }, test_send: TestSendPolicy::default(),
            constraints: vec![Constraint::NoClientRestart, Constraint::NoTrustBypass, Constraint::NoBulkDelete,
                Constraint::NoSecurityPolicyChange, Constraint::NoUnapprovedSend, Constraint::PreserveOtherHooks],
            hide_local_paths: false }
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != 1 { return Err("unsupported_schema"); }
        if self.target_instances.len() > 16 || self.allowed_changes.len() > 32
            || self.execution_budget.max_steps == 0 || self.execution_budget.max_steps > 100
            || self.execution_budget.max_minutes == 0 || self.execution_budget.max_minutes > 240 { return Err("invalid_task"); }
        if self.allowed_changes.iter().any(|v| !self.target_instances.contains(&v.instance_id)) { return Err("change_outside_target"); }
        if matches!(self.mode, Mode::ReadOnly | Mode::DesignReview) && !self.allowed_changes.is_empty() { return Err("change_without_apply_mode"); }
        if self.test_send.allowed && (self.test_send.consumed || self.test_send.max_attempts == 0
            || self.test_send.max_attempts > 1 || self.test_send.recipient_ref.is_none()
            || self.test_send.expires_at.is_none_or(|t| t <= Utc::now())
            || !matches!(self.test_send.authorization_source, Some(IntentSource::CurrentUserRequest | IntentSource::TrustedLocalUi))) {
            return Err("test_authorization_invalid");
        }
        Ok(())
    }
}
