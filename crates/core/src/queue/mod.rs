//! 持久入站：admission覆盖序号分配至提交；SMTP永远不在写事务内运行。
mod ingress;
mod maintenance;
mod runtime;
mod lease;
mod spool;
mod sqlite;
use crate::{clock::Clock, config::ConfigV1, contracts::*, event::*};
use chrono::{DateTime, Utc};
pub use ingress::{
    AdmissionGate, AdmissionGuard, DirectorySecurity, SystemDirectorySecurity, reject_links,
};
pub use lease::{Lease, SenderGuard, allowed_transition};
pub use spool::{MAX_ITEM_BYTES, MAX_SPOOL_BYTES, SealedBody, SpoolAdmission};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
/// 受控错误没有路径、正文或底层数据库信息。
pub fn error(code: QueueErrorCode) -> QueueError {
    QueueError { code }
}
pub(crate) fn storage<T>(r: Result<T, impl std::fmt::Debug>) -> Result<T, QueueError> {
    r.map_err(|_| error(QueueErrorCode::StorageFailed))
}
/// 协作式deadline只约束当前调用还能启动的步骤；不声称能抢占任意系统IO。
pub(crate) fn check_deadline(deadline: Option<Instant>) -> Result<(), QueueError> {
    if deadline.is_some_and(|end| Instant::now() >= end) {
        Err(error(QueueErrorCode::AdmissionTimeout))
    } else {
        Ok(())
    }
}
/// 正文保护由任务05提供；当前核心没有默认密钥或明文后备。
/// seal必须使用唯一nonce的AEAD，认证aad，密文长度等于明文长度；nonce/tag固定12/16字节。
pub trait BodyProtector: Send + Sync {
    fn seal(&self, plaintext: &[u8], aad: &[u8]) -> Result<SealedBody, QueueError>;
    fn open(&self, sealed: &SealedBody, aad: &[u8]) -> Result<Vec<u8>, QueueError>;
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// 生命周期不能利用Unknown序号推测generation或跨事件取消；Gap之后同样降级。
pub enum OrderingEvidence {
    Ordered,
    OrderUnknown,
    SequenceGap,
    Late,
}
#[derive(Debug, Clone)]
/// 任务04的事务参与者返回初始通知状态；此结构不实现免打扰或看门狗策略。
pub struct NotificationPlan {
    pub status: QueueStatus,
    pub not_before: DateTime<Utc>,
}
/// 生命周期、策略和看门狗共用当前事务；不得另开连接或进行网络/文件IO。
/// 持久化自身活动状态后返回通知计划；错误将回滚本接收单元全部写入。
/// OrderingEvidence非Ordered时禁止推断generation、跨事件取消或回写已有身份。
pub trait TransactionParticipant: Send + Sync {
    /// spool重放的活动不是当前运行确认；默认保留既有参与者行为，Policy覆写为保守挂起。
    fn apply_replayed(
        &self,
        tx: &rusqlite::Transaction<'_>,
        envelope: &IngressEnvelopeV1,
        order: OrderingEvidence,
    ) -> Result<NotificationPlan, QueueError> {
        self.apply(tx, envelope, order)
    }

    /// 同事务发送复查，false时参与者负责持久化抑制或摘要决定。
    fn before_lease(
        &self,
        _tx: &rusqlite::Transaction<'_>,
        _id: uuid::Uuid,
        _now: DateTime<Utc>,
    ) -> Result<bool, QueueError> {
        Ok(true)
    }
    /// DATA前再次复查revision及免打扰，不重复消耗本次领取配额。
    fn before_send(
        &self,
        _tx: &rusqlite::Transaction<'_>,
        _id: uuid::Uuid,
        _now: DateTime<Utc>,
    ) -> Result<bool, QueueError> {
        Ok(true)
    }

    fn apply(
        &self,
        tx: &rusqlite::Transaction<'_>,
        envelope: &IngressEnvelopeV1,
        order: OrderingEvidence,
    ) -> Result<NotificationPlan, QueueError>;
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// 仅存储边界故障注入，生产默认NoFaults；不用其模拟平台实测。
pub enum FaultPoint {
    BeforeSequenceWrite,
    AfterSequenceWrite,
    AfterSequenceSync,
    BeforeSequenceRename,
    AfterSequenceRename,
    BeforeSpoolTemp,
    AfterSpoolWrite,
    AfterSpoolSync,
    BeforeSpoolRename,
    AfterSpoolRename,
    BeforeLifecycle,
    AfterLifecycle,
    AfterNotification,
    BeforeCheckpoint,
    BeforeCommit,
    BeforeImportCommit,
    AfterImportCommit,
}
pub trait FaultInjector: Send + Sync {
    fn check(&self, point: FaultPoint) -> Result<(), QueueError>;
}
pub struct NoFaults;
impl FaultInjector for NoFaults {
    fn check(&self, _: FaultPoint) -> Result<(), QueueError> {
        Ok(())
    }
}
/// 所有依赖显式注入，生产构造不会偷偷使用测试目录校验器或测试AEAD。
pub struct QueueDependencies {
    pub clock: Arc<dyn Clock + Send + Sync>,
    pub security: Arc<dyn DirectorySecurity>,
    pub protector: Option<Arc<dyn BodyProtector>>,
    pub participant: Arc<dyn TransactionParticipant>,
    pub faults: Arc<dyn FaultInjector>,
}
/// 每次操作短连接，SQLite唯一索引及文件锁可跨独立进程生效。
pub struct SqliteQueue {
    pub(crate) root: PathBuf,
    pub(crate) config: ConfigV1,
    pub(crate) deps: QueueDependencies,
    pub(crate) gate: AdmissionGate,
    pub(crate) import_scan: std::sync::Mutex<spool::ImportScan>,
    pub(crate) spool_admission: Option<Arc<dyn SpoolAdmission>>,
}
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub imported: usize,
    pub duplicates: usize,
    pub quarantined: usize,
    /// 当前快照待办提示；扫描未完成或其他导入者正在推进时保守至少为1。
    /// 零只表示本快照处理完，并非对并发新增spool的原子总量查询。
    pub remaining: usize,
}
impl SqliteQueue {
    /// root必须预先创建并通过平台安全验证；平台未接通时SystemDirectorySecurity拒绝全部路径。
    pub fn open(
        root: &Path,
        config: ConfigV1,
        deps: QueueDependencies,
    ) -> Result<Self, QueueError> {
        // 1. 身份和目录检查先于任何存储创建，所有子路径再执行不跟随链接校验。
        config
            .validate()
            .map_err(|_| error(QueueErrorCode::InvalidInput))?;
        deps.security.verify(root)?;
        reject_links(root)?;
        let root = storage(root.canonicalize())?;
        let gate = AdmissionGate::new(root.clone(), deps.security.clone());
        let _guard = gate.enter()?;
        for name in ["spool", "sequences"] {
            let path = root.join(name);
            reject_links(&path)?;
            storage(std::fs::create_dir_all(path))?;
        }
        for name in ["queue.sqlite", "queue.sqlite-wal", "queue.sqlite-shm"] {
            reject_links(&root.join(name))?;
        }
        // 2. schema/WAL初始化仅在安全admission下执行，不在Hook fallback期间重复迁移。
        let queue = Self {
            root,
            config,
            deps,
            gate,
            import_scan: std::sync::Mutex::new(spool::ImportScan::default()),
            spool_admission: None,
        };
        // 维护中的Hook只建立spool入口，不能打开/迁移正在恢复的业务数据库。
        if queue.maintenance()? { return Ok(queue); }
        let db = queue.connect(Duration::from_secs(1))?;
        let version: i64 = storage(db.pragma_query_value(None, "user_version", |row| row.get(0)))?;
        if version > 2 {
            return Err(error(QueueErrorCode::UnsupportedSchema));
        }
        // 只有首次初始化执行DDL/user_version写入；正常Hook重开已初始化WAL库只读schema证据。
        // PRAGMA user_version=1即使同值也需要写锁，不能在重开路径无条件执行。
        if version == 0 {
            storage(db.pragma_update(None, "journal_mode", "WAL"))?;
            storage(db.execute_batch(include_str!("../../migrations/001_queue.sql")))?;
        } else {
            let mode: String =
                storage(db.pragma_query_value(None, "journal_mode", |row| row.get(0)))?;
            if mode != "wal" {
                return Err(error(QueueErrorCode::StorageUnavailable));
            }
        }
        if version < 2 {
            storage(db.execute_batch(include_str!("../../migrations/002_activity_state.sql")))?;
        }
        Ok(queue)
    }
    /// 生产宿主附加准入验证后，队列只回放由该宿主认证的暂存。
    pub fn with_spool_admission(mut self, admission: Arc<dyn SpoolAdmission>) -> Self {
        self.spool_admission = Some(admission);
        self
    }
    pub fn database_path(&self) -> PathBuf {
        self.root.join("queue.sqlite")
    }
    pub fn spool_path(&self) -> PathBuf {
        self.root.join("spool")
    }
    pub(crate) fn connect(&self, timeout: Duration) -> Result<rusqlite::Connection, QueueError> {
        self.deps.security.verify(&self.root)?;
        reject_links(&self.root)?;
        for name in ["queue.sqlite", "queue.sqlite-wal", "queue.sqlite-shm"] {
            reject_links(&self.root.join(name))?;
        }
        let db = storage(rusqlite::Connection::open(self.database_path()))?;
        storage(db.busy_timeout(timeout))?;
        storage(db.pragma_update(None, "synchronous", "FULL"))?;
        storage(db.pragma_update(None, "foreign_keys", "ON"))?;
        Ok(db)
    }
    pub(crate) fn sanitize(
        &self,
        mut e: IngressEnvelopeV1,
    ) -> Result<IngressEnvelopeV1, QueueError> {
        e.validate()
            .map_err(|_| error(QueueErrorCode::InvalidInput))?;
        if let Some(admission) = &self.spool_admission {
            admission.check_source(&e.adapter_output)?;
        }
        if let Some(mut event) = e.adapter_output.notification.take() {
            // 抑制决定随envelope持久化；任何接收路径均不能为这类记录保存正文。
            if e.presence_suppressed {
                event.question = None;
                event.answer = None;
            }
            let event = crate::privacy::sanitize(event, &self.config)
                .map_err(|_| error(QueueErrorCode::BodyProtectionUnavailable))?
                .event;
            if (event.question.is_some() || event.answer.is_some()) && self.deps.protector.is_none()
            {
                return Err(error(QueueErrorCode::BodyProtectionUnavailable));
            }
            e.adapter_output.notification = Some(event);
        }
        Ok(e)
    }
}
pub(crate) fn instance(e: &IngressEnvelopeV1) -> uuid::Uuid {
    e.adapter_output
        .notification
        .as_ref()
        .map(|n| n.instance_id)
        .unwrap_or_else(|| {
            e.adapter_output
                .lifecycle
                .as_ref()
                .expect("validated envelope")
                .instance_id
        })
}

mod body_crypto;
pub use body_crypto::AeadBodyProtector;
