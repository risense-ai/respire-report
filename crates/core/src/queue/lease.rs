//! 发送状态机：DATA前记录阶段，租约过期后仅Prepared/Connecting/Envelope可以自动恢复。
use super::sqlite::{discards_body, status_name};
use super::*;
use rusqlite::{TransactionBehavior, params};
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub id: uuid::Uuid,
    pub expires_at: DateTime<Utc>,
    pub send_stage: SendStage,
    pub(crate) token: uuid::Uuid,
}
/// 十状态的显式允许矩阵；终态仅DeliveryUnknown可由用户显式重试，默认发送器不会领取它。
pub fn allowed_transition(from: QueueStatus, to: QueueStatus) -> bool {
    use QueueStatus::*;
    matches!(
        (from, to),
        (
            Queued,
            Leased | Suppressed | DigestPending | Expired | PermanentFailed
        ) | (
            Leased,
            Sent | RetryWait | AuthBlocked | DeliveryUnknown | Expired | PermanentFailed
        ) | (DigestPending, Queued | Suppressed | Expired)
            | (RetryWait, Leased | Suppressed | Expired | PermanentFailed)
            | (AuthBlocked, Queued | Expired)
            | (DeliveryUnknown, Queued)
    )
}
impl SqliteQueue {
    /// 原子领取到期项，最多60秒；只有已提交入站可见，不在本方法中调用SMTP。
    pub fn lease_due(&self, now: DateTime<Utc>, limit: usize) -> Result<Vec<Lease>, QueueError> {
        let _guard = self.gate.enter()?;
        if self.maintenance()? {
            return Ok(Vec::new());
        }
        let mut db = self.connect(Duration::from_secs(1))?;
        let tx = storage(db.transaction_with_behavior(TransactionBehavior::Immediate))?;
        // 1. TTL优先于认证修复/恢复；终态清正文而不声称安全擦盘。
        storage(tx.execute("UPDATE notifications SET status='expired',body=NULL,lease_until=NULL WHERE expires_at<=?1 AND status IN('queued','digest_pending','retry_wait','auth_blocked')",[now.timestamp_millis()]))?;
        let rows = {
            let mut stmt=storage(tx.prepare("SELECT n.id,i.instance_id FROM notifications n JOIN ingress i ON i.id=n.id WHERE n.status IN('queued','retry_wait') AND n.due_at<=?1 AND n.expires_at>?1 ORDER BY n.due_at,i.rowid LIMIT ?2"))?;
            let values = storage(stmt.query_map(
                params![now.timestamp_millis(), limit.min(100) as i64],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            ))?;
            storage(values.collect::<Result<Vec<_>, _>>())?
        };
        let expires = now + chrono::Duration::seconds(60);
        let mut leases = Vec::new();
        // 2. 同实例有spool时先重放，不能让看门狗/通知发送越过已接收的结束事件。
        for (id, inst) in rows {
            if self.has_backlog(storage(uuid::Uuid::parse_str(&inst))?)? {
                continue;
            }
            if !self.deps.participant.before_lease(
                &tx,
                storage(uuid::Uuid::parse_str(&id))?,
                now,
            )? {
                continue;
            }
            let token = uuid::Uuid::new_v4();
            storage(tx.execute("UPDATE notifications SET status='leased',lease_until=?2,lease_token=?3,send_stage='prepared',attempts=attempts+1 WHERE id=?1",params![id,expires.timestamp_millis(),token.to_string()]))?;
            leases.push(Lease {
                id: storage(uuid::Uuid::parse_str(&id))?,
                expires_at: expires,
                send_stage: SendStage::Prepared,
                token,
            });
        }
        storage(tx.commit())?;
        Ok(leases)
    }
    /// CAS转换不接受非法边；过TTL时强制Expired。DeliveryUnknown->Queued必须由显式用户动作调用。
    pub fn transition(
        &self,
        id: uuid::Uuid,
        expected: QueueStatus,
        next: QueueStatus,
    ) -> Result<(), QueueError> {
        if !allowed_transition(expected, next) {
            return Err(error(QueueErrorCode::InvalidInput));
        }
        let mut db = self.connect(Duration::from_secs(1))?;
        let tx = storage(db.transaction_with_behavior(TransactionBehavior::Immediate))?;
        let (expiry, stage): (i64, String) = storage(tx.query_row(
            "SELECT expires_at,send_stage FROM notifications WHERE id=?1 AND status=?2",
            params![id.to_string(), status_name(expected)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ))?;
        // 通用状态入口也遵守DATA安全边界，不能绕过带token发送接口重新进入自动重试。
        let next = if expected == QueueStatus::Leased {
            safe_completion(&stage, next, expiry, self.deps.clock.now())?
        } else if expiry <= self.deps.clock.now().timestamp_millis() {
            QueueStatus::Expired
        } else {
            next
        };
        // 逻辑状态转换进入Leased也必须生成可恢复的租约，不能留下永久悬挂状态。
        let leasing = next == QueueStatus::Leased;
        let lease_until = if leasing {
            Some((self.deps.clock.now() + chrono::Duration::seconds(60)).timestamp_millis())
        } else {
            None
        };
        let token = if leasing {
            Some(uuid::Uuid::new_v4().to_string())
        } else {
            None
        };
        storage(tx.execute("UPDATE notifications SET status=?2,body=CASE WHEN ?3 THEN NULL ELSE body END,lease_until=?4,lease_token=?5,send_stage=CASE WHEN ?6 THEN 'prepared' ELSE send_stage END,attempts=attempts+CASE WHEN ?6 THEN 1 ELSE 0 END WHERE id=?1",params![id.to_string(),status_name(next),discards_body(next),lease_until,token,leasing]))?;
        storage(tx.commit())?;
        Ok(())
    }
    pub fn notification_status(&self, id: uuid::Uuid) -> Result<QueueStatus, QueueError> {
        let db = self.connect(Duration::from_secs(1))?;
        let text: String = storage(db.query_row(
            "SELECT status FROM notifications WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        ))?;
        storage(serde_json::from_value(serde_json::Value::String(text)))
    }
    /// 必须在DATA网络写入之前调用并成功返回；阶段只前进，CAS避免并发覆盖。
    pub fn record_send_stage(
        &self,
        lease: &Lease,
        expected: SendStage,
        next: SendStage,
    ) -> Result<(), QueueError> {
        let rank = |s| match s {
            SendStage::Prepared => 0,
            SendStage::Connecting => 1,
            SendStage::Envelope => 2,
            SendStage::DataStarted => 3,
            SendStage::Accepted => 4,
        };
        if rank(next) <= rank(expected)
            || (next == SendStage::Accepted && expected != SendStage::DataStarted)
        {
            return Err(error(QueueErrorCode::InvalidInput));
        }
        let _guard = self.gate.enter()?;
        let mut db = self.connect(Duration::from_secs(1))?;
        let tx = storage(db.transaction_with_behavior(TransactionBehavior::Immediate))?;
        let valid: bool = storage(tx.query_row("SELECT EXISTS(SELECT 1 FROM notifications WHERE id=?1 AND status='leased' AND lease_token=?2 AND lease_until>?3 AND send_stage=?4)",params![lease.id.to_string(),lease.token.to_string(),self.deps.clock.now().timestamp_millis(),stage_name(expected)],|r|r.get(0)))?;
        if !valid {
            return Err(error(QueueErrorCode::InvalidInput));
        }
        if next == SendStage::DataStarted {
            let inst: String = storage(tx.query_row(
                "SELECT instance_id FROM ingress WHERE id=?1",
                [lease.id.to_string()],
                |r| r.get(0),
            ))?;
            if self.maintenance()? || self.has_backlog(storage(uuid::Uuid::parse_str(&inst))?)? {
                return Err(error(QueueErrorCode::StorageUnavailable));
            }
            if !self
                .deps
                .participant
                .before_send(&tx, lease.id, self.deps.clock.now())?
            {
                storage(tx.commit())?;
                return Err(error(QueueErrorCode::InvalidInput));
            }
        }
        let changed=storage(tx.execute("UPDATE notifications SET send_stage=?3 WHERE id=?1 AND status='leased' AND send_stage=?2 AND lease_until>?4 AND lease_token=?5",params![lease.id.to_string(),stage_name(expected),stage_name(next),self.deps.clock.now().timestamp_millis(),lease.token.to_string()]))?;
        if changed != 1 {
            return Err(error(QueueErrorCode::InvalidInput));
        }
        storage(tx.commit())?;
        Ok(())
    }
    pub fn recover_leases(&self, now: DateTime<Utc>) -> Result<usize, QueueError> {
        let mut db = self.connect(Duration::from_secs(1))?;
        let tx = storage(db.transaction_with_behavior(TransactionBehavior::Immediate))?;
        // 未确认DATA绝不自动重发；已记录SMTP接受则恢复Sent，不表示收件箱已投递。
        let count=storage(tx.execute("UPDATE notifications SET status=CASE WHEN send_stage='accepted' THEN 'sent' WHEN send_stage='data_started' THEN 'delivery_unknown' WHEN expires_at<=?1 THEN 'expired' ELSE 'queued' END,body=CASE WHEN send_stage IN('accepted','data_started') OR expires_at<=?1 THEN NULL ELSE body END,lease_until=NULL WHERE status='leased' AND lease_until<=?1",[now.timestamp_millis()]))?;
        storage(tx.commit())?;
        Ok(count)
    }
}
fn stage_name(stage: SendStage) -> &'static str {
    match stage {
        SendStage::Prepared => "prepared",
        SendStage::Connecting => "connecting",
        SendStage::Envelope => "envelope",
        SendStage::DataStarted => "data_started",
        SendStage::Accepted => "accepted",
    }
}
/// 后台同用户单发送器OS锁；持有到所有在途SMTP阶段落盘为止。
pub struct SenderGuard(std::fs::File);
impl Drop for SenderGuard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
impl SqliteQueue {
    pub fn acquire_sender(&self) -> Result<SenderGuard, QueueError> {
        self.deps.security.verify(&self.root)?;
        let path = self.root.join("sender.lock");
        reject_links(&path)?;
        let file = storage(
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(path),
        )?;
        file.try_lock()
            .map_err(|_| error(QueueErrorCode::StorageUnavailable))?;
        Ok(SenderGuard(file))
    }
    /// 发送者只使用带租约token的接口，旧worker不得修改后来重新领取的事件。
    pub fn complete_lease(&self, lease: &Lease, next: QueueStatus) -> Result<(), QueueError> {
        if !allowed_transition(QueueStatus::Leased, next) {
            return Err(error(QueueErrorCode::InvalidInput));
        }
        let mut db = self.connect(Duration::from_secs(1))?;
        let tx = storage(db.transaction_with_behavior(TransactionBehavior::Immediate))?;
        let (stage,expiry):(String,i64)=storage(tx.query_row("SELECT send_stage,expires_at FROM notifications WHERE id=?1 AND status='leased' AND lease_token=?2 AND lease_until>?3",params![lease.id.to_string(),lease.token.to_string(),self.deps.clock.now().timestamp_millis()],|r|Ok((r.get(0)?,r.get(1)?))))?;
        let next = safe_completion(&stage, next, expiry, self.deps.clock.now())?;
        storage(tx.execute("UPDATE notifications SET status=?2,body=CASE WHEN ?3 THEN NULL ELSE body END,lease_until=NULL,lease_token=NULL WHERE id=?1",params![lease.id.to_string(),status_name(next),discards_body(next)]))?;
        storage(tx.commit())?;
        Ok(())
    }
    /// 明确未发送的临时失败初次+4次；jitter_percent由调用方随机源提供，范围0..=20。
    /// DATA后失败强制DeliveryUnknown，Accepted只能进入Sent，不受临时重试请求影响。
    pub fn retry_temporary(
        &self,
        lease: &Lease,
        now: DateTime<Utc>,
        jitter_percent: u8,
    ) -> Result<(), QueueError> {
        if jitter_percent > 20 {
            return Err(error(QueueErrorCode::InvalidInput));
        }
        let mut db = self.connect(Duration::from_secs(1))?;
        let tx = storage(db.transaction_with_behavior(TransactionBehavior::Immediate))?;
        let (stage,attempts,expiry):(String,usize,i64)=storage(tx.query_row("SELECT send_stage,attempts,expires_at FROM notifications WHERE id=?1 AND status='leased' AND lease_token=?2 AND lease_until>?3",params![lease.id.to_string(),lease.token.to_string(),now.timestamp_millis()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))))?;
        let requested = if attempts >= 5 {
            QueueStatus::PermanentFailed
        } else {
            QueueStatus::RetryWait
        };
        let next = safe_completion(&stage, requested, expiry, now)?;
        let delay = [30i64, 120, 600, 1800]
            .get(attempts.saturating_sub(1))
            .copied()
            .unwrap_or(0)
            * 10
            * (100 + i64::from(jitter_percent));
        storage(tx.execute("UPDATE notifications SET status=?2,due_at=?3,lease_until=NULL,lease_token=NULL,body=CASE WHEN ?4 THEN NULL ELSE body END WHERE id=?1",params![lease.id.to_string(),status_name(next),now.timestamp_millis()+delay,discards_body(next)]))?;
        storage(tx.commit())?;
        Ok(())
    }
}
fn safe_completion(
    stage: &str,
    next: QueueStatus,
    expiry: i64,
    now: DateTime<Utc>,
) -> Result<QueueStatus, QueueError> {
    // SMTP已接受的证据优先；DATA后的不确定结果不能借错误分类重新进入自动发送。
    if stage == "accepted" {
        return Ok(QueueStatus::Sent);
    }
    if stage == "data_started" {
        return Ok(QueueStatus::DeliveryUnknown);
    }
    if next == QueueStatus::Sent {
        return Err(error(QueueErrorCode::InvalidInput));
    }
    if expiry <= now.timestamp_millis() {
        return Ok(QueueStatus::Expired);
    }
    Ok(next)
}

impl SqliteQueue {
    /// 发信前重新验证普通记录与摘要成员的来源，兼容旧库中尚未投递的通用事件。
    /// 返回 false 代表未授权，不读取或解密正文。摘要混入未授权成员时整封不发送。
    pub(crate) fn lease_source_authorized(&self, lease: &Lease) -> Result<bool, QueueError> {
        let Some(admission) = &self.spool_admission else { return Ok(true) };
        let db = self.connect(Duration::from_secs(1))?;
        let meta: Vec<u8> = storage(db.query_row(
            "SELECT i.metadata FROM notifications n JOIN ingress i ON i.id=n.id WHERE n.id=?1 AND n.status='leased' AND n.lease_token=?2 AND n.lease_until>?3",
            params![lease.id.to_string(), lease.token.to_string(), self.deps.clock.now().timestamp_millis()],
            |r| r.get(0),
        ))?;
        let authorize = |raw: &[u8]| {
            let envelope: IngressEnvelopeV1 = storage(serde_json::from_slice(raw))?;
            match admission.check_source(&envelope.adapter_output) {
                Ok(()) => Ok(true),
                Err(e) if e.code == QueueErrorCode::InvalidInput => Ok(false),
                Err(e) => Err(e),
            }
        };
        if !authorize(&meta)? {
            return Ok(false);
        }
        // 摘要的共同事件是内部合成的，来源授权必须读取仍参与摘要的原始成员。
        let mut members = storage(db.prepare(
            "SELECT i.metadata FROM policy_digests d JOIN policy_notifications p ON p.digest_key=d.digest_key JOIN notifications n ON n.id=p.id JOIN ingress i ON i.id=n.id WHERE d.notification_id=?1 AND n.status='digest_pending' AND n.expires_at>?2",
        ))?;
        let members = storage(members.query_map(
            params![lease.id.to_string(), self.deps.clock.now().timestamp_millis()],
            |r| r.get::<_, Vec<u8>>(0),
        ))?;
        for member in members {
            if !authorize(&storage(member)?)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// 仅持当前有效token的发送者读取暂存正文；终态不允许通过此入口重新解密。
    pub fn leased_mail_event(&self, lease: &Lease) -> Result<(NormalizedEventV1, u64), QueueError> {
        let db = self.connect(Duration::from_secs(1))?;
        let (meta,body,count):(Vec<u8>,Option<Vec<u8>>,u64)=storage(db.query_row("SELECT i.metadata,n.body,COALESCE(p.aggregate_count,1) FROM notifications n JOIN ingress i ON i.id=n.id LEFT JOIN policy_notifications p ON p.id=n.id WHERE n.id=?1 AND n.status='leased' AND n.lease_token=?2 AND n.lease_until>?3",params![lease.id.to_string(),lease.token.to_string(),self.deps.clock.now().timestamp_millis()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))))?;
        let envelope: IngressEnvelopeV1 = if let Some(bytes) = body {
            self.decode(&bytes)?
        } else {
            storage(serde_json::from_slice(&meta))?
        };
        if envelope.ingress_id != lease.id {
            return Err(error(QueueErrorCode::InvalidInput));
        }
        Ok((
            envelope
                .adapter_output
                .notification
                .ok_or_else(|| error(QueueErrorCode::InvalidInput))?,
            count,
        ))
    }
    /// 只接受SMTP协议解析器确认的完整4xx/5xx负响应；同事务保留DATA阶段证据并完成租约。
    /// 不能用超时、断开、半行或通用错误调用；崩溃在提交之前仍恢复为不确定。
    pub(crate) fn complete_smtp_rejection(
        &self,
        lease: &Lease,
        code: u16,
        jitter: u8,
    ) -> Result<(), QueueError> {
        if !(400..600).contains(&code) || jitter > 20 {
            return Err(error(QueueErrorCode::InvalidInput));
        }
        let now = self.deps.clock.now();
        let mut db = self.connect(Duration::from_secs(1))?;
        let tx = storage(db.transaction_with_behavior(TransactionBehavior::Immediate))?;
        let (attempts,expiry):(usize,i64)=storage(tx.query_row("SELECT attempts,expires_at FROM notifications WHERE id=?1 AND status='leased' AND lease_token=?2 AND lease_until>?3 AND send_stage='data_started'",params![lease.id.to_string(),lease.token.to_string(),now.timestamp_millis()],|r|Ok((r.get(0)?,r.get(1)?))))?;
        let status = if expiry <= now.timestamp_millis() {
            QueueStatus::Expired
        } else if code >= 500 || attempts >= 5 {
            QueueStatus::PermanentFailed
        } else {
            QueueStatus::RetryWait
        };
        let delay = [30i64, 120, 600, 1800]
            .get(attempts.saturating_sub(1))
            .copied()
            .unwrap_or(0)
            * 10
            * (100 + i64::from(jitter));
        storage(tx.execute("UPDATE notifications SET status=?2,due_at=?3,body=CASE WHEN ?4 THEN NULL ELSE body END,lease_until=NULL,lease_token=NULL WHERE id=?1",params![lease.id.to_string(),status_name(status),now.timestamp_millis()+delay,discards_body(status)]))?;
        storage(tx.commit())
    }
}

impl SqliteQueue {
    /// 宿主定时调用：24h上限按入队expires_at执行，元数据按配置保留天数有界清理。
    /// 返回本批删除的历史入站数（最多500），不是安全擦盘；保留checkpoint及仍活动的运行状态。
    pub fn maintain_retention(&self, now: DateTime<Utc>) -> Result<usize, QueueError> {
        let _guard = self.gate.enter()?;
        // 1. 过期租约先按持久SMTP阶段恢复，DATA未知不能被一般TTL清理伪装成未发送。
        self.recover_leases(now)?;
        let mut db = self.connect(Duration::from_secs(1))?;
        let tx = storage(db.transaction_with_behavior(TransactionBehavior::Immediate))?;
        storage(tx.execute("UPDATE notifications SET status='expired',body=NULL,lease_until=NULL,lease_token=NULL WHERE expires_at<=?1 AND (status IN('queued','digest_pending','retry_wait','auth_blocked') OR (status='leased' AND send_stage NOT IN('data_started','accepted')))",[now.timestamp_millis()]))?;
        storage(tx.execute(
            "UPDATE notifications SET body=NULL WHERE expires_at<=?1",
            [now.timestamp_millis()],
        ))?;
        // 2. 仅删除终态历史或纯生命周期历史；运行中的队列及其身份继续保留。
        let cutoff = now
            .checked_sub_signed(chrono::Duration::days(i64::from(
                self.config.metadata_retention_days,
            )))
            .ok_or_else(|| error(QueueErrorCode::InvalidInput))?
            .timestamp_millis();
        let ids = {
            let mut stmt=storage(tx.prepare("SELECT i.id FROM ingress i LEFT JOIN notifications n ON n.id=i.id WHERE i.received_at<?1 AND (n.id IS NULL OR n.status IN('sent','suppressed','delivery_unknown','expired','permanent_failed')) LIMIT 500"))?;
            let rows = storage(stmt.query_map([cutoff], |r| r.get::<_, String>(0)))?;
            storage(rows.collect::<Result<Vec<_>, _>>())?
        };
        for id in &ids {
            storage(tx.execute("DELETE FROM policy_notifications WHERE id=?1", [id]))?;
            storage(tx.execute("DELETE FROM policy_digests WHERE notification_id=?1", [id]))?;
            storage(tx.execute("DELETE FROM diagnostics WHERE ingress_id=?1", [id]))?;
            storage(tx.execute("DELETE FROM lifecycle WHERE ingress_id=?1", [id]))?;
            storage(tx.execute("DELETE FROM notifications WHERE id=?1", [id]))?;
            storage(tx.execute("DELETE FROM ingress WHERE id=?1", [id]))?;
        }
        storage(tx.execute(
            "DELETE FROM policy_dedupe WHERE expires_at<?1",
            [now.timestamp_millis()],
        ))?;
        storage(tx.execute("DELETE FROM policy_rate WHERE attempted_at<?1", [cutoff]))?;
        storage(tx.execute("DELETE FROM policy_digests WHERE due_at<?1 AND notification_id IS NULL AND NOT EXISTS(SELECT 1 FROM policy_notifications p WHERE p.digest_key=policy_digests.digest_key)",[cutoff]))?;
        storage(tx.commit())?;
        Ok(ids.len())
    }
}
