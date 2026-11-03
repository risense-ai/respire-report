//! 摘要只保留计数/标签；正文不复制，来源事实仍保存在原入站行。
use super::*;
#[derive(Debug, Clone, PartialEq, Eq)]
/// 供任务05邮件构建使用的内部读取结果；不属于外部事件协议。
pub struct DeliverySummary {
    pub count: u64,
    pub labels: Vec<String>,
}
/// 同成员只入桶一次；实际UTC静默区间+实例或UTC分钟+实例作为桶键。
pub(super) fn add_digest(
    tx: &Transaction<'_>,
    e: &IngressEnvelopeV1,
    key: &str,
    due: DateTime<Utc>,
) -> Result<(), QueueError> {
    let existing: Option<String> = storage(tx.query_row(
        "SELECT digest_key FROM policy_notifications WHERE id=?1",
        [e.ingress_id.to_string()],
        |r| r.get(0),
    ))?;
    if existing.is_some() {
        return Ok(());
    }
    let key = format!("{}:{}", key, crate::queue::instance(e));
    let labels: Option<String> = storage(
        tx.query_row(
            "SELECT labels FROM policy_digests WHERE digest_key=?1",
            [&key],
            |r| r.get(0),
        )
        .optional(),
    )?;
    let mut labels: Vec<String> = match labels {
        Some(s) => storage(serde_json::from_str(&s))?,
        None => Vec::new(),
    };
    if let Some(n) = &e.adapter_output.notification {
        for label in [&n.project_label, &n.session_label].into_iter().flatten() {
            if labels.len() < 8 && !labels.contains(label) {
                labels.push(label.clone());
            }
        }
    }
    storage(tx.execute("INSERT INTO policy_digests(digest_key,instance_id,due_at,count,labels) VALUES(?1,?2,?3,1,?4) ON CONFLICT(digest_key) DO UPDATE SET count=count+1,labels=excluded.labels",
params![key,
crate::queue::instance(e).to_string(),
due.timestamp_millis(),
storage(serde_json::to_string(&labels))?]))?;
    storage(tx.execute(
        "UPDATE policy_notifications SET digest_key=?2 WHERE id=?1",
        params![e.ingress_id.to_string(), key],
    ))?;
    Ok(())
}
impl Policy {
    /// 到期摘要直接写入同一队列；成员TTL/撤销状态复查后才计数，不复活原邮件。
    pub fn flush_digests(
        &self,
        queue: &SqliteQueue,
        instance: Uuid,
    ) -> Result<Option<usize>, QueueError> {
        let c = self.current_context();
        queue.with_instance_transaction(instance, |tx| self.flush_transaction(tx, instance, &c))
    }
    /// 任务05发送前读取；Some表示必须使用摘要计数/标签模板，不使用响应结束模板。
    pub fn delivery_summary(
        &self,
        queue: &SqliteQueue,
        id: Uuid,
    ) -> Result<Option<DeliverySummary>, QueueError> {
        let db = queue.connect(std::time::Duration::from_secs(1))?;
        let row: Option<(u64, String)> = storage(
            db.query_row(
                "SELECT count,labels FROM policy_digests WHERE notification_id=?1",
                [id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional(),
        )?;
        row.map(|(count, raw)| {
            Ok(DeliverySummary {
                count,
                labels: storage(serde_json::from_str(&raw))?,
            })
        })
        .transpose()
    }
}
/// 摘要发送前重新计算有效成员；已取消、过TTL或revision过期的告警不再计数。
pub(super) fn refresh(
    tx: &Transaction<'_>,
    id: Uuid,
    now: DateTime<Utc>,
) -> Result<bool, QueueError> {
    let key: String = storage(tx.query_row(
        "SELECT digest_key FROM policy_digests WHERE notification_id=?1",
        [id.to_string()],
        |r| r.get(0),
    ))?;
    let count:i64=storage(tx.query_row("SELECT COALESCE(SUM(p.aggregate_count),0) FROM notifications n JOIN policy_notifications p ON p.id=n.id WHERE p.digest_key=?1 AND p.kind!='digest' AND n.status='digest_pending' AND n.expires_at>?2 AND (p.kind!='stalled' OR EXISTS(SELECT 1 FROM activity_state a WHERE a.actor_key=p.actor_key AND a.turn_key=p.turn_key AND a.revision=p.activity_revision AND a.state='active'))",
params![key,
now.timestamp_millis()],
|r|r.get(0)))?;
    storage(tx.execute(
        "UPDATE policy_digests SET count=?2 WHERE digest_key=?1",
        params![key, count],
    ))?;
    storage(tx.execute(
        "UPDATE policy_notifications SET aggregate_count=?2 WHERE id=?1",
        params![id.to_string(), count],
    ))?;
    Ok(count > 0)
}

impl Policy {
    /// 只使用调用方持有的IMMEDIATE事务完成持久评估；不进行网络/文件IO。
    fn flush_transaction(
        &self,
        tx: &Transaction<'_>,
        instance: Uuid,
        c: &Context,
    ) -> Result<usize, QueueError> {
        let keys = {
            let mut stmt=storage(tx.prepare("SELECT digest_key FROM policy_digests WHERE instance_id=?1 AND due_at<=?2 AND notification_id IS NULL"))?;
            let rows = storage(stmt.query_map(
                params![instance.to_string(), c.snapshot.utc.timestamp_millis()],
                |r| r.get::<_, String>(0),
            ))?;
            storage(rows.collect::<Result<Vec<_>, _>>())?
        };
        let mut emitted = 0;
        for key in keys {
            // 1. 尚未发送且未过TTL的成员才纳入摘要；取消或到期不补发。
            let (count,
expiry):(i64,
Option<i64>)=storage(tx.query_row("SELECT COALESCE(SUM(p.aggregate_count),0),MIN(n.expires_at) FROM notifications n JOIN policy_notifications p ON p.id=n.id WHERE p.digest_key=?1 AND n.status='digest_pending' AND n.expires_at>?2",
params![key,
c.snapshot.utc.timestamp_millis()],
|r|Ok((r.get(0)?,
r.get(1)?))))?;
            let id = stable_id(&format!("digest:{key}"));
            storage(tx.execute(
                "UPDATE policy_digests SET notification_id=?2,count=?3 WHERE digest_key=?1",
                params![key, id.to_string(), count],
            ))?;
            if count == 0 {
                continue;
            }
            // 2. 摘要信封只用于共同队列身份；邮件必须使用delivery_summary而非单事件完成文案。
            let event = NormalizedEventV1 {
                schema_version: 1,
                adapter_id: "core-digest".into(),
                instance_id: instance,
                event_id: Some(id.to_string()),
                session_id: None,
                turn_id: None,
                parent_session_id: None,
                agent_kind: AgentKind::Unknown,
                actor_id: None,
                actor_scope: ActorScope::Unknown,
                kind: EventKind::ResponseFinished,
                completion_basis: CompletionBasis::ResponseStop,
                source_time: None,
                received_at: c.snapshot.utc,
                project_label: None,
                session_label: None,
                question: None,
                answer: None,
                background_state: BackgroundState::Unknown,
                resume_url: None,
                content_key_available: false,
                watchdog_detail: None,
            };
            let e = IngressEnvelopeV1 {
                presence_suppressed: false,
                schema_version: 1,
                ingress_id: id,
                received_at: c.snapshot.utc,
                per_instance_sequence: None,
                adapter_output: AdapterOutput {
                    notification: Some(event),
                    lifecycle: None,
                    source_evidence: SourceEvidence {
                        source_class: SourceClass::Unknown,
                        origin_evidence: OriginEvidence::Unknown,
                        background_result: BackgroundResult::Unknown,
                        classification_reason: "core_digest_counts_only_v1".into(),
                    },
                },
            };
            insert_generated(tx, &e)?;
            storage(tx.execute("INSERT INTO policy_notifications(id,kind,reason,aggregate_count,digest_key) VALUES(?1,'digest','queued',?2,?3)",
params![id.to_string(),
count,
key]))?;
            insert_notification(
                tx,
                id,
                NotificationPlan {
                    status: QueueStatus::Queued,
                    not_before: c.snapshot.utc,
                },
                &c.config,
                c.snapshot.utc,
            )?;
            // 摘要TTL不得延长成员原接收期限；成员保留digest_pending以允许发送前按有效状态复核。
            storage(tx.execute(
                "UPDATE notifications SET expires_at=MIN(expires_at,?2) WHERE id=?1",
                params![id.to_string(), expiry],
            ))?;
            emitted += 1;
        }
        Ok(emitted)
    }
}
