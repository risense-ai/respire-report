//! 文件admission和独立序号；持锁至envelope提交DB或原子spool落盘。
use super::*;
use std::{
    fs::{File, OpenOptions},
    io::Write,
    time::Instant,
};
/// 平台实现必须实际检查所有者/ACL、共享写权限与reparse路径；不能用调用方bool证明可信。
/// 返回Ok意味着路径及其子目录由当前用户独占写入，且攻击者不能替换父路径。
pub trait DirectorySecurity: Send + Sync {
    fn verify(&self, path: &Path) -> Result<(), QueueError>;
}
/// 任务05接通平台验证前失败关闭，绝不把路径存在当成安全证据。
pub struct SystemDirectorySecurity;
impl DirectorySecurity for SystemDirectorySecurity {
    fn verify(&self, _: &Path) -> Result<(), QueueError> {
        Err(error(QueueErrorCode::StorageUnavailable))
    }
}
/// 检查已存在的每一级路径；Windows junction也作为reparse point拒绝。
pub fn reject_links(path: &Path) -> Result<(), QueueError> {
    for p in path.ancestors() {
        match std::fs::symlink_metadata(p) {
            Ok(m) => {
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if m.file_attributes() & 0x400 != 0 {
                        return Err(error(QueueErrorCode::StorageUnavailable));
                    }
                }
                if m.file_type().is_symlink() {
                    return Err(error(QueueErrorCode::StorageUnavailable));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(error(QueueErrorCode::StorageUnavailable)),
        }
    }
    Ok(())
}
pub struct AdmissionGate {
    root: PathBuf,
    security: Arc<dyn DirectorySecurity>,
}
/// File持有OS锁，drop/进程退出自动释放；不删除lock文件以免产生双锁inode。
pub struct AdmissionGuard {
    file: File,
}
impl Drop for AdmissionGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}
impl AdmissionGate {
    pub fn new(root: PathBuf, security: Arc<dyn DirectorySecurity>) -> Self {
        Self { root, security }
    }
    pub fn enter(&self) -> Result<AdmissionGuard, QueueError> {
        self.enter_until(Instant::now() + Duration::from_millis(700))
    }
    /// 导入者传入整轮deadline，不能重新取得Hook的700ms等待预算。
    pub(crate) fn enter_until(&self, deadline: Instant) -> Result<AdmissionGuard, QueueError> {
        check_deadline(Some(deadline))?;
        self.security.verify(&self.root)?;
        reject_links(&self.root)?;
        check_deadline(Some(deadline))?;
        let lock = self.root.join("admission.lock");
        reject_links(&lock)?;
        let file = storage(
            OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(lock),
        )?;
        loop {
            check_deadline(Some(deadline))?;
            match file.try_lock() {
                Ok(()) => {
                    let guard = AdmissionGuard { file };
                    // try_lock成功时也可能已经越过截止点，必须先归还锁，不再开始下一项工作。
                    check_deadline(Some(deadline))?;
                    return Ok(guard);
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(error(QueueErrorCode::AdmissionTimeout));
                    }
                    std::thread::sleep(remaining.min(Duration::from_millis(2)));
                }
                Err(_) => return Err(error(QueueErrorCode::StorageUnavailable)),
            }
        }
    }
}
impl SqliteQueue {
    pub fn set_maintenance(&self, enabled: bool) -> Result<(), QueueError> {
        let _guard = self.gate.enter()?;
        // 标记含操作ID，持久化整个替换而非简单删除；恢复/迁移协调由升级器完成。
        let bytes = serde_json::to_vec(&(enabled, uuid::Uuid::new_v4()))
            .map_err(|_| error(QueueErrorCode::StorageFailed))?;
        atomic_write(&self.root.join("maintenance"), &bytes, |_| Ok(()), None)?;
        Ok(())
    }
    pub fn maintenance(&self) -> Result<bool, QueueError> {
        let p = self.root.join("maintenance");
        reject_links(&p)?;
        match std::fs::read(p) {
            Ok(b) => {
                let (enabled, _): (bool, uuid::Uuid) = storage(serde_json::from_slice(&b))?;
                Ok(enabled)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err(error(QueueErrorCode::StorageFailed)),
        }
    }
    /// 独立于SQLite和序号值的持久降级证据，必须先于未知序号envelope的确认落盘。
    /// 标记不自动删除：后来补回连续序号也不能撤销之前的观察缺口。
    pub(crate) fn mark_order_unknown(&self, instance: uuid::Uuid) -> Result<(), QueueError> {
        if self.order_degraded(instance)? {
            return Ok(());
        }
        let path = self
            .root
            .join("sequences")
            .join(format!("{instance}.order-unknown"));
        atomic_write(&path, b"ANQ-ORDER-UNKNOWN-1\n", |_| Ok(()), None)
    }
    pub(crate) fn order_degraded(&self, instance: uuid::Uuid) -> Result<bool, QueueError> {
        let path = self
            .root
            .join("sequences")
            .join(format!("{instance}.order-unknown"));
        reject_links(&path)?;
        match std::fs::read(path) {
            Ok(bytes) if bytes == b"ANQ-ORDER-UNKNOWN-1\n" => Ok(true),
            Ok(_) => Err(error(QueueErrorCode::StorageFailed)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err(error(QueueErrorCode::StorageFailed)),
        }
    }
    pub(crate) fn next_sequence(&self, instance: uuid::Uuid) -> Result<u64, QueueError> {
        let p = self.root.join("sequences").join(instance.to_string());
        reject_links(&p)?;
        let previous = match std::fs::read(&p) {
            Ok(b) if b.len() == 8 => u64::from_le_bytes(b.try_into().unwrap()),
            Ok(_) => return Err(error(QueueErrorCode::StorageFailed)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(_) => return Err(error(QueueErrorCode::StorageFailed)),
        };
        let next = previous
            .checked_add(1)
            .filter(|n| *n <= MAX_SAFE_INTEGER)
            .ok_or_else(|| error(QueueErrorCode::StorageFailed))?;
        atomic_write(
            &p,
            &next.to_le_bytes(),
            |point| self.deps.faults.check(point),
            Some(true),
        )?;
        Ok(next)
    }
    pub(crate) fn persist_locked(
        &self,
        e: IngressEnvelopeV1,
        force_spool: bool,
    ) -> Result<EnqueueOutcome, QueueError> {
        // 未知序号的降级证据先于DB/spool持久化，后续连续序号不能抢先作为Ordered观察。
        if e.per_instance_sequence.is_none() {
            self.mark_order_unknown(instance(&e))?;
        }
        // 相同实例存在积压或maintenance期间只落spool，避免后来的Stop越过Started。
        if force_spool || self.maintenance()? || self.has_backlog(instance(&e))? {
            return self.write_spool(&e);
        }
        match self.ingest_transaction(&e, Duration::from_secs(1), false, None) {
            Ok(out) => Ok(out),
            Err(_) => self.write_spool(&e),
        }
    }
}
impl Ingress for SqliteQueue {
    fn accept(&self, mut output: AdapterOutput) -> Result<EnqueueOutcome, QueueError> {
        // 1. 本机时钟统一TTL；进入锁前仅校验/清理，不执行持久副作用。
        let received_at = self.deps.clock.now();
        if let Some(n) = &mut output.notification {
            n.received_at = received_at;
        }
        if let Some(l) = &mut output.lifecycle {
            l.received_at = received_at;
        }
        let mut e = self.sanitize(IngressEnvelopeV1 {
            presence_suppressed: false,
            schema_version: 1,
            ingress_id: uuid::Uuid::new_v4(),
            received_at,
            per_instance_sequence: None,
            adapter_output: output,
        })?;
        let _guard = self.gate.enter()?;
        // 2. 序号故障仍只通过UUID应急spool接受；不声称恢复真实顺序。
        e.per_instance_sequence = self.next_sequence(instance(&e)).ok();
        let unknown = e.per_instance_sequence.is_none();
        self.persist_locked(e, unknown)
    }
}
impl Queue for SqliteQueue {
    fn ingest(&self, envelope: IngressEnvelopeV1) -> Result<EnqueueOutcome, QueueError> {
        let envelope = self.sanitize(envelope)?;
        let _guard = self.gate.enter()?;
        self.persist_locked(envelope, false)
    }
}
/// 写临时文件、flush/fsync、原子替换。Windows用MOVEFILE_WRITE_THROUGH保证改名落盘。
/// sequence=true启用序号故障点，false为spool故障点；None是维护标记。
pub(crate) fn atomic_write(
    path: &Path,
    bytes: &[u8],
    check: impl Fn(FaultPoint) -> Result<(), QueueError>,
    sequence: Option<bool>,
) -> Result<(), QueueError> {
    reject_links(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| error(QueueErrorCode::StorageFailed))?;
    if let Some(seq) = sequence {
        check(if seq {
            FaultPoint::BeforeSequenceWrite
        } else {
            FaultPoint::BeforeSpoolTemp
        })?;
    }
    let mut temp = storage(tempfile::NamedTempFile::new_in(parent))?;
    storage(temp.write_all(bytes))?;
    if let Some(seq) = sequence {
        check(if seq {
            FaultPoint::AfterSequenceWrite
        } else {
            FaultPoint::AfterSpoolWrite
        })?;
    }
    storage(temp.as_file_mut().sync_all())?;
    if let Some(seq) = sequence {
        check(if seq {
            FaultPoint::AfterSequenceSync
        } else {
            FaultPoint::AfterSpoolSync
        })?;
        check(if seq {
            FaultPoint::BeforeSequenceRename
        } else {
            FaultPoint::BeforeSpoolRename
        })?;
    }
    replace_durable(temp.path(), path)?;
    if let Some(seq) = sequence {
        check(if seq {
            FaultPoint::AfterSequenceRename
        } else {
            FaultPoint::AfterSpoolRename
        })?;
    }
    Ok(())
}
pub(crate) fn replace_durable(from: &Path, to: &Path) -> Result<(), QueueError> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
        // 安全：两个缓冲区均以NUL结尾且在调用期间存活，标记要求同步持久改名。
        let ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                windows_sys::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING
                    | windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH,
            )
        };
        if ok == 0 {
            return Err(error(QueueErrorCode::StorageFailed));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        storage(std::fs::rename(from, to))?;
        storage(storage(File::open(to.parent().unwrap()))?.sync_all())?;
        Ok(())
    }
}
