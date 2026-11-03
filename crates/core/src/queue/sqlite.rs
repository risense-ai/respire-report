//! 入站事务把不可变事实、业务参与者、通知和checkpoint一起提交。
use super::*;
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
impl SqliteQueue {
    pub(crate) fn ingest_transaction(
        &self,
        e: &IngressEnvelopeV1,
        timeout: Duration,
        importing: bool,
        deadline: Option<Instant>,
    ) -> Result<EnqueueOutcome, QueueError> {
        check_deadline(deadline)?;
        // 兼容已有应急spool：在任何生命周期事务之前先持久化降级，不能等处理完后来事件才补记。
        if e.per_instance_sequence.is_none() {
            self.mark_order_unknown(instance(e))?;
        }
        check_deadline(deadline)?;
        // 1. 加密在写事务之前完成，失败不能先落明文或部分生命周期。
        let encoded = self.encode(e)?;
        check_deadline(deadline)?;
        let mut metadata = e.clone();
        if let Some(n) = &mut metadata.adapter_output.notification {
            n.question = None;
            n.answer = None;
        }
        let metadata = storage(serde_json::to_vec(&metadata))?;
        let event_key = dedup_key(e)?;
        let timeout = deadline.map_or(timeout, |end| {
            timeout.min(end.saturating_duration_since(Instant::now()))
        });
        check_deadline(deadline)?;
        let mut db = self.connect(timeout)?;
        check_deadline(deadline)?;
        let tx = storage(db.transaction_with_behavior(TransactionBehavior::Immediate))?;
        check_deadline(deadline)?;
        let existing: Option<String> = storage(
            tx.query_row(
                "SELECT id FROM ingress WHERE id=?1 OR event_key=?2",
                params![e.ingress_id.to_string(), event_key],
                |r| r.get(0),
            )
            .optional(),
        )?;
        let (order, checkpoint) = checkpoint_evidence(&tx, e, self.order_degraded(instance(e))?)?;
        check_deadline(deadline)?;
        if let Some(existing) = existing {
            // 重复事件不重放生命周期，但已分配序号仍推进checkpoint，避免假缺口。
            if existing != e.ingress_id.to_string() {
                write_checkpoint(&tx, e, order, checkpoint)?;
            }
            check_deadline(deadline)?;
            storage(tx.commit())?;
            return Ok(EnqueueOutcome::Duplicate {
                id: storage(uuid::Uuid::parse_str(&existing))?,
            });
        }
        storage(tx.execute("INSERT INTO ingress(id,instance_id,sequence,event_key,received_at,metadata,order_known) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![e.ingress_id.to_string(),instance(e).to_string(),e.per_instance_sequence,event_key,e.received_at.timestamp_millis(),metadata,order==OrderingEvidence::Ordered]))?;
        // 2. 生命周期事实和任务04业务观察在同一事务；Unknown不能再推断generation。
        check_deadline(deadline)?;
        self.deps.faults.check(FaultPoint::BeforeLifecycle)?;
        if let Some(l) = &e.adapter_output.lifecycle {
            storage(tx.execute(
                "INSERT INTO lifecycle VALUES(?1,?2)",
                params![e.ingress_id.to_string(), storage(serde_json::to_vec(l))?],
            ))?;
        }
        check_deadline(deadline)?;
        let plan = if importing {
            self.deps.participant.apply_replayed(&tx, e, order)?
        } else {
            self.deps.participant.apply(&tx, e, order)?
        };
        self.deps.faults.check(FaultPoint::AfterLifecycle)?;
        check_deadline(deadline)?;
        if let Some(n) = &e.adapter_output.notification {
            if !matches!(
                plan.status,
                QueueStatus::Queued
                    | QueueStatus::Suppressed
                    | QueueStatus::DigestPending
                    | QueueStatus::RetryWait
            ) {
                return Err(error(QueueErrorCode::InvalidInput));
            }
            let expiry = e
                .received_at
                .checked_add_signed(chrono::Duration::hours(i64::from(
                    self.config.queue_ttl_hours.min(24),
                )))
                .ok_or_else(|| error(QueueErrorCode::InvalidInput))?;
            let status = if expiry <= self.deps.clock.now() {
                QueueStatus::Expired
            } else {
                plan.status
            };
            let body = if (n.question.is_some() || n.answer.is_some()) && !discards_body(status) {
                Some(encoded)
            } else {
                None
            };
            storage(tx.execute("INSERT INTO notifications(id,status,due_at,expires_at,body) VALUES(?1,?2,?3,?4,?5)",params![e.ingress_id.to_string(),status_name(status),plan.not_before.timestamp_millis(),expiry.timestamp_millis(),body]))?;
        }
        self.deps.faults.check(FaultPoint::AfterNotification)?;
        // 3. checkpoint与全部副作用同提交，断点回滚后可以按同一入站ID安全重放。
        self.deps.faults.check(FaultPoint::BeforeCheckpoint)?;
        write_checkpoint(&tx, e, order, checkpoint)?;
        self.deps.faults.check(FaultPoint::BeforeCommit)?;
        if importing {
            self.deps.faults.check(FaultPoint::BeforeImportCommit)?;
        }
        check_deadline(deadline)?;
        storage(tx.commit())?;
        Ok(EnqueueOutcome::Queued { id: e.ingress_id })
    }
}
fn dedup_key(e: &IngressEnvelopeV1) -> Result<String, QueueError> {
    // 可证事件ID按实例/适配器/会话/actor限定；未知actor不按正文或父会话猜测身份。
    if let Some(n) = &e.adapter_output.notification {
        if let Some(id) = &n.event_id {
            return storage(serde_json::to_string(&(
                n.instance_id,
                &n.adapter_id,
                &n.session_id,
                n.actor_scope,
                &n.actor_id,
                id,
            )));
        }
    }
    if let Some(l) = &e.adapter_output.lifecycle {
        if let Some(id) = &l.upstream_id {
            return storage(serde_json::to_string(&(
                l.instance_id,
                &l.session_id,
                l.actor_scope,
                &l.actor_id,
                l.kind,
                id,
            )));
        }
    }
    Ok(e.ingress_id.to_string())
}
fn checkpoint_evidence(
    tx: &Transaction<'_>,
    e: &IngressEnvelopeV1,
    degraded: bool,
) -> Result<(OrderingEvidence, u64), QueueError> {
    let cp: Option<(u64, bool)> = storage(
        tx.query_row(
            "SELECT sequence,ordered FROM checkpoints WHERE instance_id=?1",
            [instance(e).to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional(),
    )?;
    let (last, ordered) = cp.unwrap_or((0, true));
    let evidence = if degraded {
        OrderingEvidence::OrderUnknown
    } else {
        match e.per_instance_sequence {
            None => OrderingEvidence::OrderUnknown,
            Some(s) if s <= last => OrderingEvidence::Late,
            Some(s) if s != last + 1 => OrderingEvidence::SequenceGap,
            Some(_) if !ordered => OrderingEvidence::OrderUnknown,
            Some(_) => OrderingEvidence::Ordered,
        }
    };
    Ok((evidence, last))
}
fn write_checkpoint(
    tx: &Transaction<'_>,
    e: &IngressEnvelopeV1,
    order: OrderingEvidence,
    last: u64,
) -> Result<(), QueueError> {
    let seq = e.per_instance_sequence.unwrap_or(last).max(last);
    let ordered = order == OrderingEvidence::Ordered;
    storage(tx.execute("INSERT INTO checkpoints VALUES(?1,?2,?3) ON CONFLICT(instance_id) DO UPDATE SET sequence=MAX(sequence,excluded.sequence),ordered=MIN(ordered,excluded.ordered)",params![instance(e).to_string(),seq,ordered]))?;
    if !ordered {
        let code = match order {
            OrderingEvidence::SequenceGap => "sequence_gap",
            OrderingEvidence::Late => "late_sequence",
            _ => "order_unknown",
        };
        storage(tx.execute(
            "INSERT INTO diagnostics(ingress_id,code) VALUES(?1,?2)",
            params![e.ingress_id.to_string(), code],
        ))?;
    }
    Ok(())
}
pub(crate) fn status_name(status: QueueStatus) -> &'static str {
    match status {
        QueueStatus::Queued => "queued",
        QueueStatus::Leased => "leased",
        QueueStatus::Sent => "sent",
        QueueStatus::Suppressed => "suppressed",
        QueueStatus::DigestPending => "digest_pending",
        QueueStatus::RetryWait => "retry_wait",
        QueueStatus::AuthBlocked => "auth_blocked",
        QueueStatus::DeliveryUnknown => "delivery_unknown",
        QueueStatus::Expired => "expired",
        QueueStatus::PermanentFailed => "permanent_failed",
    }
}
pub(crate) fn terminal(status: QueueStatus) -> bool {
    matches!(
        status,
        QueueStatus::Sent
            | QueueStatus::Suppressed
            | QueueStatus::DeliveryUnknown
            | QueueStatus::Expired
            | QueueStatus::PermanentFailed
    )
}
impl SqliteQueue {
    /// 任务04到期评估接点：同实例存在更早spool或maintenance时返回None。
    /// 闭包仅操作同一事务，不执行网络/文件IO；Ok结果与全部活动/通知变更一起提交。
    pub fn with_instance_transaction<T>(
        &self,
        instance_id: uuid::Uuid,
        operation: impl FnOnce(&Transaction<'_>) -> Result<T, QueueError>,
    ) -> Result<Option<T>, QueueError> {
        let _guard = self.gate.enter()?;
        if self.maintenance()? || self.has_backlog(instance_id)? {
            return Ok(None);
        }
        let mut db = self.connect(Duration::from_secs(1))?;
        let tx = storage(db.transaction_with_behavior(TransactionBehavior::Immediate))?;
        // 无序持久标记可能早于spool/入站成功；到期判断也不能越过该降级证据。
        if self.order_degraded(instance_id)? {
            storage(tx.execute(
                "UPDATE checkpoints SET ordered=0 WHERE instance_id=?1",
                [instance_id.to_string()],
            ))?;
        }
        let result = operation(&tx)?;
        storage(tx.commit())?;
        Ok(Some(result))
    }
}

/// 摘要只保留计数/标签；它虽然不是队列终态，也必须立即丢弃正文密文。
pub(crate) fn discards_body(status: QueueStatus) -> bool {
    terminal(status) || status == QueueStatus::DigestPending
}
