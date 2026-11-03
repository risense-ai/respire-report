//! 旧 Windows 通知器仅做有界只读发现和非秘密偏好预演；正式入口切换需要可验证的旧发送器隔离。
use agentnotify_core::config::{ConfigV1, QuietBehavior, WatchdogMode, PresenceMode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::{Path, PathBuf}};

#[derive(Debug, Clone, Serialize)]
pub struct LegacyMailPreview {
    pub host: String,
    pub port: u16,
    pub security: String,
    pub sender: String,
    pub recipient: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LegacyReport {
    pub root: PathBuf,
    pub settings_hash: String,
    pub legacy_mode: String,
    pub session_filter_count: usize,
    pub idle_seconds: u32,
    pub mail: LegacyMailPreview,
    pub preferences: ConfigV1,
    pub warnings: Vec<&'static str>,
    pub secret_reauthorization_required: bool,
    pub legacy_queue: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationPlan {
    pub legacy: LegacyReport,
    pub target: ConfigV1,
    pub expected_target_revision: String,
    pub cutover: &'static str,
    pub assistance: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationReport {
    pub preferences_saved: bool,
    pub config_active: bool,
    pub activation_reason: String,
    pub cutover: &'static str,
    pub legacy_queue: &'static str,
    pub live_verified: bool,
}

pub struct MigrationActivation { pub active: bool, pub reason: String }

#[derive(Debug, Clone)]
pub struct MigrationError(pub &'static str);
pub type Result<T> = std::result::Result<T, MigrationError>;

#[derive(Deserialize)]
#[serde(default)]
struct LegacySettings {
    mode: String,
    idle_seconds: u32,
    sessions: Vec<String>,
    smtp_host: String,
    smtp_port: u16,
    smtp_security: String,
    sender: String,
    recipient: String,
    include_message: bool,
    retention_days: u32,
    stalled_seconds: u32,
    quiet_enabled: bool,
    quiet_start: String,
    quiet_end: String,
    include_question: bool,
}
impl Default for LegacySettings {
    fn default() -> Self {
        Self { mode:"auto".into(), idle_seconds:300, sessions:Vec::new(), smtp_host:String::new(), smtp_port:465,
            smtp_security:"ssl".into(), sender:String::new(), recipient:String::new(), include_message:true,
            retention_days:30, stalled_seconds:1800, quiet_enabled:true, quiet_start:"23:00".into(),
            quiet_end:"07:30".into(), include_question:true }
    }
}

fn safe_legacy_file(root:&Path)->Result<PathBuf> {
    if !root.is_absolute() || root.components().any(|c| matches!(c,std::path::Component::ParentDir)) {
        return Err(MigrationError("legacy_path_invalid"));
    }
    // 旧配置只读；拒绝符号链接或 junction，避免借迁移入口读取其他位置。
    for part in root.ancestors() {
        if fs::symlink_metadata(part).ok().is_some_and(|m| m.file_type().is_symlink()) {
            return Err(MigrationError("legacy_path_redirected"));
        }
    }
    let script=root.join("notifier.py");
    let script_meta=fs::symlink_metadata(script).map_err(|_|MigrationError("legacy_notifier_missing"))?;
    if !script_meta.is_file() || script_meta.file_type().is_symlink() {
        return Err(MigrationError("legacy_notifier_invalid"));
    }
    let path=root.join("settings.json");
    let meta=fs::symlink_metadata(&path).map_err(|_|MigrationError("legacy_settings_missing"))?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len()>65536 {
        return Err(MigrationError("legacy_settings_invalid"));
    }
    Ok(path)
}

/// 仅解析旧 settings.json 的已知非秘密字段；不打开旧数据库、授权码文件或信任记录。
pub fn inspect_legacy(root:&Path)->Result<LegacyReport> {
    let path=safe_legacy_file(root)?;
    let mut raw=Vec::new();
    fs::File::open(path).map_err(|_|MigrationError("legacy_settings_unavailable"))?
        .take(65537).read_to_end(&mut raw).map_err(|_|MigrationError("legacy_settings_unavailable"))?;
    if raw.len()>65536 { return Err(MigrationError("legacy_settings_invalid")); }
    let old:LegacySettings=serde_json::from_slice(&raw).map_err(|_|MigrationError("legacy_settings_invalid"))?;
    if !matches!(old.mode.as_str(),"off"|"always"|"auto") || !matches!(old.smtp_security.as_str(),"ssl"|"starttls")
        || old.smtp_port==0 || old.idle_seconds<1 || !(60..=86400).contains(&old.stalled_seconds)
        || old.retention_days==0 || old.quiet_start==old.quiet_end {
        return Err(MigrationError("legacy_settings_invalid"));
    }
    let mut preferences=ConfigV1::default();
    // 提问：旧 include_question 显式选择进入新 include_question；即使开启，也须先建立新内容密钥。
    preferences.include_question=old.include_question;
    // 回答：旧 include_message 是 Stop 最新内容，映射为新 include_answer，具体内容来源语义在预演中提示。
    preferences.include_answer=old.include_message;
    // 免打扰：旧本地时段映射 system 时区和 skip，不补发旧静默事件。
    preferences.quiet.enabled=old.quiet_enabled;
    preferences.quiet.start=old.quiet_start;
    preferences.quiet.end=old.quiet_end;
    preferences.quiet.timezone="system".into();
    preferences.quiet.behavior=QuietBehavior::Skip;
    // 元数据留存：旧 retention_days 仅映射新元数据留存，不复制旧历史。
    preferences.metadata_retention_days=old.retention_days;
    // 疑似无进展阈值：保留秒数供用户比较；新总耗时/进展语义不同，开关仍关闭。
    preferences.watchdog.threshold_seconds=old.stalled_seconds;
    preferences.watchdog.mode=WatchdogMode::ElapsedWithoutFinish;
    preferences.watchdog.enabled=false;
    // 通知开关：所有模式先保持关闭；旧 auto 保留 Windows 实时离席门禁；会话筛选仍需单独处理。
    preferences.enabled=false;
    preferences.presence.mode=if old.mode=="auto" { PresenceMode::AwayOnly } else { PresenceMode::Always };
    preferences.presence.idle_seconds=old.idle_seconds;
    preferences.validate().map_err(|_|MigrationError("legacy_settings_invalid"))?;
    let mut warnings=vec!["legacy_answer_semantics_differ", "watchdog_semantics_differ", "private_authorization_required", "legacy_pending_queue_isolated_in_place"];
    if old.mode=="auto" { warnings.push("windows_away_mode_preserved"); }
    if !old.sessions.is_empty() { warnings.push("session_filter_has_no_direct_mapping"); }
    let settings_hash=Sha256::digest(&raw).iter().map(|b|format!("{b:02x}")).collect();
    Ok(LegacyReport { root:root.to_owned(), settings_hash, legacy_mode:old.mode,
        session_filter_count:old.sessions.len(), idle_seconds:old.idle_seconds,
        mail:LegacyMailPreview { host:old.smtp_host, port:old.smtp_port, security:old.smtp_security,
            sender:old.sender, recipient:old.recipient }, preferences, warnings,
        secret_reauthorization_required:true, legacy_queue:"left_in_legacy_directory" })
}

/// 合并偏好预演，不改变当前安装、凭据、队列或客户端 Hook。
pub fn plan_migration(report:&LegacyReport,current:&ConfigV1,expected_target_revision:String)->Result<MigrationPlan> {
    let fresh=inspect_legacy(&report.root)?;
    if fresh.settings_hash!=report.settings_hash { return Err(MigrationError("legacy_settings_changed")); }
    let mut target=current.clone();
    target.enabled=false;
    target.include_question=fresh.preferences.include_question;
    target.include_answer=fresh.preferences.include_answer;
    target.quiet=fresh.preferences.quiet.clone();
    target.metadata_retention_days=fresh.preferences.metadata_retention_days;
    target.watchdog=fresh.preferences.watchdog.clone();
    target.presence=fresh.preferences.presence.clone();
    target.validate().map_err(|_|MigrationError("migration_config_invalid"))?;
    Ok(MigrationPlan { legacy:fresh, target, expected_target_revision,
        cutover:"needs_agent_verified_legacy_quiescence_and_queue_isolation",
        assistance:"migrate_python" })
}

/// 偏好写入由宿主现有 CAS 保存入口完成；本模块不提供可绕过双发门禁的 Hook apply。
pub fn apply_migration(plan:&MigrationPlan,save:impl FnOnce(ConfigV1,&str)->Result<MigrationActivation>)->Result<MigrationReport> {
    if inspect_legacy(&plan.legacy.root)?.settings_hash!=plan.legacy.settings_hash {
        return Err(MigrationError("legacy_settings_changed"));
    }
    if plan.target.enabled || plan.target.watchdog.enabled { return Err(MigrationError("unsafe_migration_plan")); }
    let activation=save(plan.target.clone(),&plan.expected_target_revision)?;
    Ok(MigrationReport { preferences_saved:true, config_active:activation.active, activation_reason:activation.reason,
        cutover:"needs_agent", legacy_queue:"left_in_legacy_directory", live_verified:false })
}
