//! M0 experiments only; these probes are not product credential, service or mail implementations.
mod credential;
mod service;
mod smtp;

pub use credential::credential_roundtrip;
pub use service::service_independence;
pub use smtp::{smtp_stage_probe, smtp_tls_probe};

use serde::Serialize;

/// A non-secret observation. Native backends must never be substituted with mocks.
#[derive(Serialize)]
pub struct FeasibilityResult {
    /// 探针稳定名称：由实验入口给定，写入 JSON probe，非空且不含用户内容。
    pub probe: String,
    /// 系统构建：来自脚本探测的进程环境，缺失时用当前 OS 名，写入 JSON os_build。
    pub os_build: String,
    /// 实际后端：实验确认后的原生类型/协议说明，写入 JSON backend，不能把 mock 标为原生。
    pub backend: String,
    /// 依赖版本：实验锁定版说明，写入 JSON dependency_version，与 Cargo.lock 对照。
    pub dependency_version: String,
    /// 编译能力：实验启用的 features 列表，写入 JSON features；无能力用空数组。
    pub features: Vec<String>,
    /// 实际观察：固定无秘密描述，写入 JSON actual，不放凭据或邮件正文。
    pub actual: String,
    /// 证据文件：由稳定探针名生成的相对 JSON 文件名，写入 JSON evidence。
    pub evidence: String,
    /// 执行状态：snake_case 枚举写入 JSON status，未执行与阻塞不等于通过。
    pub status: ProbeStatus,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    Passed,
    Failed,
    Blocked,
    NotRun,
}

/// Persist only a bounded, non-secret result. Detailed live output remains ignored.
pub(crate) fn record(
    probe: &str,
    backend: &str,
    dependency: &str,
    features: &[&str],
    actual: &str,
) -> Result<(), String> {
    let result = FeasibilityResult {
        probe: probe.into(),
        os_build: std::env::var("AGENTNOTIFY_OS_BUILD")
            .unwrap_or_else(|_| std::env::consts::OS.into()),
        backend: backend.into(),
        dependency_version: dependency.into(),
        features: features.iter().map(|value| (*value).into()).collect(),
        actual: actual.into(),
        evidence: format!("{probe}.json"),
        status: ProbeStatus::Passed,
    };
    let json = serde_json::to_string_pretty(&result).map_err(|_| "serialize observation")?;
    println!("{json}");
    if let Some(directory) = std::env::var_os("AGENTNOTIFY_EVIDENCE_DIR") {
        std::fs::create_dir_all(&directory).map_err(|_| "create evidence directory")?;
        std::fs::write(
            std::path::Path::new(&directory).join(&result.evidence),
            json,
        )
        .map_err(|_| "persist observation")?;
    }
    Ok(())
}
