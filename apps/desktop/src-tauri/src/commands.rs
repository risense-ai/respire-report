//! 桌面白名单：所有路径由本机发现，WebView不能传入任意文件或进程命令。
use agentnotify_core::{
    config::ConfigV1,
    contracts::{CredentialRef, CredentialStatus, CredentialStore, DaemonStatus, DeliveryOutcome, HealthIssue, HealthReport, PreparedMail, QueueCounts, SecretInput, SendStage, Transport},
    delivery::{mail::valid_address, smtp::{public_tls, SendJournal, JournalError, SmtpConfig, SmtpTransport}},
    queue::DirectorySecurity,
};
use agentnotify_integration::{IntegrationManager, IntegrationRequest, inspect::InspectionReport};
use agentnotify_integration::migration::{self, LegacyReport, MigrationPlan, MigrationReport};
use agentnotify_agent_support::{AgentTaskV1, Scenario, bundle::create_bundle_with_context, prompt::render_prompt, redaction::BundleContext};
use agentnotify_platform::{
    credentials::NativeCredentialStore,
    ipc::{self, Request, Response},
    paths::PlatformPaths,
    runtime::{DeliverySettings, HostSettings},
    service::{self, ServiceManager, ServiceReport},
};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, fs::{self, File, OpenOptions}, io::Write, path::{Path, PathBuf}, process::{Command, Stdio}, sync::{Arc, Mutex}, time::{Duration, Instant}};
use tauri::State;
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Default)]
pub struct AppState { previews: Mutex<HashMap<Uuid, PreviewRecord>> }
struct PreviewRecord { issued: Instant, host_revision: String, sender: String, recipient: String }

#[derive(Serialize)]
pub struct ConfigSnapshot { pub(crate) config: ConfigV1, pub(crate) revision: String, watchdog_ready: bool, inactivity_ready: bool }
#[derive(Serialize)]
pub struct MailSnapshot { mail: MailView, revision: String }
#[derive(Serialize)]
pub struct MailView {
    sender: String, recipient: String, provider: String, host: String, port: u16,
    tls: String, username: String, has_secret: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MailInput {
    sender: String, recipient: String, provider: String, host: String, port: u16,
    tls: String, username: String, secret: Option<String>,
}
#[derive(Serialize)]
pub struct SaveReport { pub(crate) saved: bool, pub(crate) active: bool, pub(crate) reason: String, pub(crate) revision: String }
#[derive(Serialize)]
pub struct SafePlanSummary { operation_id: Uuid, original_hash: Option<String>, planned_hash: String, events: Vec<String>, status: String }
#[derive(Deserialize)]
struct CliPlanSummary { operation_id: Uuid, original_hash: Option<String>, planned_hash: String, events: Vec<String>, status: String }
#[derive(Deserialize)]
struct CliVerification { operation_id: Uuid, config_matches_plan: bool, binding_matches: bool, status: String, live_verified: bool }
#[derive(Serialize)]
pub struct SafeVerification { operation_id: Uuid, config_matches_plan: bool, binding_matches: bool, status: String, live_verified: bool, completion_channel: String, reason: String }
#[derive(Serialize)]
pub struct HistoryRow {
    id: String, status: String, received_at: i64, kind: String,
    project: String, session: String, source: String, occurred_at: Option<String>,
    attempts: u32, send_stage: String, policy_reason: Option<String>, diagnostics: Vec<String>,
}
#[derive(Serialize)]
pub struct DiagnosticRow { code: String, count: u32 }
#[derive(Serialize)]
pub struct DiagnosticsReport { issues: Vec<HealthIssue>, recent: Vec<DiagnosticRow>, agent_bundle_available: bool }
#[derive(Serialize)]
pub struct AgentBundleResult { bundle_id: Uuid, bundle_path: PathBuf, path_preview: Vec<String>, prompt: String, expires_at: String, plan_status: &'static str }
#[derive(Serialize)]
pub struct TestPreview { confirmation: Uuid, sender: String, recipient: String, subject: String, note: String }
#[derive(Serialize)]
pub struct TestReport { outcome: DeliveryOutcome, smtp_accepted: bool, recipient_confirmed: bool }

/// 根目录仅来自启动进程参数，不接受WebView传入；与CLI使用相同绝对路径/安全目录检查。
pub(crate) fn paths() -> Result<PlatformPaths, String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let paths = match args.as_slice() {
        [] => PlatformPaths::discover(),
        [flag, root] if flag == "--data-dir" => PlatformPaths::override_data(Path::new(root)),
        _ => return Err("invalid_desktop_arguments".into()),
    };
    paths.map_err(|e| e.0.into())
}
fn secure(paths: &PlatformPaths, create: bool) -> Result<Arc<dyn DirectorySecurity>, String> { paths.secure(create).map_err(|e| e.0.to_owned()) }
fn file_revision(path: &Path, max: u64) -> Result<String, String> {
    if !path.exists() { return Ok("missing".into()); }
    let meta = fs::metadata(path).map_err(|_| "settings_unavailable")?;
    if !meta.is_file() || meta.len() > max { return Err("settings_invalid".into()); }
    let raw = fs::read(path).map_err(|_| "settings_unavailable")?;
    Ok(format!("{:x}", Sha256::digest(raw)))
}
pub(crate) fn settings_lock(root: &Path) -> Result<File, String> {
    let path = root.join("desktop-settings.lock");
    let file = OpenOptions::new().read(true).write(true).create(true).open(path).map_err(|_| "settings_lock_unavailable")?;
    file.try_lock().map_err(|_| "settings_busy")?;
    Ok(file)
}
fn save_host(root: &Path, host: &HostSettings) -> Result<(), String> {
    let raw = serde_json::to_vec_pretty(host).map_err(|_| "host_config_invalid")?;
    if raw.len() > 65536 { return Err("host_config_too_large".into()); }
    let mut temp = tempfile::NamedTempFile::new_in(root).map_err(|_| "host_write_failed")?;
    temp.write_all(&raw).and_then(|_| temp.as_file().sync_all()).map_err(|_| "host_write_failed")?;
    temp.persist(root.join("host.json")).map_err(|_| "host_write_failed")?;
    Ok(())
}
/// 文件已保存与后台实际采用是两件事；仅重启本工具后台，绝不触碰 AI 客户端。
fn activate(paths: &PlatformPaths, revision: String) -> SaveReport {
    if !paths.data.join("installation.json").exists() {
        return SaveReport { saved: true, active: false, reason: "background_not_installed".into(), revision };
    }
    let manager = ServiceManager::new(paths.clone());
    if service::daemon_running(paths) {
        match manager.stop() {
            Ok(r) if !r.failed => {},
            _ => return SaveReport { saved: true, active: false, reason: "background_restart_failed".into(), revision },
        }
    }
    match manager.start() {
        Ok(r) if !r.failed && r.running => SaveReport { saved: true, active: true, reason: "active".into(), revision },
        _ => SaveReport { saved: true, active: false, reason: "background_start_failed".into(), revision },
    }
}

#[tauri::command]
pub fn get_health() -> Result<HealthReport, String> {
    let paths = paths()?;
    let mut daemon = DaemonStatus::Stopped;
    if paths.data.exists() {
        let _security = secure(&paths, false)?;
        if let Ok(Response::Health { report }) = ipc::call(&paths, &Request::Health) { return Ok(report); }
        daemon = DaemonStatus::Unknown;
    }
    Ok(HealthReport { schema_version: 1, daemon, queue_counts: QueueCounts::default(), credential_status: CredentialStatus::Unknown, integrations: Vec::new(), issues: vec![HealthIssue { code: "daemon_unreachable".into(), message: "后台状态不可确认；队列数量尚未读取。".into() }] })
}

#[tauri::command]
pub fn load_config() -> Result<ConfigSnapshot, String> {
    let paths = paths()?;
    if !paths.data.exists() { return Ok(ConfigSnapshot { config: ConfigV1::default(), revision: "missing".into(), watchdog_ready: false, inactivity_ready: false }); }
    let _security = secure(&paths, false)?;
    let path = paths.data.join("config.json");
    let revision = file_revision(&path, 1024 * 1024)?;
    let config = if revision == "missing" { ConfigV1::default() } else { ConfigV1::load(&path).map_err(|e| e.code.to_owned())? };
    let host = HostSettings::load(&paths.data).map_err(|e| e.0.to_owned())?;
    let watchdog_ready = !host.instances.is_empty() && host.instances.iter().all(|i| i.capabilities.reliable_started && i.capabilities.reliable_actor && i.capabilities.reliable_turn_or_generation);
    let inactivity_ready = watchdog_ready && host.instances.iter().all(|i| i.capabilities.reliable_progress);
    Ok(ConfigSnapshot { config, revision, watchdog_ready, inactivity_ready })
}

#[tauri::command]
pub fn save_config(config: ConfigV1, expected_revision: String) -> Result<SaveReport, String> {
    save_config_inner(config, expected_revision, true)
}
/// 托盘退出只保存关闭状态，不能为了退出启动后台；复用同一CAS与安全锁。
pub(crate) fn save_config_inner(config: ConfigV1, expected_revision: String, activate_after: bool) -> Result<SaveReport, String> {
    // 退出专用分支只能保存关闭状态，不能被复用为跳过正文密钥校验的启用路径。
    if !activate_after && config.enabled { return Err("invalid_config".into()); }
    // 1. 验证非秘密契约并在当前用户安全目录内做跨窗口 CAS。
    config.validate().map_err(|e| e.code.to_owned())?;
    if config.queue_ttl_hours > 24 { return Err("invalid_config".into()); }
    let paths = paths()?;
    let _security = secure(&paths, true)?;
    let _admission = agentnotify_platform::update::configuration_guard(&paths, _security.clone()).map_err(|e| e.0.to_owned())?;
    let _lock = settings_lock(&paths.data)?;
    let registered = HostSettings::load(&paths.data).map_err(|e| e.0.to_owned())?;
    if config.watchdog.enabled && (registered.instances.is_empty() || registered.instances.iter().any(|i| config.watchdog.validate_capabilities(&i.capabilities).is_err())) {
        return Err("watchdog_capability_missing".into());
    }
    let path = paths.data.join("config.json");
    if file_revision(&path, 1024 * 1024)? != expected_revision { return Err("config_conflict".into()); }
    // 2. 正文选择仅在系统安全存储可建立内容密钥时发布，永不退回明文。
    if activate_after && (config.include_question || config.include_answer) {
        let mut host = HostSettings::load(&paths.data).map_err(|e| e.0.to_owned())?;
        let store = NativeCredentialStore::default();
        if let Some(reference) = &host.content_key {
            store.load_content_key(reference).map_err(|_| "content_key_unavailable")?;
        } else {
            let (reference, _) = store.create_content_key().map_err(|_| "content_key_unavailable")?;
            host.content_key = Some(reference.clone());
            if let Err(e) = save_host(&paths.data, &host) {
                let _ = store.delete(&reference);
                return Err(e);
            }
        }
    }
    config.save_atomic(&path).map_err(|e| e.code.to_owned())?;
    let revision = file_revision(&path, 1024 * 1024)?;
    drop(_lock);
    drop(_admission);
    if activate_after { Ok(activate(&paths, revision)) }
    else { Ok(SaveReport { saved: true, active: false, reason: "saved_disabled".into(), revision }) }
}

#[tauri::command]
pub fn load_mail() -> Result<MailSnapshot, String> {
    let paths = paths()?;
    if !paths.data.exists() { return Ok(MailSnapshot { mail: empty_mail(), revision: "missing".into() }); }
    let _security = secure(&paths, false)?;
    let revision = file_revision(&paths.data.join("host.json"), 65536)?;
    let host = HostSettings::load(&paths.data).map_err(|e| e.0.to_owned())?;
    let mail = match host.delivery {
        Some(d) => MailView { sender: d.from, recipient: d.recipients.first().cloned().unwrap_or_default(), provider: provider_for(&d.host, d.port, &d.tls).into(), host: d.host, port: d.port, tls: d.tls, username: d.username, has_secret: true },
        None => empty_mail(),
    };
    Ok(MailSnapshot { mail, revision })
}
fn empty_mail() -> MailView { MailView { sender: String::new(), recipient: String::new(), provider: "qq".into(), host: "smtp.qq.com".into(), port: 465, tls: "tls".into(), username: String::new(), has_secret: false } }
fn provider_for(host: &str, port: u16, tls: &str) -> &'static str { match (host, port, tls) { ("smtp.qq.com", 465, "tls") => "qq", ("smtp.163.com", 465, "tls") => "163", ("smtp.gmail.com", 465, "tls") => "gmail", _ => "custom" } }
fn smtp_for(input: &MailInput) -> Result<(String, u16, String), String> {
    match input.provider.as_str() {
        "qq" => Ok(("smtp.qq.com".into(), 465, "tls".into())),
        "163" => Ok(("smtp.163.com".into(), 465, "tls".into())),
        "gmail" => Ok(("smtp.gmail.com".into(), 465, "tls".into())),
        "custom" => Ok((input.host.clone(), input.port, input.tls.clone())),
        "outlook" | "microsoft365" => Err("oauth_publisher_registration_missing".into()),
        _ => Err("provider_unsupported".into()),
    }
}

#[tauri::command]
pub fn save_mail(input: MailInput, expected_revision: String) -> Result<SaveReport, String> {
    // 1. 三字段主流程：地址与授权码；企业自定义 SMTP 才读取高级参数。
    if !valid_address(&input.sender) || !valid_address(&input.recipient) { return Err("invalid_mail_address".into()); }
    let (host_name, port, tls) = smtp_for(&input)?;
    let username = if input.username.is_empty() { input.sender.clone() } else { input.username.clone() };
    SmtpConfig::new(&host_name, port, &username, &tls).map_err(|_| "invalid_smtp_config")?;
    let secret = Zeroizing::new(input.secret.unwrap_or_default());
    if secret.len() > 4096 || secret.bytes().any(|b| b == 0 || b == b'\n' || b == b'\r') { return Err("invalid_mail_secret".into()); }
    let paths = paths()?;
    let _security = secure(&paths, true)?;
    let _admission = agentnotify_platform::update::configuration_guard(&paths, _security.clone()).map_err(|e| e.0.to_owned())?;
    let _lock = settings_lock(&paths.data)?;
    let path = paths.data.join("host.json");
    if file_revision(&path, 65536)? != expected_revision { return Err("config_conflict".into()); }
    let mut settings = HostSettings::load(&paths.data).map_err(|e| e.0.to_owned())?;
    // 2. 新秘密只进当前用户 Credential Manager；失败时删除未发布的新引用。
    let old = settings.delivery.as_ref().map(|d| d.credential.clone());
    let reference = if secret.is_empty() { old.clone().ok_or("secret_required")? } else {
        let reference = CredentialRef { id: Uuid::new_v4() };
        NativeCredentialStore::default().put(&reference, SecretInput::new(secret.as_bytes().to_vec())).map_err(|_| "credential_write_failed")?;
        reference
    };
    settings.delivery = Some(DeliverySettings { host: host_name, port, username, tls, oauth: false, from: input.sender, recipients: vec![input.recipient], credential: reference.clone() });
    if let Err(e) = save_host(&paths.data, &settings) {
        if old.as_ref() != Some(&reference) { let _ = NativeCredentialStore::default().delete(&reference); }
        return Err(e);
    }
    let revision = file_revision(&path, 65536)?;
    drop(_lock);
    drop(_admission);
    let result = activate(&paths, revision);
    if old.as_ref() != Some(&reference) && (result.active || !service::daemon_running(&paths)) {
        if let Some(old) = old { let _ = NativeCredentialStore::default().delete(&old); }
    }
    Ok(result)
}

#[tauri::command]
pub fn inspect_integrations(overrides: Option<Vec<agentnotify_integration::inspect::DiscoveryOverride>>) -> Result<InspectionReport, String> {
    let paths = paths()?;
    let security = if paths.data.exists() { Some(secure(&paths, false)?) } else { None };
    let overrides=overrides.unwrap_or_default();
    if overrides.len()>8 || overrides.iter().any(|o| !matches!(o.adapter_id.as_str(),"codex"|"claude_code") || !o.config_dir.is_absolute() || o.config_dir.components().any(|c|matches!(c,std::path::Component::ParentDir))) {return Err("invalid_discovery_path".into());}
    let mut report=agentnotify_integration::inspect::inspect_with(&IntegrationManager::new(paths.data.clone()),&overrides);
    crate::onboarding::enrich(&paths.data,&mut report,security.as_deref());Ok(report)
}

#[tauri::command]
pub fn inspect_legacy_preferences(root: PathBuf) -> Result<LegacyReport, String> {
    migration::inspect_legacy(&root).map_err(|e|e.0.into())
}

#[tauri::command]
pub fn plan_legacy_preferences(root: PathBuf) -> Result<MigrationPlan, String> {
    let report=migration::inspect_legacy(&root).map_err(|e|e.0.to_owned())?;
    let current=load_config()?;
    migration::plan_migration(&report,&current.config,current.revision).map_err(|e|e.0.into())
}

#[tauri::command]
pub fn apply_legacy_preferences(root: PathBuf, settings_hash: String, expected_revision: String) -> Result<MigrationReport, String> {
    let report=migration::inspect_legacy(&root).map_err(|e|e.0.to_owned())?;
    if report.settings_hash!=settings_hash { return Err("legacy_settings_changed".into()); }
    let current=load_config()?;
    if current.revision!=expected_revision { return Err("config_conflict".into()); }
    let plan=migration::plan_migration(&report,&current.config,expected_revision).map_err(|e|e.0.to_owned())?;
    migration::apply_migration(&plan,|config,revision| {
        let saved=save_config(config,revision.to_owned()).map_err(|_|migration::MigrationError("preferences_save_failed"))?;
        if !saved.saved { return Err(migration::MigrationError("preferences_save_failed")); }
        Ok(migration::MigrationActivation { active:saved.active, reason:saved.reason })
    }).map_err(|e|e.0.into())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanInput {
    adapter_id: String, product_version: String, config_path: PathBuf,
    client_executable: PathBuf, instance_id: Option<Uuid>,
}
fn cli_path() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|_| "desktop_executable_unavailable".into())
}
fn run_cli<T: DeserializeOwned>(args: &[&str]) -> Result<T, String> {
    // 文件输出避免后代继承管道导致 EOF 永不到达；超时不是取消，明确要求重新核对状态。
    let output=tempfile::tempfile().map_err(|_|"cli_launch_failed")?;
    let mut command=Command::new(cli_path()?);
    command.args(args).stdin(Stdio::null()).stderr(Stdio::null()).stdout(output.try_clone().map_err(|_|"cli_launch_failed")?);
    #[cfg(windows)] { use std::os::windows::process::CommandExt; command.creation_flags(0x08000000); }
    let mut child=command.spawn().map_err(|_|"cli_launch_failed")?;
    let deadline=std::time::Instant::now()+Duration::from_secs(75);
    let status=loop {
        match child.try_wait().map_err(|_|"cli_status_unknown")? {
            Some(status)=>break status,
            None if std::time::Instant::now()<deadline=>std::thread::sleep(Duration::from_millis(50)),
            None=>return Err("cli_timeout_outcome_unknown".into()),
        }
    };
    use std::io::{Read,Seek,SeekFrom};
    let mut output=output;output.seek(SeekFrom::Start(0)).map_err(|_|"cli_report_invalid")?;
    let mut bytes=Vec::new();output.take(131073).read_to_end(&mut bytes).map_err(|_|"cli_report_invalid")?;
    if bytes.len()>131072{return Err("cli_report_too_large".into());}
    let report: agentnotify_core::contracts::CommandReport<T> = serde_json::from_slice(&bytes).map_err(|_| "cli_report_invalid")?;
    if !status.success() || !report.ok { return Err(report.error.map(|e| e.code).unwrap_or("cli_operation_failed".into())); }
    report.data.ok_or("cli_report_empty".into())
}
pub fn plan_integration_inner(input: PlanInput) -> Result<SafePlanSummary, String> {
    // 1. 仅允许已登记客户端类型；CLI自行验证版本与文件，UI无法传入代理执行路径。
    if !matches!(input.adapter_id.as_str(), "codex" | "claude_code") { return Err("unsupported_adapter".into()); }
    let paths = paths()?;
    let _security = secure(&paths, true)?;
    let cli = cli_path()?;
    let existing=IntegrationManager::new(paths.data.clone()).bindings().map_err(|_|"binding_state_unavailable")?;
    let instance_id=input.instance_id.or_else(||existing.iter().find(|b|b.adapter_id==input.adapter_id && b.config_path.canonicalize().ok().zip(input.config_path.canonicalize().ok()).is_some_and(|(a,b)|a==b)).map(|b|b.instance_id)).unwrap_or_else(Uuid::new_v4);
    let request = IntegrationRequest {
        schema_version: 1,
        adapter_id: input.adapter_id.clone(),
        instance_id,
        product_version: input.product_version,
        codex_home: if input.adapter_id == "codex" { input.config_path.parent().map(Path::to_path_buf) } else { None },
        config_path: input.config_path,
        client_executable: input.client_executable,
        agentnotify_executable: cli,
    };
    // 2. 请求只含版本与路径；原客户端字节留在集成管理器内，返回只保留安全摘要。
    let mut file = tempfile::NamedTempFile::new_in(&paths.data).map_err(|_| "request_unavailable")?;
    serde_json::to_writer(&mut file, &request).map_err(|_| "request_invalid")?;
    file.flush().map_err(|_| "request_unavailable")?;
    let root = paths.data.to_str().ok_or("path_encoding_unsupported")?;
    let request_path = file.path().to_str().ok_or("path_encoding_unsupported")?;
    let report: CliPlanSummary = run_cli(&["--data-dir", root, "integration", "plan", "--request", request_path])?;
    Ok(SafePlanSummary { operation_id: report.operation_id, original_hash: report.original_hash, planned_hash: report.planned_hash, events: report.events, status: report.status.into() })
}

pub fn apply_integration_inner(operation_id: Uuid) -> Result<SafeVerification, String> {
    let paths = paths()?;
    let _security = secure(&paths, false)?;
    let plan = IntegrationManager::new(paths.data.clone()).operation_path(operation_id);
    let root = paths.data.to_str().ok_or("path_encoding_unsupported")?;
    let path = plan.to_str().ok_or("path_encoding_unsupported")?;
    let report: CliVerification = run_cli(&["--data-dir", root, "integration", "apply", "--plan", path])?;
    let bytes=fs::read(&plan).map_err(|_|"operation_unavailable")?;
    let stored:agentnotify_integration::IntegrationPlan=serde_json::from_slice(&bytes).map_err(|_|"operation_invalid")?;
    let mut channel="hook".to_owned();let mut reason=report.status.clone();
    if stored.request.adapter_id=="codex" && report.binding_matches {
        match crate::onboarding::observer_enabled(&paths.data,stored.request.instance_id) {
            Ok(true)=>{channel="local_observer".into();reason="observer_already_enabled".into();},
            Ok(false) if stored.request.product_version=="0.166.0-alpha.5.1"=>{
                let id=stored.request.instance_id.to_string();
                match run_cli::<serde_json::Value>(&["--data-dir",root,"observer","enable","codex","--instance-id",&id]) {
                    Ok(_)=>{channel="local_observer".into();reason="observer_enabled_event_pending".into();},Err(code)=>reason=code,
                }
            },
            Ok(false)=>{},Err(code)=>reason=code,
        }
    }
    Ok(SafeVerification { operation_id: report.operation_id, config_matches_plan: report.config_matches_plan, binding_matches: report.binding_matches, status: report.status, live_verified: report.live_verified, completion_channel:channel,reason })
}

#[tauri::command]
pub fn get_history() -> Result<Vec<HistoryRow>, String> {
    let paths = paths()?;
    if !paths.data.exists() { return Ok(Vec::new()); }
    let _security = secure(&paths, false)?;
    let db_path = paths.data.join("queue.sqlite");
    if !db_path.is_file() { return Ok(Vec::new()); }
    let db = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|_| "history_unavailable")?;
    db.busy_timeout(Duration::from_secs(1)).map_err(|_| "history_unavailable")?;
    // 只投影已有元数据和状态；不查询正文密文，也不回查客户端记录补齐历史。
    let mut stmt = db.prepare("SELECT n.id,n.status,i.received_at,i.metadata,n.attempts,n.send_stage,p.reason,
        (SELECT group_concat(code,char(31)) FROM (SELECT code FROM diagnostics WHERE ingress_id=n.id ORDER BY id DESC LIMIT 12))
        FROM notifications n JOIN ingress i ON i.id=n.id LEFT JOIN policy_notifications p ON p.id=n.id
        ORDER BY i.received_at DESC LIMIT 40")
        .map_err(|_| "history_unavailable")?;
    let rows = stmt.query_map([], |r| {
        let metadata: Vec<u8> = r.get(3)?;
        let event: serde_json::Value = serde_json::from_slice(&metadata).unwrap_or_default();
        let notice = &event["adapter_output"]["notification"];
        let label = |key: &str| notice[key].as_str().unwrap_or("").chars().take(512).collect::<String>();
        let codes: Option<String> = r.get(7)?;
        Ok(HistoryRow {
            // 入站ID：通知主键，仅供列表定位，不作为用户会话标题。
            id: r.get(0)?,
            // 当前状态：数据库已有状态，前端中文展示，不推断实际收件。
            status: r.get(1)?,
            // 接收时间：UTC毫秒，由前端按本地时间显示。
            received_at: r.get(2)?,
            // 事件类型：中立元数据kind，缺失空串交前端提示未知。
            kind: label("kind"),
            // 项目：保存时project_label，缺失不回查或编造，最多512字符。
            project: label("project_label"),
            // 会话：保存时session_label，不拿项目或代理昵称代替。
            session: label("session_label"),
            // 来源工具：adapter_id只表示接入工具，不升级为来源可信证明。
            source: label("adapter_id"),
            // 发生时间：可空RFC3339元数据，缺失不以接收时间冒充。
            occurred_at: notice["source_time"].as_str().map(|v| v.chars().take(80).collect()),
            // 尝试次数：已持久化租约尝试数，不代表成功发送次数。
            attempts: r.get(4)?,
            // 投递阶段：最近持久阶段，不是完整时序日志。
            send_stage: r.get(5)?,
            // 接收规则原因：可空policy记录，不冒充SMTP失败详情。
            policy_reason: r.get(6)?,
            // 诊断：当前通知最多12个已有代码，每项最多120字符，缺失为空集合。
            diagnostics: codes.unwrap_or_default().split(char::from(31)).filter(|v| !v.is_empty())
                .map(|v| v.chars().take(120).collect()).collect(),
        })
    }).map_err(|_| "history_unavailable")?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|_| "history_unavailable".into())
}

#[tauri::command]
pub fn get_diagnostics() -> Result<DiagnosticsReport, String> {
    let health = get_health()?;
    let paths = paths()?;
    if !paths.data.exists() { return Ok(DiagnosticsReport { issues: health.issues, recent: Vec::new(), agent_bundle_available: true }); }
    let _security = secure(&paths, false)?;
    let db_path = paths.data.join("queue.sqlite");
    let mut recent = Vec::new();
    if db_path.is_file() {
        let db = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|_| "diagnostics_unavailable")?;
        let mut stmt = db.prepare("SELECT code,count(*) FROM diagnostics GROUP BY code ORDER BY count(*) DESC LIMIT 20").map_err(|_| "diagnostics_unavailable")?;
        let rows = stmt.query_map([], |r| Ok(DiagnosticRow { code: r.get(0)?, count: r.get(1)? })).map_err(|_| "diagnostics_unavailable")?;
        recent = rows.collect::<Result<Vec<_>, _>>().map_err(|_| "diagnostics_unavailable")?;
    }
    Ok(DiagnosticsReport { issues: health.issues, recent, agent_bundle_available: true })
}

/// 仅从本机状态生成只读求助材料；UI场景选择不能变成安装或试发授权。
#[tauri::command]
pub fn create_agent_bundle(scenario: String, hide_local_paths: bool, output_directory: Option<PathBuf>) -> Result<AgentBundleResult, String> {
    let scenario = match scenario.as_str() {
        "install" => Scenario::Install, "repair" => Scenario::Repair,
        "migrate_machine" => Scenario::MigrateMachine, "audit" => Scenario::Audit,
        "upgrade_restore" => Scenario::UpgradeRestore, _ => return Err("unsupported_scenario".into()),
    };
    let paths = paths()?;
    let _security = secure(&paths, true)?;
    let bundles = crate::onboarding::bundle_parent(output_directory)?;
    fs::create_dir_all(&bundles).map_err(|_| "bundle_create_failed")?;
    let destination = bundles.join(Uuid::new_v4().to_string());
    let health = get_health()?;
    let inspection = IntegrationManager::new(paths.data).inspect();
    let context = BundleContext { health: Some(&health), inspection: Some(&inspection), plan: None };
    let mut task = AgentTaskV1::read_only(scenario);
    task.hide_local_paths = hide_local_paths;
    let manifest = create_bundle_with_context(&task, &destination, &context).map_err(|e| e.code.to_owned())?;
    let prompt = render_prompt(&task, &manifest).map_err(|e| e.code.to_owned())?;
    let mut path_preview = if hide_local_paths { Vec::new() } else { manifest.path_preview };
    if !hide_local_paths { path_preview.insert(0, destination.display().to_string()); }
    Ok(AgentBundleResult { bundle_id: manifest.bundle_id, bundle_path: destination,
        path_preview, prompt, expires_at: manifest.expires_at.to_rfc3339(), plan_status: "unavailable" })
}

#[tauri::command]
pub fn preview_test(state: State<'_, AppState>) -> Result<TestPreview, String> {
    let paths = paths()?;
    let _security = secure(&paths, false)?;
    let host = HostSettings::load(&paths.data).map_err(|e| e.0.to_owned())?;
    let delivery = host.delivery.ok_or("mail_not_configured")?;
    if delivery.oauth { return Err("oauth_publisher_registration_missing".into()); }
    let confirmation = Uuid::new_v4();
    let recipient = delivery.recipients.first().cloned().ok_or("mail_not_configured")?;
    let record = PreviewRecord { issued: Instant::now(), host_revision: file_revision(&paths.data.join("host.json"), 65536)?, sender: delivery.from.clone(), recipient: recipient.clone() };
    let mut previews = state.previews.lock().map_err(|_| "preview_unavailable")?;
    previews.retain(|_, value| value.issued.elapsed() <= Duration::from_secs(120));
    if previews.len() >= 16 { return Err("too_many_previews".into()); }
    previews.insert(confirmation, record);
    Ok(TestPreview { confirmation, sender: delivery.from, recipient, subject: "AgentNotify 测试邮件".into(), note: "点击发送后只发送一次。SMTP 接受不代表已到达收件箱。".into() })
}

struct TestJournal { root: PathBuf }
impl TestJournal {
    fn persist(&self, id: &str, stage: &str, reply: Option<u16>) -> Result<(), JournalError> {
        let mut temp = tempfile::NamedTempFile::new_in(&self.root).map_err(|_| JournalError)?;
        let raw = serde_json::json!({ "test_id": id, "stage": stage, "reply": reply, "at": chrono::Utc::now().to_rfc3339() });
        serde_json::to_writer(&mut temp, &raw).map_err(|_| JournalError)?;
        temp.as_file().sync_all().map_err(|_| JournalError)?;
        temp.persist(self.root.join("test-send-state.json")).map_err(|_| JournalError)?;
        Ok(())
    }
}
impl SendJournal for TestJournal {
    fn record(&self, id: &str, stage: SendStage) -> Result<(), JournalError> {
        let label = match stage { SendStage::Prepared => "prepared", SendStage::Connecting => "connecting", SendStage::Envelope => "envelope", SendStage::DataStarted => "data_started", SendStage::Accepted => "accepted" };
        self.persist(id, label, None)
    }
    fn rejected(&self, id: &str, code: u16) -> Result<(), JournalError> { self.persist(id, "rejected", Some(code)) }
}

#[tauri::command]
pub fn send_test(state: State<'_, AppState>, confirmation: Uuid, explicit: bool) -> Result<TestReport, String> {
    // 1. 单次、短时确认只能由预演获取；普通保存与页面加载不会调用本命令。
    if !explicit { return Err("explicit_confirmation_required".into()); }
    let preview = state.previews.lock().map_err(|_| "preview_unavailable")?.remove(&confirmation).ok_or("preview_expired")?;
    if preview.issued.elapsed() > Duration::from_secs(120) { return Err("preview_expired".into()); }
    let paths = paths()?;
    let _security = secure(&paths, false)?;
    let host = HostSettings::load(&paths.data).map_err(|e| e.0.to_owned())?;
    let d = host.delivery.ok_or("mail_not_configured")?;
    if file_revision(&paths.data.join("host.json"), 65536)? != preview.host_revision || d.from != preview.sender || d.recipients.first() != Some(&preview.recipient) { return Err("preview_changed".into()); }
    if d.oauth { return Err("oauth_publisher_registration_missing".into()); }
    let smtp = SmtpConfig::new(&d.host, d.port, &d.username, &d.tls).map_err(|_| "invalid_smtp_config")?;
    if !valid_address(&d.from) || d.recipients.len() != 1 || !valid_address(&d.recipients[0]) { return Err("invalid_mail_address".into()); }
    let credential = NativeCredentialStore::default().get(&d.credential).map_err(|_| "credential_unavailable")?;
    // 2. DATA 前持久化阶段；不确定结果只呈现，不自动补发。
    let journal = TestJournal { root: paths.data };
    let mail = PreparedMail { message_id: format!("<{confirmation}@agentnotify.local>"), from: d.from, recipients: d.recipients, subject: "AgentNotify 测试邮件".into(), body: SecretInput::new(b"This is an explicit AgentNotify test message. SMTP acceptance does not confirm inbox delivery.".to_vec()), html_body: None };
    journal.record(&confirmation.to_string(), SendStage::Prepared).map_err(|_| "journal_unavailable")?;
    let outcome = SmtpTransport::new(smtp, public_tls(), &journal, confirmation).send(&mail, &credential);
    Ok(TestReport { smtp_accepted: matches!(outcome, DeliveryOutcome::Accepted { .. }), recipient_confirmed: false, outcome })
}

#[derive(Serialize)]
pub struct ControlReport { code: String, running: bool, installed: bool }
pub fn control_background_inner(action: String) -> Result<ControlReport, String> {
    let paths = paths()?;
    let report: ServiceReport = match action.as_str() {
        "status" | "start" | "stop" => {let root=paths.data.to_str().ok_or("path_encoding_unsupported")?;run_cli(&["--data-dir",root,"service",&action])?},
        "install" => {
            let root = paths.data.to_str().ok_or("path_encoding_unsupported")?;
            run_cli(&["--data-dir", root, "service", "install"])?
        },
        "pause" | "resume" => {
            let _security = secure(&paths, false)?;
            match ipc::call(&paths, &Request::Pause { paused: action == "pause" }).map_err(|e| e.0.to_owned())? {
                Response::Ok => return Ok(ControlReport { code: if action == "pause" { "paused" } else { "resumed" }.into(), running: true, installed: paths.data.join("installation.json").exists() }),
                Response::Error { code } => return Err(code),
                _ => return Err("background_control_failed".into()),
            }
        },
        _ => return Err("unsupported_action".into()),
    };
    if report.failed || report.unsupported { return Err(report.code); }
    Ok(ControlReport { code: report.code, running: report.running, installed: report.installed })
}

#[derive(Serialize)]
pub struct UpdateAvailability { available: bool, code: &'static str }
#[tauri::command]
pub fn update_availability() -> UpdateAvailability {
    let available = agentnotify_platform::update::update_available();
    UpdateAvailability { available, code: if available { "signed_manual_update_only" } else { "release_trust_root_missing" } }
}

#[tauri::command]
pub async fn plan_integration(input:PlanInput)->Result<SafePlanSummary,String>{tauri::async_runtime::spawn_blocking(move||plan_integration_inner(input)).await.map_err(|_|"operation_worker_failed".to_owned())?}
#[tauri::command]
pub async fn apply_integration(operation_id:Uuid)->Result<SafeVerification,String>{tauri::async_runtime::spawn_blocking(move||apply_integration_inner(operation_id)).await.map_err(|_|"operation_worker_failed".to_owned())?}
#[tauri::command]
pub async fn control_background(action:String)->Result<ControlReport,String>{tauri::async_runtime::spawn_blocking(move||control_background_inner(action)).await.map_err(|_|"operation_worker_failed".to_owned())?}
#[tauri::command]
pub async fn preview_uninstall(mode:agentnotify_cli::uninstall::UninstallMode)->Result<agentnotify_cli::uninstall::UninstallPreview,String>{
    let paths=paths()?;tauri::async_runtime::spawn_blocking(move||agentnotify_cli::uninstall::preview(paths,mode)).await.map_err(|_|"operation_worker_failed".to_owned())?
}
#[tauri::command]
pub async fn apply_uninstall(app:tauri::AppHandle,plan_id:Uuid)->Result<agentnotify_cli::uninstall::UninstallLaunch,String>{
    let paths=paths()?;let result=tauri::async_runtime::spawn_blocking(move||agentnotify_cli::uninstall::launch(paths,plan_id)).await.map_err(|_|"operation_worker_failed".to_owned())??;
    // 先把 helper_started 返回前端；关闭仍经统一草稿守卫，不强退有未保存设置的窗口。
    std::thread::spawn(move||{std::thread::sleep(Duration::from_secs(1));use tauri::Emitter;let _=app.emit("agentnotify-quit-requested",());});
    Ok(result)
}