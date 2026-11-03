//! 一次性私密输入：只填充已准备且关闭通知的邮箱引用，不读取旧工具凭据。
use crate::{commands::Invocation, output};
fn success(code: &str, message: &str) -> i32 {
    let report = agentnotify_core::contracts::CommandReport::success(serde_json::json!({"code":code,"message":message}));
    match serde_json::to_string(&report) {
        Ok(json) => { println!("{json}"); 0 },
        Err(_) => 4,
    }
}
use agentnotify_core::{config::ConfigV1, contracts::{CredentialErrorCode, CredentialStore, SecretInput}};
use agentnotify_platform::{credentials::NativeCredentialStore, paths::PlatformPaths, runtime::{HostRuntime, HostSettings}, PlatformError};
use std::io::{IsTerminal, Read};

pub fn execute(invocation: Invocation) -> i32 {
    match store(invocation) {
        Ok(()) => success("credential_stored", "授权码已保存至当前用户系统安全存储；通知仍保持关闭。"),
        Err(e) => output::report(3, e.0, "私密输入未完成；请使用本机设置窗口检查，不要在聊天或命令参数中填写授权码。", None),
    }
}
fn store(invocation: Invocation) -> Result<(), PlatformError> {
    // 终端输入可能回显，故仅接受私密窗口通过匿名管道传入的原始 UTF-8 字节。
    if std::io::stdin().is_terminal() { return Err(PlatformError("private_pipe_required")); }
    let paths = match invocation.data_dir { Some(p) => PlatformPaths::override_data(&p)?, None => PlatformPaths::discover()? };
    let host = HostRuntime::open(paths.clone(), false)?;
    let _guard = agentnotify_platform::update::configuration_guard(&paths, host.security.clone())?;
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
        .open(paths.data.join("desktop-settings.lock")).map_err(|_| PlatformError("settings_busy"))?;
    lock.try_lock().map_err(|_| PlatformError("settings_busy"))?;
    let _sender = host.queue.acquire_sender().map_err(|_| PlatformError("stop_backend_first"))?;
    let config = ConfigV1::load(&paths.data.join("config.json")).map_err(|_| PlatformError("config_invalid"))?;
    if config.enabled { return Err(PlatformError("pause_notifications_first")); }
    let settings = HostSettings::load(&paths.data)?;
    let delivery = settings.delivery.ok_or(PlatformError("configure_mail_first"))?;
    let credentials = NativeCredentialStore::default();
    match credentials.get(&delivery.credential) {
        Err(e) if e.code == CredentialErrorCode::Missing => (),
        _ => return Err(PlatformError("credential_exists_or_unavailable")),
    }
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    std::io::stdin().take(4097).read_to_end(&mut bytes).map_err(|_| PlatformError("private_input_unavailable"))?;
    if bytes.is_empty() || bytes.len() > 4096 || bytes.iter().any(|b| matches!(b, 0 | b'\r' | b'\n')) || std::str::from_utf8(&bytes).is_err() {
        return Err(PlatformError("private_input_invalid"));
    }
    credentials.put(&delivery.credential, SecretInput::new(bytes.to_vec())).map_err(|_| PlatformError("credential_write_failed"))?;
    let readback = credentials.get(&delivery.credential).map_err(|_| PlatformError("credential_readback_failed"))?;
    if !readback.with_secret(|saved| saved == bytes.as_slice()) { return Err(PlatformError("credential_readback_failed")); }
    Ok(())
}

/// 只初始化尚未配置的新实例；读取旧 settings.json 非秘密字段，旧服务保持原状。
pub fn prepare(invocation: Invocation) -> i32 {
    match prepare_inner(invocation) {
        Ok(()) => success("migration_prepared", "旧偏好和邮箱已准备，正文密钥已创建；通知关闭，授权码须私密输入。"),
        Err(e) => output::report(3, e.0, "迁移准备未完成；未切换旧通知工具。", None),
    }
}
fn prepare_inner(invocation: Invocation) -> Result<(), PlatformError> {
    use crate::commands::Command;
    use agentnotify_platform::runtime::DeliverySettings;
    use agentnotify_core::contracts::CredentialRef;
    use std::io::Write;
    let Command::MigrationPrepare(root) = invocation.command else { return Err(PlatformError("invalid_arguments")); };
    let legacy = agentnotify_integration::migration::inspect_legacy(&root).map_err(|e| PlatformError(e.0))?;
    if legacy.session_filter_count != 0 { return Err(PlatformError("session_filter_requires_review")); }
    if !agentnotify_core::delivery::mail::valid_address(&legacy.mail.sender) || !agentnotify_core::delivery::mail::valid_address(&legacy.mail.recipient) {
        return Err(PlatformError("invalid_mail_address"));
    }
    let paths = match invocation.data_dir { Some(p) => PlatformPaths::override_data(&p)?, None => PlatformPaths::discover()? };
    let host = HostRuntime::open(paths.clone(), true)?;
    let _guard = agentnotify_platform::update::configuration_guard(&paths, host.security.clone())?;
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
        .open(paths.data.join("desktop-settings.lock")).map_err(|_| PlatformError("settings_busy"))?;
    lock.try_lock().map_err(|_| PlatformError("settings_busy"))?;
    let _sender = host.queue.acquire_sender().map_err(|_| PlatformError("stop_backend_first"))?;
    if paths.data.join("host.json").exists() || paths.data.join("config.json").exists() { return Err(PlatformError("target_already_configured")); }
    let mut settings = HostSettings::default();
    settings.delivery = Some(DeliverySettings {
        host: legacy.mail.host, port: legacy.mail.port, username: legacy.mail.sender.clone(),
        tls: if legacy.mail.security == "ssl" { "tls".into() } else { "starttls".into() }, oauth:false,
        from: legacy.mail.sender, recipients: vec![legacy.mail.recipient], credential: CredentialRef { id:uuid::Uuid::new_v4() },
    });
    // 先持久化关闭态，后续任何失败都不能使发送路径开启。
    legacy.preferences.save_atomic(&paths.data.join("config.json")).map_err(|_| PlatformError("config_write_failed"))?;
    let store = NativeCredentialStore::default();
    if legacy.preferences.include_question || legacy.preferences.include_answer {
        settings.content_key = Some(store.create_content_key().map_err(|_| PlatformError("content_key_unavailable"))?.0);
    }
    let publish = (|| {
        let raw = serde_json::to_vec_pretty(&settings).map_err(|_| PlatformError("host_config_invalid"))?;
        let mut temp = tempfile::NamedTempFile::new_in(&paths.data).map_err(|_| PlatformError("host_write_failed"))?;
        temp.write_all(&raw).and_then(|_| temp.as_file().sync_all()).map_err(|_| PlatformError("host_write_failed"))?;
        temp.persist(paths.data.join("host.json")).map_err(|_| PlatformError("host_write_failed"))?;
        Ok(())
    })();
    if publish.is_err() { if let Some(key) = &settings.content_key { let _ = store.delete(key); } }
    publish
}
