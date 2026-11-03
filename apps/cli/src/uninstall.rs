//! 两阶段卸载。预演不停止服务；独立同字节 helper 只处理固定白名单，保留未知文件。
use agentnotify_core::{
    contracts::{CredentialRef, CredentialStore},
    queue::{AdmissionGate, reject_links},
};
use agentnotify_integration::{
    IntegrationManager,
    remove::{self, RemovalFile},
};
use agentnotify_platform::{
    credentials::NativeCredentialStore,
    paths::PlatformPaths,
    runtime::HostSettings,
    service::{self, ServiceManager},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use uuid::Uuid;
type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UninstallMode {
    KeepSettings,
    RemoveSettings,
}
#[derive(Serialize)]
pub struct UninstallPreview {
    pub plan_id: Uuid,
    pub mode: UninstallMode,
    pub remove_items: Vec<String>,
    pub preserve_items: Vec<String>,
    pub blockers: Vec<String>,
    pub credential_count: usize,
}
#[derive(Serialize)]
pub struct UninstallLaunch {
    pub status: &'static str,
    pub report_path: PathBuf,
}
#[derive(Serialize, Deserialize)]
struct Plan {
    id: Uuid,
    mode: UninstallMode,
    sid: String,
    root: PathBuf,
    created: i64,
    manifest: String,
    config: Option<String>,
    host: Option<String>,
    bindings: Option<String>,
    runtime: Vec<PathBuf>,
    hooks: Vec<RemovalFile>,
    credentials: Vec<CredentialRef>,
    helper_hash: String,
    shortcut: Option<(PathBuf, String)>,
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn digest(path: &Path) -> Result<Option<String>> {
    reject_links(path).map_err(|_| "unsafe_uninstall_path")?;
    if !path.exists() {
        return Ok(None);
    }
    service::file_digest(path).map(Some).map_err(err)
}
fn lock(root: &Path, name: &str) -> Result<File> {
    let p = root.join(name);
    reject_links(&p).map_err(|_| "unsafe_uninstall_path")?;
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(p)
        .map_err(|_| "uninstall_lock_failed")?;
    f.try_lock().map_err(|_| "uninstall_busy")?;
    Ok(f)
}
fn write(path: &Path, v: &impl Serialize) -> Result<()> {
    reject_links(path).map_err(|_| "unsafe_uninstall_path")?;
    let mut f = tempfile::NamedTempFile::new_in(path.parent().ok_or("unsafe_uninstall_path")?)
        .map_err(|_| "uninstall_write_failed")?;
    serde_json::to_writer_pretty(&mut f, v).map_err(|_| "uninstall_write_failed")?;
    f.flush()
        .and_then(|_| f.as_file().sync_all())
        .map_err(|_| "uninstall_write_failed")?;
    f.persist(path).map_err(|_| "uninstall_write_failed")?;
    Ok(())
}
#[cfg(windows)]
fn sid() -> Result<String> {
    agentnotify_platform::windows::current_sid().map_err(err)
}
#[cfg(not(windows))]
fn sid() -> Result<String> {
    Err("uninstall_platform_unsupported".into())
}
fn plan_path(root: &Path, id: Uuid) -> PathBuf {
    root.join(format!("uninstall-plan-{id}.json"))
}
fn helper_root(id: Uuid) -> Result<PathBuf> {
    #[cfg(windows)]
    {
        Ok(agentnotify_platform::windows::local_data()
            .map_err(err)?
            .join(format!("AgentNotify-Uninstall-{id}")))
    }
    #[cfg(not(windows))]
    {
        let _ = id;
        Err("uninstall_platform_unsupported".into())
    }
}
/// 预览仅持有短设置锁，输出路径/范围与数量，不输出邮箱值、Hook正文或凭据。
pub fn preview(paths: PlatformPaths, mode: UninstallMode) -> Result<UninstallPreview> {
    paths.secure(false).map_err(err)?;
    let _settings = lock(&paths.data, "desktop-settings.lock")?;
    let runtime = service::uninstall_runtime_files(&paths).map_err(err)?;
    let manager = IntegrationManager::new(paths.data.clone());
    let hooks = remove::preview(&manager).map_err(|e| e.code)?;
    let host = HostSettings::load(&paths.data).map_err(err)?;
    let credentials = references(&host);
    validate_retirement(&paths, &manager)?;
    let shortcut = service::owned_desktop_shortcut(&paths)
        .map_err(err)?
        .map(|p| service::file_digest(&p).map(|h| (p, h)))
        .transpose()
        .map_err(err)?;
    let id = Uuid::new_v4();
    let plan = Plan {
        shortcut,
        id,
        mode,
        sid: sid()?,
        root: paths.data.clone(),
        created: chrono::Utc::now().timestamp(),
        manifest: digest(&paths.data.join("installation.json"))?.ok_or("installation_missing")?,
        config: digest(&paths.data.join("config.json"))?,
        host: digest(&paths.data.join("host.json"))?,
        bindings: digest(&manager.bindings_path())?,
        runtime,
        hooks,
        credentials,
        helper_hash: service::file_digest(
            &std::env::current_exe().map_err(|_| "executable_unavailable")?,
        )
        .map_err(err)?,
    };
    let mut remove_items = vec![
        "本工具后台与登录自启动".into(),
        "归属已确认且未被修改的接入 Hook".into(),
        "通知历史、待发送通知、观察游标与实例绑定（重装重新建立基线）".into(),
    ];
    if let Some((p, _)) = &plan.shortcut {
        remove_items.push(format!("本安装桌面快捷方式：{}", p.display()));
    }
    remove_items.extend(plan.runtime.iter().map(|p| p.display().to_string()));
    let mut preserve_items = vec![
        "未知文件、旧备份和下载目录原始程序不会删除".into(),
        "卸载结果报告与临时清理 helper（供失败排查）".into(),
        "其他位置或被改动的快捷方式保留；需手动核对".into(),
    ];
    if mode == UninstallMode::KeepSettings {
        preserve_items.push("邮箱设置、通知偏好及本安装引用的授权凭据".into());
    } else {
        remove_items.push("邮箱设置、通知偏好及本安装引用的凭据".into());
    }
    for entry in fs::read_dir(&paths.data).map_err(|_| "uninstall_scan_failed")? {
        let p = entry.map_err(|_| "uninstall_scan_failed")?.path();
        if p.is_dir() {
            preserve_items.push(format!("未证明归属的目录内容保留：{}", p.display()));
        }
    }
    write(&plan_path(&paths.data, id), &plan)?;
    write(&paths.data.join("uninstall-current.json"), &id)?;
    Ok(UninstallPreview {
        plan_id: id,
        mode,
        remove_items,
        preserve_items,
        blockers: vec![],
        credential_count: plan.credentials.len(),
    })
}
fn load(paths: &PlatformPaths, id: Uuid) -> Result<Plan> {
    paths.secure(false).map_err(err)?;
    let p = plan_path(&paths.data, id);
    reject_links(&p).map_err(|_| "unsafe_uninstall_path")?;
    if fs::metadata(&p)
        .map_err(|_| "uninstall_plan_missing")?
        .len()
        > 2097152
    {
        return Err("uninstall_plan_invalid".into());
    }
    let plan: Plan = serde_json::from_slice(&fs::read(p).map_err(|_| "uninstall_plan_missing")?)
        .map_err(|_| "uninstall_plan_invalid")?;
    if plan.id != id
        || plan.sid != sid()?
        || plan.root != paths.data
        || chrono::Utc::now().timestamp() - plan.created > 600
    {
        return Err("uninstall_plan_expired".into());
    }
    Ok(plan)
}
fn verify(paths: &PlatformPaths, p: &Plan) -> Result<()> {
    let current_shortcut = service::owned_desktop_shortcut(paths)
        .map_err(err)?
        .map(|path| service::file_digest(&path).map(|h| (path, h)))
        .transpose()
        .map_err(err)?;
    if current_shortcut != p.shortcut {
        return Err("uninstall_shortcut_changed".into());
    }

    if references(&HostSettings::load(&paths.data).map_err(err)?) != p.credentials {
        return Err("uninstall_credential_reference_conflict".into());
    }
    let current: Uuid = serde_json::from_slice(
        &fs::read(paths.data.join("uninstall-current.json"))
            .map_err(|_| "uninstall_plan_expired")?,
    )
    .map_err(|_| "uninstall_plan_expired")?;
    if current != p.id {
        return Err("uninstall_plan_expired".into());
    }
    validate_retirement(paths, &IntegrationManager::new(paths.data.clone()))?;
    if digest(&paths.data.join("installation.json"))? != Some(p.manifest.clone())
        || digest(&paths.data.join("config.json"))? != p.config
        || digest(&paths.data.join("host.json"))? != p.host
        || digest(&paths.data.join("integrations/instances.json"))? != p.bindings
        || service::uninstall_runtime_files(paths).map_err(err)? != p.runtime
    {
        return Err("uninstall_plan_conflict".into());
    }
    let now = remove::preview(&IntegrationManager::new(paths.data.clone())).map_err(|e| e.code)?;
    if serde_json::to_vec(&now).ok() != serde_json::to_vec(&p.hooks).ok() {
        return Err("owned_hook_conflict".into());
    }
    Ok(())
}
/// 只启动受限 helper；最终结果写 report，调用方不得把此返回视为完成。
pub fn launch(paths: PlatformPaths, id: Uuid) -> Result<UninstallLaunch> {
    let p = load(&paths, id)?;
    verify(&paths, &p)?;
    let helper = helper_root(id)?;
    if helper.exists() {
        return Err("uninstall_already_started".into());
    }
    PlatformPaths::override_data(&helper)
        .map_err(err)?
        .secure(true)
        .map_err(err)?;
    let exe = helper.join("AgentNotify-Uninstall.exe");
    fs::copy(
        std::env::current_exe().map_err(|_| "executable_unavailable")?,
        &exe,
    )
    .map_err(|_| "uninstall_helper_failed")?;
    if service::file_digest(&exe).map_err(err)? != p.helper_hash {
        return Err("uninstall_helper_changed".into());
    }
    let mut cmd = Command::new(exe);
    cmd.arg("--data-dir")
        .arg(&paths.data)
        .arg("uninstall-helper")
        .arg(id.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    cmd.spawn().map_err(|_| "uninstall_helper_failed")?;
    Ok(UninstallLaunch {
        status: "helper_started",
        report_path: paths.data.join("uninstall-result.json"),
    })
}
/// 等待用户确认退出后才操作。每次执行需未消费 token 与同 SID/root/hash，不接受删除路径实参。
fn finish_inner(paths: PlatformPaths, id: Uuid) -> Result<()> {
    let p = load(&paths, id)?;
    let expected = helper_root(id)?.join("AgentNotify-Uninstall.exe");
    if fs::canonicalize(std::env::current_exe().map_err(|_| "executable_unavailable")?).ok()
        != fs::canonicalize(&expected).ok()
        || service::file_digest(&expected).map_err(err)? != p.helper_hash
    {
        return Err("uninstall_helper_changed".into());
    }
    let _operation = lock(&paths.data, "uninstall.lock")?;
    let consumed = paths.data.join(format!("uninstall-consumed-{id}"));
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&consumed)
        .map_err(|_| "uninstall_already_started")?;
    let result = (|| {
        // 1. 等待同 root GUI 的真实锁释放，不按进程名称结束窗口或后台。
        let end = Instant::now() + Duration::from_secs(90);
        let _gui = loop {
            match lock(&paths.data, "gui.lock") {
                Ok(f) => break f,
                Err(_) if Instant::now() < end => std::thread::sleep(Duration::from_millis(100)),
                Err(_) => return Err("uninstall_window_still_open".into()),
            }
        };
        let _settings = lock(&paths.data, "desktop-settings.lock")?;
        verify(&paths, &p)?;
        // 2. admission 内标记维护，禁止新接收/领取；正常卸载等待发送者退出，绝不强杀。
        let security = paths.secure(false).map_err(err)?;
        {
            let _admission = AdmissionGate::new(paths.data.clone(), security.clone())
                .enter()
                .map_err(|_| "uninstall_busy")?;
            write(&paths.data.join("maintenance"), &(true, id))?;
        }
        let service = ServiceManager::new(paths.clone())
            .uninstall()
            .map_err(err)?;
        if service.failed || service.running || service.unsupported {
            return Err("uninstall_service_incomplete".into());
        }
        let _service = lock(&paths.data, "service.lock")?;
        let _sender = lock(&paths.data, "sender.lock")?;
        let _admission = AdmissionGate::new(paths.data.clone(), security)
            .enter()
            .map_err(|_| "uninstall_busy")?;
        verify(&paths, &p)?;
        let original_installation: serde_json::Value = serde_json::from_slice(
            &fs::read(paths.data.join("installation.json")).map_err(|_| "installation_missing")?,
        )
        .map_err(|_| "installation_invalid")?;
        if paths.data.join("queue.sqlite").exists() {
            let db = rusqlite::Connection::open_with_flags(
                paths.data.join("queue.sqlite"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .map_err(|_| "uninstall_queue_unavailable")?;
            let count:i64=db.query_row("SELECT count(*) FROM notifications WHERE status IN ('leased','delivery_unknown')",[],|r|r.get(0)).map_err(|_|"uninstall_queue_unavailable")?;
            if count > 0 {
                return Err("uninstall_delivery_unresolved".into());
            }
        }
        remove::apply(&IntegrationManager::new(paths.data.clone()), &p.hooks)
            .map_err(|e| e.code)?;
        // 3. 仅删除本安装记录与固定状态名；未知备份/目录不递归进入。
        let bindings = IntegrationManager::new(paths.data.clone())
            .bindings()
            .map_err(|e| e.code)?;
        let mut targets = vec![
            "queue.sqlite".to_string(),
            "queue.sqlite-wal".into(),
            "queue.sqlite-shm".into(),
            "integrations/instances.json".into(),
            "gui.ready.json".into(),
            "hook-diagnostic.json".into(),
        ];
        for b in &bindings {
            for suffix in [".sqlite", ".sqlite-wal", ".sqlite-shm", ".sqlite-journal"] {
                targets.push(format!("codex-observer-{}{suffix}", b.instance_id));
            }
        }
        for b in &bindings {
            targets.push(format!("sequences/{}", b.instance_id));
            targets.push(format!("sequences/{}.order-unknown", b.instance_id));
        }

        if p.mode == UninstallMode::RemoveSettings {
            for r in &p.credentials {
                NativeCredentialStore::default()
                    .delete(r)
                    .map_err(|_| "uninstall_credential_delete_failed")?;
            }
            targets.extend(["config.json".into(), "host.json".into()]);
        } else {
            let mut host = HostSettings::load(&paths.data).map_err(err)?;
            host.instances.clear();
            write(&paths.data.join("host.json"), &host)?;
        }
        for name in targets {
            delete_file(&paths.data.join(name))?;
        }
        if let Some((shortcut, hash)) = &p.shortcut {
            if digest(shortcut)? != Some(hash.clone()) {
                return Err("uninstall_shortcut_changed".into());
            }
            delete_file(shortcut)?;
        }
        for file in &p.runtime {
            delete_file(file)?;
        }
        delete_file(&paths.data.join("installation.json"))?;
        delete_file(&paths.data.join("maintenance"))?;
        agentnotify_platform::uninstall_receipt::create(&paths, original_installation)
            .map_err(err)?;
        Ok(())
    })();
    result
}

fn delete_file(path: &Path) -> Result<()> {
    reject_links(path).map_err(|_| "unsafe_uninstall_path")?;
    if !path.exists() {
        return Ok(());
    }
    if !path.is_file() {
        return Err("uninstall_target_changed".into());
    }
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        match fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(_) if Instant::now() < end => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => return Err("uninstall_file_delete_failed".into()),
        }
    }
}

fn references(host: &HostSettings) -> Vec<CredentialRef> {
    let mut refs = Vec::new();
    if let Some(d) = &host.delivery {
        refs.push(d.credential.clone());
    }
    if let Some(k) = &host.content_key {
        if !refs.contains(k) {
            refs.push(k.clone());
        }
    }
    refs
}
/// 孤立观察库不能静默留给重装继续去重；无法证明绑定时先阻止，交由协助处理。
fn validate_retirement(paths: &PlatformPaths, manager: &IntegrationManager) -> Result<()> {
    let bindings = manager.bindings().map_err(|e| e.code)?;
    let queue = paths.data.join("queue.sqlite");
    if queue.exists() {
        reject_links(&queue).map_err(|_| "unsafe_uninstall_path")?;
        let db = rusqlite::Connection::open_with_flags(
            queue,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|_| "uninstall_queue_unavailable")?;
        let count: i64 = db
            .query_row(
                "SELECT count(*) FROM notifications WHERE status IN ('leased','delivery_unknown')",
                [],
                |r| r.get(0),
            )
            .map_err(|_| "uninstall_queue_unavailable")?;
        if count > 0 {
            return Err("uninstall_delivery_unresolved".into());
        }
    }

    for entry in fs::read_dir(&paths.data).map_err(|_| "uninstall_scan_failed")? {
        let entry = entry.map_err(|_| "uninstall_scan_failed")?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(raw) = name
            .strip_prefix("codex-observer-")
            .and_then(|s| s.strip_suffix(".sqlite"))
        {
            let id = Uuid::parse_str(raw).map_err(|_| "uninstall_observer_ownership_unknown")?;
            let binding = bindings
                .iter()
                .find(|b| b.instance_id == id && b.adapter_id == "codex")
                .ok_or("uninstall_observer_ownership_unknown")?;
            reject_links(&entry.path()).map_err(|_| "unsafe_uninstall_path")?;
            let db = rusqlite::Connection::open_with_flags(
                entry.path(),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .map_err(|_| "uninstall_observer_ownership_unknown")?;
            let actual: String = db
                .query_row("SELECT value FROM meta WHERE key='binding'", [], |r| {
                    r.get(0)
                })
                .map_err(|_| "uninstall_observer_ownership_unknown")?;
            let home = binding
                .codex_home
                .as_ref()
                .ok_or("uninstall_observer_ownership_unknown")?
                .canonicalize()
                .map_err(|_| "uninstall_observer_ownership_unknown")?;
            if actual
                != format!("{}|{}|{}", id, binding.product_version, home.display()).to_lowercase()
            {
                return Err("uninstall_observer_ownership_unknown".into());
            }
        }
    }
    let spool = paths.data.join("spool");
    reject_links(&spool).map_err(|_| "unsafe_uninstall_path")?;
    if spool.exists() {
        for e in fs::read_dir(spool).map_err(|_| "uninstall_scan_failed")? {
            if e.map_err(|_| "uninstall_scan_failed")?
                .path()
                .extension()
                .is_some_and(|s| s == "spool")
            {
                return Err("uninstall_spool_pending".into());
            }
        }
    }
    Ok(())
}
/// 早期失败也展示真实失败；仅安全 root 可以写报告，安全验证失败绝不绕过权限。
pub fn finish(paths: PlatformPaths, id: Uuid) -> Result<()> {
    let result = finish_inner(paths.clone(), id);
    let report = paths.data.join("uninstall-result.json");
    let reported = if paths.secure(false).is_ok() {
        write(&report,&serde_json::json!({"status":if result.is_ok(){"completed"}else{"incomplete"},"code":result.as_ref().err().map(String::as_str).unwrap_or("uninstalled"),"unknown_files_preserved":true,"temporary_helper_preserved":true})).is_ok()
    } else {
        false
    };
    agentnotify_platform::service::show_uninstall_result(result.is_ok() && reported, &report);
    result.and(if reported {
        Ok(())
    } else {
        Err("uninstall_report_unavailable".into())
    })
}
