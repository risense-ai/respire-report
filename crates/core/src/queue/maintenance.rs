//! 升级专用维护边界；调用者停发并持有sender锁后才允许一致快照/恢复。
use super::*;
use rusqlite::{Connection, OpenFlags};

impl SqliteQueue {
    /// 发送器已停止后收敛所有租约；DATA未知不重发，accepted保留为sent。
    pub fn settle_stopped_sender(&self, _sender: &SenderGuard) -> Result<(), QueueError> {
        let _admission = self.gate.enter()?;
        if !self.maintenance()? { return Err(error(QueueErrorCode::StorageUnavailable)); }
        let db = self.connect(Duration::from_secs(1))?;
        storage(db.execute("UPDATE notifications SET status=CASE WHEN send_stage='accepted' THEN 'sent' WHEN send_stage='data_started' THEN 'delivery_unknown' ELSE 'queued' END,body=CASE WHEN send_stage IN('accepted','data_started') THEN NULL ELSE body END,lease_until=NULL WHERE status='leased'", []))?;
        Ok(())
    }
    /// SQLite在线backup API复制一致DB；快照路径须位于安全根下且不可覆盖。
    pub fn backup_for_update(&self, destination: &Path, _sender: &SenderGuard) -> Result<(), QueueError> {
        let _admission = self.gate.enter()?;
        self.check_snapshot_path(destination)?;
        if !self.maintenance()? || destination.exists() { return Err(error(QueueErrorCode::StorageUnavailable)); }
        let source = self.connect(Duration::from_secs(1))?;
        let mut target = storage(Connection::open(destination))?;
        let backup = storage(rusqlite::backup::Backup::new(&source, &mut target))?;
        storage(backup.run_to_completion(128, Duration::from_millis(1), None))?;
        drop(backup);
        drop(target);
        storage(storage(std::fs::File::open(destination))?.sync_all())
    }
    /// 仅升级器在尚未恢复消费时调用；不复制/删除spool和独立序号，WAL由backup API协调。
    pub fn restore_update_snapshot(&self, snapshot: &Path, _sender: &SenderGuard) -> Result<(), QueueError> {
        let _admission = self.gate.enter()?;
        self.check_snapshot_path(snapshot)?;
        if !self.maintenance()? { return Err(error(QueueErrorCode::StorageUnavailable)); }
        let source = storage(Connection::open_with_flags(snapshot, OpenFlags::SQLITE_OPEN_READ_ONLY))?;
        let version: u32 = storage(source.pragma_query_value(None, "user_version", |r| r.get(0)))?;
        if version != 2 { return Err(error(QueueErrorCode::UnsupportedSchema)); }
        let mut target = self.connect(Duration::from_secs(1))?;
        let backup = storage(rusqlite::backup::Backup::new(&source, &mut target))?;
        storage(backup.run_to_completion(128, Duration::from_millis(1), None))?;
        Ok(())
    }
    fn check_snapshot_path(&self, path: &Path) -> Result<(), QueueError> {
        if !path.starts_with(&self.root) || path.components().any(|c| matches!(c, std::path::Component::ParentDir)) { return Err(error(QueueErrorCode::StorageUnavailable)); }
        reject_links(path)
    }
    /// 在admission锁内重放全部已接受spool。提交日志前保留文件，失败可恢复快照而不丢入站。
    /// commit必须持久记录“不得再恢复旧快照”；先于删除spool和开放消费，崩溃只能向前恢复。
    pub fn finish_update_maintenance(&self, _sender: &SenderGuard, commit: impl FnOnce() -> Result<(), QueueError>) -> Result<(), QueueError> {
        let _admission = self.gate.enter()?;
        if !self.maintenance()? { return Err(error(QueueErrorCode::StorageUnavailable)); }
        // 1. 维护期间入口都只写spool；持锁后获得完整序列，沿用导入器的实例/序号排序。
        let mut pending = Vec::new();
        for entry in storage(std::fs::read_dir(self.spool_path()))? {
            let path = storage(entry)?.path();
            if path.extension().is_some_and(|e| e == "spool") {
                // 已撤销或无来源证明的记录保留原件并隔离，不能从升级恢复绕过准入。
                match self.inspect_spool_for_import(&path) {
                    Ok(e) => pending.push((path, e)),
                    Err(e) if self.spool_admission.is_some() && e.code == QueueErrorCode::InvalidInput => {
                        ingress::replace_durable(&path, &path.with_extension("quarantine"))?;
                    }
                    // 不兼容或暂时无法读取凭据时保留现场，拒绝解除维护。
                    Err(e) => return Err(e),
                }
            }
        }
        pending.sort_by_key(|(_, e)| (instance(e), e.per_instance_sequence.is_some(), e.per_instance_sequence.unwrap_or(0), e.ingress_id));
        for (_, e) in &pending { self.ingest_transaction(e, Duration::from_secs(1), true, None)?; }
        // 2. 持久化不可回退边界；即使下次删除/解锁失败，也只能去重导入后向前恢复。
        commit()?;
        for (path, _) in pending { storage(std::fs::remove_file(path))?; }
        let bytes = storage(serde_json::to_vec(&(false, uuid::Uuid::new_v4())))?;
        ingress::atomic_write(&self.root.join("maintenance"), &bytes, |_| Ok(()), None)?;
        *self.import_scan.lock().map_err(|_| error(QueueErrorCode::StorageFailed))? = spool::ImportScan::default();
        Ok(())
    }
}
