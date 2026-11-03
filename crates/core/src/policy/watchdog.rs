//! 生命周期仅按可靠actor/轮次更新；SQLite状态是唯一事实源。
use super::*;
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LifecycleEffect {
    pub generation: Option<u64>,
    pub cancelled: usize,
    pub suspended: bool,
}
/// 门面复用同一Policy/SqliteQueue，不另建看门狗队列。
pub struct Watchdog<'a>(pub &'a Policy);
impl Watchdog<'_> {
    pub fn observe(
        &self,
        tx: &Transaction<'_>,
        e: &IngressEnvelopeV1,
        order: OrderingEvidence,
    ) -> Result<LifecycleEffect, QueueError> {
        self.0.observe_lifecycle(tx, e, order)
    }
    pub fn evaluate_due(
        &self,
        queue: &SqliteQueue,
        instance: Uuid,
    ) -> Result<Option<usize>, QueueError> {
        self.0.evaluate_due(queue, instance)
    }
}
impl Policy {
    /// 未知顺序只挂起，不推断generation或取消通知。
    pub fn observe_lifecycle(
        &self,
        tx: &Transaction<'_>,
        e: &IngressEnvelopeV1,
        order: OrderingEvidence,
    ) -> Result<LifecycleEffect, QueueError> {
        let c = self.current_context();
        let mut effect = LifecycleEffect::default();
        let instance = crate::queue::instance(e);
        if order != OrderingEvidence::Ordered {
            suspend_instance(tx, instance)?;
            effect.suspended = true;
            return Ok(effect);
        }
        let Some(s) = &e.adapter_output.lifecycle else {
            return Ok(effect);
        };
        // CallerReported和来源标签不能触发内部后台过滤。
        if verified_internal(&e.adapter_output.source_evidence) {
            return Ok(effect);
        }
        let cap = c.capabilities.get(&instance).cloned().unwrap_or_default();
        if !cap.reliable_actor {
            return Ok(effect);
        }
        if s.kind == LifecycleKind::SessionClosed {
            storage(tx.execute("UPDATE activity_state SET state='closed',revision=revision+1 WHERE instance_id=?1 AND session_id=?2 AND state!='closed'",
params![instance.to_string(),
s.session_id]))?;
            storage(tx.execute(
                "UPDATE actor_state SET running=0 WHERE instance_id=?1 AND session_id=?2",
                params![instance.to_string(), s.session_id],
            ))?;
            effect.cancelled = cancel_session(tx, instance, &s.session_id)?;
            return Ok(effect);
        }
        let Some(actor) = signal_actor(s)? else {
            return Ok(effect);
        };
        let old:Option<(u64,
String,
Option<String>,
bool)>=storage(tx.query_row("SELECT generation,current_turn,start_token,running FROM actor_state WHERE actor_key=?1",
[&actor],
|r|Ok((r.get(0)?,
r.get(1)?,
r.get(2)?,
r.get(3)?))).optional())?;
        match s.kind {
            LifecycleKind::Started if cap.reliable_started && cap.reliable_turn_or_generation => {
                let token = s.upstream_id.clone().or_else(|| s.turn_id.clone());
                // 历史可靠开始先持久去重，旧轮重入不得关闭当前活动或取消当前告警。
                if dedupe::record_start(tx, &actor, s)? {
                    effect.generation = old.as_ref().map(|row| row.0);
                    return Ok(effect);
                }
                // 无ID的连续Started无法证明新轮；同一已知轮次即使结束也不重入。
                if let Some((g, t, prior, running)) = &old {
                    let same_turn = s.turn_id.as_ref().is_some_and(|u| *t == format!("t:{u}"));
                    if same_turn
                        || token.as_ref().is_some_and(|id| Some(id) == prior.as_ref())
                        || (token.is_none() && *running)
                    {
                        effect.generation = Some(*g);
                        return Ok(effect);
                    }
                }
                let generation = old
                    .as_ref()
                    .map_or(0, |r| r.0)
                    .checked_add(1)
                    .filter(|g| *g <= crate::contracts::MAX_SAFE_INTEGER)
                    .ok_or_else(|| error(QueueErrorCode::InvalidInput))?;
                let turn = s
                    .turn_id
                    .as_ref()
                    .map(|t| format!("t:{t}"))
                    .unwrap_or_else(|| format!("g:{generation}"));
                // 新轮关闭同actor旧计时，只取消旧stalled，不触碰兄弟actor。
                storage(tx.execute("UPDATE activity_state SET state='closed',revision=revision+1 WHERE actor_key=?1 AND state!='closed'",
[&actor]))?;
                cancel_actor(tx, &actor, None, true)?;
                storage(tx.execute("INSERT INTO actor_state VALUES(?1,?2,?3,?4,?5,?6,1) ON CONFLICT(actor_key) DO UPDATE SET generation=excluded.generation,current_turn=excluded.current_turn,start_token=excluded.start_token,running=1",
params![actor,
instance.to_string(),
s.session_id,
generation,
turn,
token]))?;
                if c.config.watchdog.enabled
                    && c.config.watchdog.validate_capabilities(&cap).is_ok()
                {
                    let now = c.snapshot.utc.timestamp_millis();
                    // 未知平台启动证据先挂起；spool重放另由显式apply_replayed挂起。
                    let state = if !c.runtime_continuity && (c.snapshot.boot_id.is_empty() || c.snapshot.boot_id == "unknown")
                    {
                        "suspended_unknown"
                    } else {
                        "active"
                    };
                    storage(tx.execute("INSERT OR IGNORE INTO activity_state(actor_key,turn_key,instance_id,session_id,actor_scope,actor_id,upstream_turn,started_at,last_progress_at,last_observed_sequence,observed_utc,monotonic_ms,boot_id,resume_epoch,runtime_id,state,source_evidence) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?8,?9,?8,?10,?11,?12,?13,?14,?15)",
params![actor,
turn,
instance.to_string(),
s.session_id,
enum_name(s.actor_scope)?,
s.actor_id,
s.turn_id,
now,
e.per_instance_sequence,
mono(&c.snapshot)?,
c.snapshot.boot_id,
safe_epoch(&c.snapshot)?,
self.runtime.to_string(),
state,
storage(serde_json::to_string(&e.adapter_output.source_evidence))?]))?;
                }
                effect.generation = Some(generation);
            }
            LifecycleKind::Progress | LifecycleKind::Resumed if cap.reliable_progress => {
                let turn = s
                    .turn_id
                    .as_ref()
                    .map(|t| format!("t:{t}"))
                    .or_else(|| old.as_ref().map(|r| r.1.clone()));
                if let Some(turn) = turn {
                    effect.cancelled = cancel_actor(tx, &actor, Some(&turn), false)?;
                    // 未知OS启动证据不确认连续性；历史重放另由apply_replayed统一挂起。
                    if !c.runtime_continuity && (c.snapshot.boot_id.is_empty() || c.snapshot.boot_id == "unknown") {
                        suspend_instance(tx, instance)?;
                        return Ok(effect);
                    }
                    // 恢复时重建起点，但保留alerted；同轮只告警一次。
                    storage(tx.execute("UPDATE activity_state SET started_at=CASE WHEN state!='active' OR runtime_id!=?8 OR boot_id!=?6 OR resume_epoch!=?7 OR ABS((?3-observed_utc)-(?5-monotonic_ms))>5000 THEN ?3 ELSE started_at END,last_progress_at=?3,last_observed_sequence=?4,observed_utc=?3,monotonic_ms=?5,boot_id=?6,resume_epoch=?7,runtime_id=?8,state='active',revision=revision+1 WHERE actor_key=?1 AND turn_key=?2",
params![actor,
turn,
c.snapshot.utc.timestamp_millis(),
e.per_instance_sequence,
mono(&c.snapshot)?,
c.snapshot.boot_id,
safe_epoch(&c.snapshot)?,
self.runtime.to_string()]))?;
                    storage(tx.execute(
                        "UPDATE actor_state SET running=1 WHERE actor_key=?1 AND current_turn=?2",
                        params![actor, turn],
                    ))?;
                }
            }
            _ => {}
        }
        Ok(effect)
    }
}
impl Policy {
    /// 显式后台恢复入口；同事务挂起，后续Progress/Resumed或新轮重建计时。
    pub fn suspend_persisted(
        &self,
        queue: &SqliteQueue,
        instance: Uuid,
    ) -> Result<Option<()>, QueueError> {
        queue.with_instance_transaction(instance, |tx| suspend_instance(tx, instance))
    }
    /// 同实例积压/maintenance返回None；每15秒tick，并周期导入spool新快照。
    pub fn evaluate_due(
        &self,
        queue: &SqliteQueue,
        instance: Uuid,
    ) -> Result<Option<usize>, QueueError> {
        let c = self.current_context();
        queue.with_instance_transaction(instance, |tx| self.evaluate_transaction(tx, instance, &c))
    }
}
struct Activity {
    actor: String,
    turn: String,
    session: String,
    scope: String,
    actor_id: Option<String>,
    upstream_turn: Option<String>,
    start: i64,
    progress: i64,
    revision: i64,
    evidence: String,
}
fn mono(s: &ClockSnapshot) -> Result<i64, QueueError> {
    i64::try_from(s.monotonic.as_millis()).map_err(|_| error(QueueErrorCode::InvalidInput))
}
fn safe_epoch(s: &ClockSnapshot) -> Result<u64, QueueError> {
    crate::contracts::validate_safe_integer(s.resume_epoch)
        .map_err(|_| error(QueueErrorCode::InvalidInput))?;
    Ok(s.resume_epoch)
}
/// 生命周期与通知共同使用ActorKey，不凭parent_session推测主体。
fn signal_actor(s: &LifecycleSignal) -> Result<Option<String>, QueueError> {
    let key = match s.actor_scope {
        ActorScope::Main => ActorKey::Main {
            instance_id: s.instance_id,
            session_id: s.session_id.clone(),
        },
        ActorScope::Identified => ActorKey::Identified {
            instance_id: s.instance_id,
            session_id: s.session_id.clone(),
            actor_id: s
                .actor_id
                .clone()
                .ok_or_else(|| error(QueueErrorCode::InvalidInput))?,
        },
        ActorScope::Unknown => return Ok(None),
    };
    Ok(Some(storage(serde_json::to_string(&key))?))
}
pub(super) fn suspend_instance(tx: &Transaction<'_>, instance: Uuid) -> Result<(), QueueError> {
    storage(tx.execute("UPDATE activity_state SET state='suspended_unknown',revision=revision+1 WHERE instance_id=?1 AND state='active'",
[instance.to_string()]))?;
    Ok(())
}
/// 仅取消DATA前可撤销状态，清token令旧worker CAS失效。
pub(super) fn cancel_actor(
    tx: &Transaction<'_>,
    actor: &str,
    turn: Option<&str>,
    stalled_only: bool,
) -> Result<usize, QueueError> {
    let n=storage(tx.execute("UPDATE notifications SET status='suppressed',body=NULL,lease_until=NULL,lease_token=NULL WHERE id IN (SELECT id FROM policy_notifications WHERE actor_key=?1 AND (?2 IS NULL OR turn_key=?2) AND (kind='stalled' OR (?3=0 AND kind IN ('response_finished','task_completed')))) AND (status IN ('queued','retry_wait','digest_pending') OR (status='leased' AND send_stage IN ('prepared','connecting','envelope')))",
params![actor,
turn,
stalled_only]))?;
    Ok(n)
}
fn cancel_session(
    tx: &Transaction<'_>,
    instance: Uuid,
    session: &str,
) -> Result<usize, QueueError> {
    let keys = {
        let mut stmt = storage(
            tx.prepare("SELECT actor_key FROM actor_state WHERE instance_id=?1 AND session_id=?2"),
        )?;
        let rows = storage(stmt.query_map(params![instance.to_string(), session], |r| {
            r.get::<_, String>(0)
        }))?;
        storage(rows.collect::<Result<Vec<_>, _>>())?
    };
    let mut count = 0;
    for k in keys {
        count += cancel_actor(tx, &k, None, true)?;
    }
    Ok(count)
}

/// 到期与发送共用恢复检查，发送不能依赖daemon先执行过tick。
pub(super) fn invalidate_clock(
    tx: &Transaction<'_>,
    instance: Uuid,
    s: &ClockSnapshot,
    runtime: Uuid,
    runtime_continuity: bool,
) -> Result<(), QueueError> {
    storage(tx.execute("UPDATE activity_state SET state='suspended_unknown',revision=revision+1 WHERE instance_id=?1 AND state='active' AND (runtime_id!=?2 OR boot_id!=?3 OR resume_epoch!=?4 OR ?5<monotonic_ms OR ABS((?6-observed_utc)-(?5-monotonic_ms))>5000 OR (?7=0 AND (?3='' OR ?3='unknown')))",
params![instance.to_string(),
runtime.to_string(),
s.boot_id,
safe_epoch(s)?,
mono(s)?,
s.utc.timestamp_millis(),
runtime_continuity]))?;
    Ok(())
}

impl Policy {
    /// 只使用调用方持有的IMMEDIATE事务完成持久评估；不进行网络/文件IO。
    fn evaluate_transaction(
        &self,
        tx: &Transaction<'_>,
        instance: Uuid,
        c: &Context,
    ) -> Result<usize, QueueError> {
        // 1. 持久checkpoint及独立进程运行代次都必须连续可信。
        let ordered: Option<bool> = storage(
            tx.query_row(
                "SELECT ordered FROM checkpoints WHERE instance_id=?1",
                [instance.to_string()],
                |r| r.get(0),
            )
            .optional(),
        )?;
        if ordered != Some(true) {
            suspend_instance(tx, instance)?;
            return Ok(0);
        }
        invalidate_clock(tx, instance, &c.snapshot, self.runtime, c.runtime_continuity)?;
        let cap = c.capabilities.get(&instance).cloned().unwrap_or_default();
        if !c.config.watchdog.enabled || c.config.watchdog.validate_capabilities(&cap).is_err() {
            return Ok(0);
        }
        let last: Option<i64> = storage(
            tx.query_row(
                "SELECT monotonic_ms FROM watchdog_ticks WHERE instance_id=?1 AND runtime_id=?2",
                params![instance.to_string(), self.runtime.to_string()],
                |r| r.get(0),
            )
            .optional(),
        )?;
        let now_mono = mono(&c.snapshot)?;
        if last.is_some_and(|last| now_mono >= last && now_mono - last < 15000) {
            return Ok(0);
        }
        storage(tx.execute("INSERT INTO watchdog_ticks VALUES(?1,?2,?3) ON CONFLICT(instance_id) DO UPDATE SET runtime_id=excluded.runtime_id,monotonic_ms=excluded.monotonic_ms",
params![instance.to_string(),
self.runtime.to_string(),
now_mono]))?;
        // 2. IMMEDIATE事务保证Stop/Progress与超时只有一个先提交。
        let rows = {
            let mut stmt=storage(tx.prepare("SELECT actor_key,turn_key,session_id,actor_scope,actor_id,upstream_turn,started_at,last_progress_at,revision,source_evidence FROM activity_state WHERE instance_id=?1 AND state='active' AND alerted=0"))?;
            let iter = storage(stmt.query_map([instance.to_string()], |r| {
                Ok(Activity {
                    actor: r.get(0)?,
                    turn: r.get(1)?,
                    session: r.get(2)?,
                    scope: r.get(3)?,
                    actor_id: r.get(4)?,
                    upstream_turn: r.get(5)?,
                    start: r.get(6)?,
                    progress: r.get(7)?,
                    revision: r.get(8)?,
                    evidence: r.get(9)?,
                })
            }))?;
            storage(iter.collect::<Result<Vec<_>, _>>())?
        };
        let mut count = 0;
        for row in rows {
            let anchor = if c.config.watchdog.mode == WatchdogMode::Inactivity {
                row.progress
            } else {
                row.start
            };
            let elapsed = (c.snapshot.utc.timestamp_millis() - anchor) / 1000;
            if elapsed < i64::from(c.config.watchdog.threshold_seconds) {
                continue;
            }
            let id = stable_id(&format!("stalled:{}:{}", row.actor, row.turn));
            let event = NormalizedEventV1 {
                schema_version: 1,
                adapter_id: "core-watchdog".into(),
                instance_id: instance,
                event_id: Some(id.to_string()),
                session_id: Some(row.session),
                turn_id: row.upstream_turn,
                parent_session_id: None,
                agent_kind: AgentKind::Unknown,
                actor_id: row.actor_id,
                actor_scope: storage(serde_json::from_value(serde_json::Value::String(row.scope)))?,
                kind: EventKind::Stalled,
                completion_basis: CompletionBasis::None,
                source_time: None,
                received_at: c.snapshot.utc,
                project_label: None,
                session_label: None,
                question: None,
                answer: None,
                background_state: BackgroundState::Unknown,
                resume_url: None,
                content_key_available: false,
                watchdog_detail: Some(WatchdogDetail {
                    mode: c.config.watchdog.mode,
                    threshold_seconds: c.config.watchdog.threshold_seconds,
                    elapsed_seconds: (elapsed as u64).min(crate::contracts::MAX_SAFE_INTEGER),
                    last_observed_at: utc(row.progress)?,
                }),
            };
            let envelope = IngressEnvelopeV1 {
                presence_suppressed: false,
                schema_version: 1,
                ingress_id: id,
                received_at: c.snapshot.utc,
                per_instance_sequence: None,
                adapter_output: AdapterOutput {
                    notification: Some(event),
                    lifecycle: None,
                    source_evidence: storage(serde_json::from_str(&row.evidence))?,
                },
            };
            // 3. 内部生成项不消费客户端序号，走同一策略/队列/TTL。
            insert_generated(tx, &envelope)?;
            let plan = self.plan_notification(
                tx,
                &envelope,
                OrderingEvidence::Ordered,
                Some((&row.actor, &row.turn, row.revision)),
            )?;
            insert_notification(tx, id, plan, &c.config, c.snapshot.utc)?;
            storage(tx.execute(
                "UPDATE activity_state SET alerted=1 WHERE actor_key=?1 AND turn_key=?2",
                params![row.actor, row.turn],
            ))?;
            count += 1;
        }
        Ok(count)
    }
}
