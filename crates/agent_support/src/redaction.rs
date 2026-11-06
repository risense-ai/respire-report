//! 仅从强类型报告选择低敏字段；错误消息、原配置字节和日志完全不进入导出链。
use agentnotify_core::contracts::HealthReport;
use agentnotify_integration::{IntegrationPlan, inspect::InspectionReport};
use serde::Serialize;
use uuid::Uuid;

pub struct BundleContext<'a> {
    pub health: Option<&'a HealthReport>,
    pub inspection: Option<&'a InspectionReport>,
    pub plan: Option<&'a IntegrationPlan>,
}
impl BundleContext<'_> {
    pub fn empty() -> Self { Self { health: None, inspection: None, plan: None } }
}
#[derive(Serialize)]
pub struct DoctorExport {
    pub schema_version: u8,
    pub status: &'static str,
    pub daemon_status: &'static str,
    pub credential_status: &'static str,
    pub finding_codes: Vec<&'static str>,
    pub path_aliases: Vec<PathAlias>,
    pub redaction: &'static [&'static str],
}
#[derive(Serialize)]
pub struct PathAlias { pub alias: String, pub kind: &'static str }
#[derive(Serialize)]
pub struct CompatibilityExport { pub schema_version: u8, pub status: &'static str, pub clients: Vec<ClientExport> }
#[derive(Serialize)]
pub struct ClientExport { pub adapter_id: &'static str, pub support_status: &'static str, pub evidence: &'static str }
#[derive(Serialize)]
pub struct PlanExport { pub schema_version: u8, pub status: &'static str, pub reason: &'static str, pub local_operation_id: Option<Uuid>, pub steps: Vec<String> }

fn adapter(id: &str) -> &'static str { match id { "codex" => "codex", "claude_code" => "claude_code", "generic" => "generic", "workbuddy" => "workbuddy", _ => "unknown" } }
fn finding(code: &str) -> &'static str { match code {
    "HOOK_UNSUPPORTED" => "HOOK_UNSUPPORTED", "CONFIG_CONFLICT" => "CONFIG_CONFLICT",
    "CLIENT_TRUST_REQUIRED" => "CLIENT_TRUST_REQUIRED", "CREDENTIAL_REQUIRED" => "CREDENTIAL_REQUIRED",
    "SERVICE_UNAVAILABLE" | "daemon_unreachable" => "SERVICE_UNAVAILABLE",
    "BUNDLE_STALE" => "BUNDLE_STALE", "CAPABILITY_LIMITED" => "CAPABILITY_LIMITED",
    _ => "UNCLASSIFIED_ISSUE",
} }
/// 路径只导出顺序别名；真实路径仅返回给本机复制前预览。
pub fn doctor(context: &BundleContext<'_>) -> DoctorExport {
    let aliases = context.inspection.map(|report| report.clients.iter().filter(|c| c.config_path.is_some())
        .take(16).enumerate().map(|(i, _)| PathAlias { alias: format!("CONFIG_{:02}", i + 1), kind: "client_config" }).collect()).unwrap_or_default();
    DoctorExport { schema_version: 1, status: if context.health.is_some() { "observed" } else { "unavailable" },
        daemon_status: context.health.map(|h| match h.daemon { agentnotify_core::contracts::DaemonStatus::Running => "running", agentnotify_core::contracts::DaemonStatus::Stopped => "stopped", _ => "unknown" }).unwrap_or("unknown"),
        credential_status: context.health.map(|h| match h.credential_status {
            agentnotify_core::contracts::CredentialStatus::Ready => "ready",
            agentnotify_core::contracts::CredentialStatus::Missing => "missing",
            agentnotify_core::contracts::CredentialStatus::Locked => "locked",
            agentnotify_core::contracts::CredentialStatus::AccessDenied => "access_denied",
            agentnotify_core::contracts::CredentialStatus::Unavailable => "unavailable",
            agentnotify_core::contracts::CredentialStatus::Unknown => "unknown",
        }).unwrap_or("unknown"),
        finding_codes: context.health.map(|h| h.issues.iter().take(32).map(|i| finding(&i.code)).collect()).unwrap_or_default(),
        path_aliases: aliases,
        redaction: &["credential_values", "mail_addresses", "message_bodies", "raw_config", "environment", "logs", "raw_errors", "unrecognized_fields"], }
}
pub fn compatibility(context: &BundleContext<'_>) -> CompatibilityExport {
    let clients = context.inspection.map(|report| report.clients.iter().take(16).map(|c| {
        let id = adapter(&c.adapter_id);
        ClientExport { adapter_id: id, support_status: "not_verified", evidence: "not_run" }
    }).collect()).unwrap_or_default();
    CompatibilityExport { schema_version: 1, status: if context.inspection.is_some() { "limited_inspection" } else { "unavailable" }, clients }
}
/// IntegrationPlan含原始配置字节和命令，不能序列化；提供的计划也必须现场重查。
pub fn plan(context: &BundleContext<'_>) -> PlanExport {
    PlanExport { schema_version: 1, status: "unavailable", reason: if context.plan.is_some() { "source_plan_contains_private_config_replan_locally" } else { "no_safe_plan_provided" },
        local_operation_id: context.plan.map(|p| p.operation_id), steps: Vec::new() }
}
pub fn local_paths(context: &BundleContext<'_>) -> Vec<String> {
    context.inspection.map(|report| report.clients.iter().filter_map(|c| c.config_path.as_ref())
        .take(16).map(|p| p.display().to_string()).collect()).unwrap_or_default()
}
