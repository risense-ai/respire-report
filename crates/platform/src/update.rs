//! 手动签名更新；无内置发布公钥时严格关闭。开发zip不获得更新真实性资格。
use crate::{PlatformError, Result, paths::PlatformPaths, runtime::HostRuntime, service::{self, Installation, ServiceManager}};
use agentnotify_core::queue::{AdmissionGate, AdmissionGuard, DirectorySecurity, reject_links};
use agentnotify_core::contracts::QueueErrorCode;
use base64::Engine;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{fs::{self, File, OpenOptions}, io::Write, path::{Path, PathBuf}, sync::Arc};

/// 签名覆盖payload原始UTF-8字节；package_dir是本机包位置，不参与远端内容身份。
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedManifest { pub payload: String, pub signature: String, pub package_dir: PathBuf }
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifest {
    /// 清单协议版本；当前仅1。
    pub manifest_version: u32,
    /// 精确允许的旧版本与目标版本，三段数字，不接受隐式跨版本迁移。
    pub from_version: String, pub version: String,
    /// 当前唯一可应用目标：windows-x86_64。
    pub platform: String,
    /// schema只允许2到2；spool只允许1，后续迁移需另行实现。
    pub old_schema: u32, pub new_schema: u32, pub spool_version: u32,
    /// 三个原生exe逐个SHA256；摘要必须由发布签名覆盖。
    pub files: Vec<ReleaseFile>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseFile { pub name: String, pub sha256: String }
/// 私有字段阻止调用者把未验证清单伪装为已验证发行版。
pub struct VerifiedRelease { manifest: ReleaseManifest, signed: SignedManifest }
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateJournal {
    /// 升级操作ID用于隔离每次快照。
    pub operation_id: uuid::Uuid,
    /// 持久维护状态；崩溃后不通过删标记绕过恢复。
    pub maintenance: bool,
    /// 仅快照全部完成后写入，未完成不能声称可恢复DB。
    pub pre_snapshot: Option<PathBuf>,
    pub old_version: String, pub new_version: String,
    pub schema_compatibility: u32, pub spool_compatibility: u32,
    /// 在删除spool/开放消费之前置true，此后绝不恢复升级前快照。
    pub consumption_resumed: bool,
    pub phase: String,
    old_installation: Installation,
    /// 只存非秘密配置是否存在；授权码/token从不进入日志。
    config_present: bool, host_present: bool,
    /// 签名输入绑定prepare/apply；本机包位置可供中断后重新校验。
    signed: SignedManifest,
}
#[derive(Serialize)]
pub struct UpdateReport { pub code: String, pub maintenance: bool, pub consumption_resumed: bool, pub version: String }
pub type UpdateError = PlatformError;

/// 发布环境通过编译期变量嵌入32字节Ed25519公钥；绝不从包或运行时环境接收替换信任根。
pub fn update_available() -> bool { option_env!("AGENTNOTIFY_RELEASE_PUBLIC_KEY").is_some_and(|s| !s.is_empty()) }
pub fn verify_release(signed: &SignedManifest) -> Result<VerifiedRelease> {
    // 1. 信任根缺失即拒绝，HTTPS、包hash或开发自签名均不能替代。
    let key = option_env!("AGENTNOTIFY_RELEASE_PUBLIC_KEY").filter(|s| !s.is_empty()).ok_or(PlatformError("release_trust_root_missing"))?;
    let key = base64::engine::general_purpose::STANDARD.decode(key).map_err(|_| PlatformError("release_trust_root_invalid"))?;
    if key.len() != 32 || signed.payload.len() > 16384 || signed.signature.len() > 128 { return Err(PlatformError("release_manifest_invalid")); }
    let signature = base64::engine::general_purpose::STANDARD.decode(&signed.signature).map_err(|_| PlatformError("release_signature_invalid"))?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key).verify(signed.payload.as_bytes(), &signature).map_err(|_| PlatformError("release_signature_invalid"))?;
    let m: ReleaseManifest = serde_json::from_str(&signed.payload).map_err(|_| PlatformError("release_manifest_invalid"))?;
    if m.manifest_version != 1 || m.platform != "windows-x86_64" || !cfg!(all(windows, target_arch="x86_64")) || m.old_schema != 2 || m.new_schema != 2 || m.spool_version != 1 { return Err(PlatformError("release_incompatible")); }
    if !service::valid_version(&m.from_version) || !service::valid_version(&m.version) || version_parts(&m.version) <= version_parts(&m.from_version) { return Err(PlatformError("release_version_rejected")); }
    // 2. 固定文件集防止路径穿越与替换未签名额外文件；应用前仍再次核验。
    if !signed.package_dir.is_absolute() || m.files.len() != 3 { return Err(PlatformError("release_package_invalid")); }
    reject_links(&signed.package_dir).map_err(|_| PlatformError("unsafe_package"))?;
    for name in ["agentnotify.exe", "agentnotifyd.exe", "agentnotify-desktop.exe"] {
        let matches: Vec<_> = m.files.iter().filter(|f| f.name == name).collect();
        if matches.len() != 1 { return Err(PlatformError("release_file_set_invalid")); }
        let file = matches[0];
        let path = signed.package_dir.join(name);
        reject_links(&path).map_err(|_| PlatformError("unsafe_package"))?;
        if file.sha256.len() != 64 || !file.sha256.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) || service::hash(&path)? != file.sha256 { return Err(PlatformError("release_hash_mismatch")); }
    }
    Ok(VerifiedRelease { manifest: m, signed: signed.clone() })
}
fn version_parts(version: &str) -> Vec<u32> { version.split('.').filter_map(|n| n.parse().ok()).collect() }

/// GUI持有此guard完成配置/凭据写入；维护中拒绝旧GUI保存，锁内检查避免TOCTOU。
pub fn configuration_guard(paths: &PlatformPaths, security: Arc<dyn DirectorySecurity>) -> Result<AdmissionGuard> {
    let guard = AdmissionGate::new(paths.data.clone(), security).enter().map_err(|_| PlatformError("configuration_busy"))?;
    if maintenance_active(paths)? { return Err(PlatformError("maintenance_in_progress")); }
    Ok(guard)
}
pub fn maintenance_active(paths: &PlatformPaths) -> Result<bool> {
    let path = paths.data.join("maintenance");
    reject_links(&path).map_err(|_| PlatformError("unsafe_maintenance"))?;
    if !path.exists() { return Ok(false); }
    let (enabled, _): (bool, uuid::Uuid) = read_json(&path, 256)?;
    Ok(enabled)
}

pub struct UpdateManager { paths: PlatformPaths }
impl UpdateManager {
    pub fn new(paths: PlatformPaths) -> Self { Self { paths } }
    /// 签名/版本/包验证先于维护；prepare不切换程序，失败保留恢复日志。
    pub fn prepare(&self, signed: &SignedManifest) -> Result<UpdateReport> {
        let release = verify_release(signed)?;
        if service::read_installation(&self.paths)?.runtime_layout == service::RuntimeLayout::Unified { return Err(PlatformError("unified_signed_update_pending")); }
        let host = HostRuntime::open(self.paths.clone(), false)?;
        let _lock = update_lock(&self.paths)?;
        if maintenance_active(&self.paths)? || (self.paths.data.join("update-journal.json").exists() && !self.read_journal()?.consumption_resumed) { return Err(PlatformError("update_recovery_required")); }
        let old = service::read_installation(&self.paths)?;
        let old_version = installed_version(&old)?;
        if old_version != release.manifest.from_version { return Err(PlatformError("release_version_rejected")); }
        let id = uuid::Uuid::new_v4();
        let snapshot = self.paths.data.join("updates").join(id.to_string());
        reject_links(&snapshot).map_err(|_| PlatformError("unsafe_snapshot"))?;
        fs::create_dir_all(&snapshot).map_err(|_| PlatformError("snapshot_create_failed"))?;
        let mut journal = UpdateJournal { operation_id: id, maintenance: true, pre_snapshot: None, old_version, new_version: release.manifest.version, schema_compatibility: 2, spool_compatibility: 1, consumption_resumed: false, phase: "preparing".into(), old_installation: old, config_present: false, host_present: false, signed: release.signed };
        // 1. 日志先落盘，随后以admission互斥建立维护，再停发/收敛租约。
        self.write_journal(&journal)?;
        host.queue.set_maintenance(true).map_err(queue_error)?;
        ensure_service(ServiceManager::new(self.paths.clone()).stop()?)?;
        let sender = host.queue.acquire_sender().map_err(queue_error)?;
        host.queue.settle_stopped_sender(&sender).map_err(queue_error)?;
        // 2. 一致DB及非秘密配置快照完成前不允许程序切换。
        host.queue.backup_for_update(&snapshot.join("queue.sqlite"), &sender).map_err(queue_error)?;
        journal.config_present = snapshot_file(&self.paths.data, &snapshot, "config.json")?;
        journal.host_present = snapshot_file(&self.paths.data, &snapshot, "host.json")?;
        journal.pre_snapshot = Some(snapshot);
        journal.phase = "prepared".into();
        self.write_journal(&journal)?;
        Ok(report(&journal, "prepared"))
    }
    /// 仅对prepare绑定的发行版切换。失败保持维护；用户可rollback或恢复已提交重放。
    pub fn apply(&self) -> Result<UpdateReport> {
        let _security = self.paths.secure(false)?;
        let _lock = update_lock(&self.paths)?;
        let mut journal = self.read_journal()?;
        if journal.consumption_resumed || journal.pre_snapshot.is_none() || !maintenance_active(&self.paths)? { return Err(PlatformError("update_not_prepared")); }
        let release = verify_release(&journal.signed)?;
        if release.manifest.from_version != journal.old_version || release.manifest.version != journal.new_version { return Err(PlatformError("update_journal_invalid")); }
        let host = HostRuntime::open(self.paths.clone(), false)?;
        ensure_service(ServiceManager::new(self.paths.clone()).stop()?)?;
        let sender = host.queue.acquire_sender().map_err(queue_error)?;
        // 1. 固定版本目录接收完整原生文件；目标有同hash时视为中断重入，陌生文件拒绝覆盖。
        let target = self.paths.data.join("versions").join(&release.manifest.version);
        reject_links(&target).map_err(|_| PlatformError("unsafe_version_directory"))?;
        fs::create_dir_all(&target).map_err(|_| PlatformError("version_create_failed"))?;
        for file in &release.manifest.files { copy_verified(&release.signed.package_dir.join(&file.name), &target.join(&file.name), &file.sha256)?; }
        let mut next = journal.old_installation.clone();
        next.daemon = target.join("agentnotifyd.exe");
        next.daemon_hash = service::hash(&next.daemon)?;
        next.cli_hash = service::hash(&target.join("agentnotify.exe"))?;
        next.desktop_hash = service::hash(&target.join("agentnotify-desktop.exe"))?;
        journal.phase = "switching".into();
        self.write_journal(&journal)?;
        service::switch_installation(&self.paths, &next)?;
        // 2. 新daemon在maintenance下启动，租约/导入仍关闭；IPC健康确认后再次停发。
        drop(sender);
        ensure_service(ServiceManager::new(self.paths.clone()).start()?)?;
        ensure_service(ServiceManager::new(self.paths.clone()).stop()?)?;
        let sender = host.queue.acquire_sender().map_err(queue_error)?;
        journal.phase = "health_checked".into();
        self.write_journal(&journal)?;
        self.resume_queue(&host, &sender, &mut journal, "updated")
    }
    /// 尚未恢复消费才能还原一致旧快照；保留升级期间全部spool与独立序号。
    pub fn rollback(&self) -> Result<UpdateReport> {
        let _security = self.paths.secure(false)?;
        let _lock = update_lock(&self.paths)?;
        let mut journal = self.read_journal()?;
        if journal.consumption_resumed { return Err(PlatformError("post_resume_downgrade_blocked")); }
        let host = HostRuntime::open(self.paths.clone(), false)?;
        host.queue.set_maintenance(true).map_err(queue_error)?;
        ensure_service(ServiceManager::new(self.paths.clone()).stop()?)?;
        let sender = host.queue.acquire_sender().map_err(queue_error)?;
        // 1. 未完成快照的prepare尚未切换DB/二进制；完整快照才执行恢复。
        if let Some(snapshot) = &journal.pre_snapshot {
            if snapshot != &self.paths.data.join("updates").join(journal.operation_id.to_string()) { return Err(PlatformError("snapshot_path_invalid")); }
            host.queue.restore_update_snapshot(&snapshot.join("queue.sqlite"), &sender).map_err(queue_error)?;
            restore_file(&self.paths.data, snapshot, "config.json", journal.config_present)?;
            restore_file(&self.paths.data, snapshot, "host.json", journal.host_present)?;
        }
        service::switch_installation(&self.paths, &journal.old_installation)?;
        journal.phase = "rolled_back".into();
        self.write_journal(&journal)?;
        // 2. 重放后再开放消费；此后同样不能再次还原快照。
        self.resume_queue(&host, &sender, &mut journal, "rolled_back")
    }
    /// commit日志后崩溃只允许向前恢复；已导入记录依靠ingress/source幂等去重。
    pub fn resume(&self) -> Result<UpdateReport> {
        let _security = self.paths.secure(false)?;
        let _lock = update_lock(&self.paths)?;
        let mut journal = self.read_journal()?;
        if !journal.consumption_resumed { return Err(PlatformError("rollback_or_apply_required")); }
        if !maintenance_active(&self.paths)? { return Ok(report(&journal, "already_resumed")); }
        let host = HostRuntime::open(self.paths.clone(), false)?;
        ensure_service(ServiceManager::new(self.paths.clone()).stop()?)?;
        let sender = host.queue.acquire_sender().map_err(queue_error)?;
        self.resume_queue(&host, &sender, &mut journal, "resumed")
    }
    pub fn status(&self) -> Result<UpdateReport> {
        let _security = self.paths.secure(false)?;
        let journal = self.read_journal()?;
        Ok(report(&journal, &journal.phase))
    }
    fn resume_queue(&self, host: &HostRuntime, sender: &agentnotify_core::queue::SenderGuard, journal: &mut UpdateJournal, code: &str) -> Result<UpdateReport> {
        host.queue.finish_update_maintenance(sender, || {
            journal.consumption_resumed = true;
            self.write_journal(journal).map_err(|_| agentnotify_core::queue::error(QueueErrorCode::StorageFailed))
        }).map_err(queue_error)?;
        journal.maintenance = false;
        journal.phase = code.into();
        self.write_journal(journal)?;
        // 调用者仍持sender锁，后台由显式service start/GUI恢复，不能在持锁时声称启动成功。
        Ok(report(journal, code))
    }
    fn read_journal(&self) -> Result<UpdateJournal> { read_json(&self.paths.data.join("update-journal.json"), 65536) }
    fn write_journal(&self, journal: &UpdateJournal) -> Result<()> { write_json(&self.paths.data.join("update-journal.json"), journal) }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UninstallPlan { pub installation_sha256: String, pub preserve_data: bool, pub preserve_credentials: bool }
#[derive(Serialize)]
pub struct UninstallReport { pub service_removed: bool, pub data_preserved: bool, pub credentials_preserved: bool, pub hooks_preserved: bool }
pub struct UninstallManager { paths: PlatformPaths }
impl UninstallManager {
    pub fn new(paths: PlatformPaths) -> Self { Self { paths } }
    /// 当前仅卸载仍归属本工具的登录任务；Hook/用户数据默认全部保留供人工决定。
    pub fn plan(&self) -> Result<UninstallPlan> {
        let _security = self.paths.secure(false)?;
        service::read_installation(&self.paths)?;
        Ok(UninstallPlan { installation_sha256: service::hash(&self.paths.data.join("installation.json"))?, preserve_data: true, preserve_credentials: true })
    }
    pub fn apply(&self, plan: &UninstallPlan) -> Result<UninstallReport> {
        let security = self.paths.secure(false)?;
        let _lock = update_lock(&self.paths)?;
        let _configuration = configuration_guard(&self.paths, security)?;
        if !plan.preserve_data || !plan.preserve_credentials || plan.installation_sha256 != service::hash(&self.paths.data.join("installation.json"))? { return Err(PlatformError("uninstall_plan_conflict")); }
        // stop可能等待在途完成，因此不持admission锁等待发送器退出。
        drop(_configuration);
        let report = ServiceManager::new(self.paths.clone()).uninstall()?;
        ensure_service(report)?;
        Ok(UninstallReport { service_removed: true, data_preserved: true, credentials_preserved: true, hooks_preserved: true })
    }
}
fn queue_error(_: agentnotify_core::contracts::QueueError) -> PlatformError { PlatformError("maintenance_queue_failed") }
fn ensure_service(report: service::ServiceReport) -> Result<()> { if report.failed || report.unsupported { Err(PlatformError("update_service_unavailable")) } else { Ok(()) } }
fn report(j: &UpdateJournal, code: &str) -> UpdateReport { UpdateReport { code: code.into(), maintenance: j.maintenance, consumption_resumed: j.consumption_resumed, version: if j.phase == "rolled_back" { j.old_version.clone() } else { j.new_version.clone() } } }
fn installed_version(i: &Installation) -> Result<String> { i.daemon.parent().and_then(|p| p.file_name()).and_then(|s| s.to_str()).map(str::to_owned).ok_or(PlatformError("installation_invalid")) }
fn update_lock(paths: &PlatformPaths) -> Result<File> {
    let path = paths.data.join("update.lock");
    reject_links(&path).map_err(|_| PlatformError("unsafe_update_lock"))?;
    let f = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path).map_err(|_| PlatformError("update_lock_failed"))?;
    f.try_lock().map_err(|_| PlatformError("update_busy"))?;
    Ok(f)
}
fn read_json<T: DeserializeOwned>(path: &Path, max: u64) -> Result<T> {
    reject_links(path).map_err(|_| PlatformError("unsafe_update_file"))?;
    if fs::metadata(path).map_err(|_| PlatformError("update_state_unavailable"))?.len() > max { return Err(PlatformError("update_state_invalid")); }
    serde_json::from_slice(&fs::read(path).map_err(|_| PlatformError("update_state_unavailable"))?).map_err(|_| PlatformError("update_state_invalid"))
}
pub fn read_signed_manifest(path: &Path) -> Result<SignedManifest> { read_json(path, 32768) }
/// 临时文件fsync后原子替换；不删除原文件制造空窗。
pub(crate) fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| PlatformError("update_state_invalid"))?;
    write_bytes(path, &bytes)
}
fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    reject_links(path).map_err(|_| PlatformError("unsafe_update_file"))?;
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().ok_or(PlatformError("update_path_invalid"))?).map_err(|_| PlatformError("update_write_failed"))?;
    temp.write_all(bytes).and_then(|_| temp.as_file().sync_all()).map_err(|_| PlatformError("update_write_failed"))?;
    #[cfg(windows)] {
        use std::os::windows::ffi::OsStrExt;
        let from: Vec<_> = temp.path().as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // 缓冲区在同步调用期间存活；WRITE_THROUGH保证日志先于恢复消费落盘。
        let ok = unsafe { windows_sys::Win32::Storage::FileSystem::MoveFileExW(from.as_ptr(), to.as_ptr(), windows_sys::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING | windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH) };
        if ok == 0 { return Err(PlatformError("update_write_failed")); }
    }
    #[cfg(not(windows))] { temp.persist(path).map_err(|_| PlatformError("update_write_failed"))?; }
    Ok(())
}
fn snapshot_file(root: &Path, snapshot: &Path, name: &str) -> Result<bool> {
    let path = root.join(name);
    reject_links(&path).map_err(|_| PlatformError("unsafe_config"))?;
    if !path.exists() { return Ok(false); }
    let bytes = fs::read(path).map_err(|_| PlatformError("snapshot_read_failed"))?;
    if bytes.len() > 1048576 { return Err(PlatformError("snapshot_config_invalid")); }
    write_bytes(&snapshot.join(name), &bytes)?;
    Ok(true)
}
fn restore_file(root: &Path, snapshot: &Path, name: &str, present: bool) -> Result<()> {
    let target = root.join(name);
    reject_links(&target).map_err(|_| PlatformError("unsafe_config"))?;
    if present { let source = snapshot.join(name); reject_links(&source).map_err(|_| PlatformError("unsafe_snapshot"))?; write_bytes(&target, &fs::read(source).map_err(|_| PlatformError("snapshot_read_failed"))?)?; }
    else if target.exists() { return Err(PlatformError("configuration_changed_during_maintenance")); }
    Ok(())
}
fn copy_verified(from: &Path, to: &Path, hash: &str) -> Result<()> {
    reject_links(to).map_err(|_| PlatformError("unsafe_package_target"))?;
    if to.exists() { return if service::hash(to)? == hash { Ok(()) } else { Err(PlatformError("version_file_conflict")) }; }
    // 对复制后的字节再验签名内hash，防止源包在verify后被并行替换。
    let mut input = File::open(from).map_err(|_| PlatformError("package_unavailable"))?;
    let mut output = OpenOptions::new().write(true).create_new(true).open(to).map_err(|_| PlatformError("version_file_conflict"))?;
    std::io::copy(&mut input, &mut output).and_then(|_| output.sync_all()).map_err(|_| PlatformError("package_copy_failed"))?;
    if service::hash(to)? != hash { return Err(PlatformError("release_hash_mismatch")); }
    Ok(())
}
