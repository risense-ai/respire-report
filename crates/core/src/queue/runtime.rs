//! 宿主调度查询和离线接收；不依赖平台或客户端。
use super::*;

/// ACK 超时重试可能使用不同本机时间或正文开关，只比较原始事件归属与标识。
fn same_spooled_event(left: &IngressEnvelopeV1, right: &IngressEnvelopeV1) -> bool {
    fn identity(output: &AdapterOutput) -> AdapterOutput {
        let mut output = output.clone();
        if let Some(event) = &mut output.notification {
            event.received_at = DateTime::UNIX_EPOCH;
            event.question = None;
            event.answer = None;
            event.content_key_available = false;
        }
        if let Some(signal) = &mut output.lifecycle {
            signal.received_at = DateTime::UNIX_EPOCH;
        }
        output
    }
    left.ingress_id == right.ingress_id
        && instance(left) == instance(right)
        && identity(&left.adapter_output) == identity(&right.adapter_output)
}
impl SqliteQueue {
    /// 离线客户端只写原子spool，由常驻Policy保守重放。
    pub fn accept_offline(&self, output: AdapterOutput) -> Result<EnqueueOutcome, QueueError> {
        self.accept_identified(output, uuid::Uuid::new_v4(), true)
    }
    /// IPC与超时fallback共用ID，ack丢失时仍按同一接收单元去重。
    pub fn accept_identified(
        &self,
        output: AdapterOutput,
        id: uuid::Uuid,
        offline: bool,
    ) -> Result<EnqueueOutcome, QueueError> {
        self.accept_identified_with_presence(output, id, offline, false)
    }
    /// 本机接收门控先于IPC和spool计算；相同ID重试必须携带同一抑制决定。
    pub fn accept_identified_with_presence(
        &self,
        mut output: AdapterOutput,
        id: uuid::Uuid,
        offline: bool,
        presence_suppressed: bool,
    ) -> Result<EnqueueOutcome, QueueError> {
        let now = self.deps.clock.now();
        if let Some(event) = &mut output.notification {
            event.received_at = now;
        }
        if let Some(event) = &mut output.lifecycle {
            event.received_at = now;
        }
        let mut e = self.sanitize(IngressEnvelopeV1 {
            presence_suppressed,
            schema_version: 1,
            ingress_id: id,
            received_at: now,
            per_instance_sequence: None,
            adapter_output: output,
        })?;
        let _guard = self.gate.enter()?;
        // ACK丢失后同ID重试不能再分配序号，否则会制造永远无法填补的checkpoint缺口。
        if !self.maintenance()? {
        if let Ok(db) = self.connect(Duration::from_millis(100)) {
            let exists: bool = storage(db.query_row(
                "SELECT EXISTS(SELECT 1 FROM ingress WHERE id=?1)",
                [id.to_string()],
                |row| row.get(0),
            ))?;
            if exists {
                return Ok(EnqueueOutcome::Duplicate { id });
            }
        }
        }
        let existing_path = self.spool_path().join(format!("{id}.spool"));
        if storage(existing_path.try_exists())? {
            // 可预测入站 ID 的文件只有通过认证且身份匹配，才算已经耐久落盘。
            let existing = self.inspect_spool(&existing_path)?;
            if !same_spooled_event(&existing, &e) {
                return Err(error(QueueErrorCode::InvalidInput));
            }
            return Ok(EnqueueOutcome::Spooled { id });
        }
        e.per_instance_sequence = self.next_sequence(instance(&e)).ok();
        if e.per_instance_sequence.is_none() {
            self.mark_order_unknown(instance(&e))?;
        }
        if offline {
            self.write_spool(&e)
        } else {
            self.persist_locked(e, false)
        }
    }
    /// 已持久化实例清单；不扫描客户端数据库。
    pub fn instances(&self) -> Result<Vec<uuid::Uuid>, QueueError> {
        let db = self.connect(Duration::from_secs(1))?;
        let mut stmt = storage(db.prepare("SELECT DISTINCT instance_id FROM ingress"))?;
        let rows = storage(stmt.query_map([], |r| r.get::<_, String>(0)))?;
        storage(rows.collect::<Result<Vec<_>, _>>())?
            .into_iter()
            .map(|s| storage(uuid::Uuid::parse_str(&s)))
            .collect()
    }
    /// 健康白名单计数，不导出正文或地址。
    pub fn counts(&self) -> Result<QueueCounts, QueueError> {
        let db = self.connect(Duration::from_secs(1))?;
        let mut stmt =
            storage(db.prepare("SELECT status,count(*) FROM notifications GROUP BY status"))?;
        let rows =
            storage(stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?))))?;
        let mut c = QueueCounts::default();
        for row in rows {
            let (state, n) = storage(row)?;
            match state.as_str() {
                "queued" => c.queued = n,
                "leased" => c.leased = n,
                "sent" => c.sent = n,
                "suppressed" => c.suppressed = n,
                "digest_pending" => c.digest_pending = n,
                "retry_wait" => c.retry_wait = n,
                "auth_blocked" => c.auth_blocked = n,
                "delivery_unknown" => c.delivery_unknown = n,
                "expired" => c.expired = n,
                "permanent_failed" => c.permanent_failed = n,
                _ => return Err(error(QueueErrorCode::StorageFailed)),
            }
        }
        Ok(c)
    }
}
