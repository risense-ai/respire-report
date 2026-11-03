//! 即时白名单健康状态；凭据仅反映观察，不为status主动取密。
use agentnotify_core::contracts::*;
use agentnotify_platform::runtime::HostRuntime;
pub fn report(
    host: &HostRuntime,
    credential: CredentialStatus,
    issues: Vec<HealthIssue>,
) -> HealthReport {
    let mut issues = issues;
    let queue_counts = host.queue.counts().unwrap_or_else(|_| {
        issues.push(HealthIssue {
            code: "queue_health_unavailable".into(),
            message: "暂时无法读取队列状态。".into(),
        });
        QueueCounts::default()
    });
    if host.content_unavailable {
        issues.push(HealthIssue {
            code: "metadata_only".into(),
            message: "安全内容密钥不可用；仅保留元数据。".into(),
        });
    }
    HealthReport {
        schema_version: 1,
        daemon: DaemonStatus::Running,
        queue_counts,
        credential_status: credential,
        integrations: Vec::new(),
        issues,
    }
}
