//! 工具中立策略；入站、看门狗、发送检查共用SQLite事务。
pub mod aggregation;
pub mod dedupe;
mod digest;
pub mod quiet;
pub mod watchdog;
use crate::{
    clock::{ClockSnapshot, RuntimeClockEvidence},
    config::*,
    contracts::{QueueError, QueueErrorCode, QueueStatus},
    event::*,
    queue::{
        NotificationPlan, OrderingEvidence, SqliteQueue, TransactionParticipant, error, storage,
    },
};
use chrono::{DateTime, Duration, Utc};
pub use digest::DeliverySummary;
use digest::add_digest;
use rusqlite::{OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::{Arc, RwLock}};
use uuid::Uuid;
/// 平台只提供当前用户是否离席，不让核心读取Windows或客户端私有数据。
pub trait PresenceEvidence: Send + Sync { fn is_away(&self, idle_seconds: u32) -> Option<bool>; }
#[derive(Clone)]
struct Context {
    config: ConfigV1,
    system_zone: String,
    snapshot: ClockSnapshot,
    capabilities: HashMap<Uuid, AdapterCapabilities>,
    runtime_continuity: bool,
    runtime_clock: Option<Arc<dyn RuntimeClockEvidence>>,
    presence: Option<Arc<dyn PresenceEvidence>>,
}
/// 独立运行代次识别daemon重启，不替代平台OS boot_id。
pub struct Policy {
    context: RwLock<Context>,
    runtime: Uuid,
}
impl Policy {
    /// 创建一个后台运行代次；必须由平台注入时区和时钟，未知boot保持挂起。
    pub fn new(
        config: ConfigV1,
        system_zone: &str,
        snapshot: ClockSnapshot,
    ) -> Result<Self, QueueError> {
        config
            .validate()
            .map_err(|_| error(QueueErrorCode::InvalidInput))?;
        system_zone
            .parse::<chrono_tz::Tz>()
            .map_err(|_| error(QueueErrorCode::InvalidInput))?;
        Ok(Self {
            runtime: Uuid::new_v4(),
            context: RwLock::new(Context {
                config,
                system_zone: system_zone.into(),
                snapshot,
                capabilities: HashMap::new(),
                runtime_continuity: false,
                runtime_clock: None,
                presence: None,
            }),
        })
    }
    /// 能力来自已登记适配器，不读取客户端原始协议。
    pub fn register_instance(&self, id: Uuid, c: AdapterCapabilities) {
        self.context.write().unwrap().capabilities.insert(id, c);
    }
    /// 发布平台时间快照；到期及DATA前读取该快照检验连续性。
    pub fn set_snapshot(&self, s: ClockSnapshot) {
        self.context.write().unwrap().snapshot = s;
    }
    /// 仅常驻宿主发布本进程连续性；它不是OS boot身份，恢复后必须先挂起旧活动。
    pub fn set_runtime_snapshot(&self, snapshot: ClockSnapshot, continuous: bool) {
        let mut context = self.context.write().unwrap();
        context.snapshot = snapshot;
        context.runtime_continuity = continuous;
    }
    /// 注入常驻宿主的只读时钟证明；DATA门槛必须同步获取，不能依赖主循环上次tick。
    pub fn set_runtime_clock(&self, clock: Arc<dyn RuntimeClockEvidence>) {
        self.context.write().unwrap().runtime_clock=Some(clock);
    }
    /// 每次领取及DATA前实时查询；不能用主循环上次采样代替用户回到电脑的事实。
    pub fn set_presence(&self, presence: Arc<dyn PresenceEvidence>) { self.context.write().unwrap().presence=Some(presence); }
    fn current_context(&self) -> Context {
        let mut context=self.context.read().unwrap().clone();
        if let Some(clock)=&context.runtime_clock {
            let (snapshot,continuous)=clock.runtime_snapshot();
            context.snapshot=snapshot;
            context.runtime_continuity=continuous;
        }
        context
    }
    /// 更新系统IANA时区；不复活已跳过通知，待发项下次领取时复查。
    pub fn set_system_timezone(&self, zone: &str) -> Result<(), QueueError> {
        zone.parse::<chrono_tz::Tz>()
            .map_err(|_| error(QueueErrorCode::InvalidInput))?;
        self.context.write().unwrap().system_zone = zone.into();
        Ok(())
    }
    /// 设置不复活终态；发送按当前规则复核。
    pub fn set_config(&self, config: ConfigV1) -> Result<(), QueueError> {
        config
            .validate()
            .map_err(|_| error(QueueErrorCode::InvalidInput))?;
        self.context.write().unwrap().config = config;
        Ok(())
    }
    /// 仅来源/用户规则分类；完整无状态决定通过共享contracts::Policy::decide。
    pub fn classify_output(&self, output: &AdapterOutput) -> QueueStatus {
        classify(&self.context.read().unwrap().config, output)
    }
    /// 在接收事务中持久化决定；非Ordered不推断代次、聚合或取消。
    fn plan_notification(
        &self,
        tx: &Transaction<'_>,
        e: &IngressEnvelopeV1,
        order: OrderingEvidence,
        activity: Option<(&str, &str, i64)>,
    ) -> Result<NotificationPlan, QueueError> {
        let c = self.current_context();
        let Some(n) = &e.adapter_output.notification else {
            return Ok(NotificationPlan {
                status: QueueStatus::Suppressed,
                not_before: e.received_at,
            });
        };
        // 1. Unknown每次接收独立，可靠身份共用ActorKey。
        let key = ActorKey::from_event(n, e.ingress_id);
        let actor = if matches!(key, ActorKey::Unknown { .. }) {
            None
        } else {
            Some(storage(serde_json::to_string(&key))?)
        };
        let generation: Option<u64> = if order == OrderingEvidence::Ordered {
            if let Some(a) = &actor {
                storage(
                    tx.query_row(
                        "SELECT generation FROM actor_state WHERE actor_key=?1",
                        [a],
                        |r| r.get(0),
                    )
                    .optional(),
                )?
            } else {
                None
            }
        } else {
            None
        };
        let turn = if order == OrderingEvidence::Ordered {
            n.turn_id
                .as_ref()
                .map(|t| format!("t:{t}"))
                .or_else(|| generation.map(|g| format!("g:{g}")))
        } else {
            None
        };
        let duplicate = dedupe::seen(
            tx,
            dedupe::dedupe_key(
                n,
                generation,
                &dedupe::DedupeWindow {
                    ingress_id: e.ingress_id,
                    ordered: order == OrderingEvidence::Ordered,
                },
            )?,
            e.received_at,
        )?;
        // 2. 重复结束不再次关闭已恢复活动；结束只关闭本actor/轮次。
        if !duplicate
            && order == OrderingEvidence::Ordered
            && matches!(
                n.kind,
                EventKind::ResponseFinished
                    | EventKind::TaskCompleted
                    | EventKind::ExecutionFailed
                    | EventKind::Interrupted
            )
        {
            if let (Some(a), Some(t)) = (&actor, &turn) {
                storage(tx.execute("UPDATE activity_state SET state='closed',revision=revision+1 WHERE actor_key=?1 AND turn_key=?2 AND state!='closed'",
params![a,
t]))?;
                storage(tx.execute(
                    "UPDATE actor_state SET running=0 WHERE actor_key=?1 AND current_turn=?2",
                    params![a, t],
                ))?;
                watchdog::cancel_actor(tx, a, Some(t), true)?;
            }
        }
        let decision = crate::contracts::Policy::decide(
            self,
            n,
            &e.adapter_output.source_evidence,
            &c.config,
            e.received_at,
        );
        let (mut status, mut reason) = match decision {
            crate::contracts::PolicyDecision::Suppress { reason } => (
                QueueStatus::Suppressed,
                if reason == "quiet" {
                    "quiet_receive"
                } else {
                    "user_policy"
                },
            ),
            crate::contracts::PolicyDecision::Digest { .. } => {
                (QueueStatus::DigestPending, "quiet_receive")
            }
            _ => (QueueStatus::Queued, "queued"),
        };
        // 本机接收时已活跃/未知的通知不因稍后离席或spool导入而复活；生命周期已处理。
        if e.presence_suppressed {
            status = QueueStatus::Suppressed;
            reason = "presence_receive";
        }
        if duplicate {
            status = QueueStatus::Suppressed;
            reason = "duplicate";
        }
        // 通用来源每分钟最多十条可投递入站；在线与离线共享持久滚动窗口。
        // ingress 已在同一事务插入当前事件，因此第十一条开始记录为按规则抑制。
        if status != QueueStatus::Suppressed
            && e.adapter_output.source_evidence.origin_evidence == OriginEvidence::CallerReported
        {
            let recent: i64 = storage(tx.query_row(
                "SELECT count(*) FROM (SELECT 1 FROM ingress WHERE instance_id=?1 AND received_at>?2 AND received_at<=?3 LIMIT 11)",
                params![n.instance_id.to_string(), e.received_at.timestamp_millis()-60000, e.received_at.timestamp_millis()],
                |r| r.get(0),
            ))?;
            if recent > 10 {
                status = QueueStatus::Suppressed;
                reason = "source_rate";
            }
        }
        let normal = matches!(
            n.kind,
            EventKind::ResponseFinished | EventKind::TaskCompleted
        );
        let mut due = e.received_at + Duration::seconds(if normal { 3 } else { 0 });
        let mut first = None;
        let mut count = 1;
        let interval = quiet::interval(&c.config.quiet, e.received_at, &c.system_zone)?;
        if status == QueueStatus::Queued && interval.is_some() {
            status = if c.config.quiet.behavior == QuietBehavior::Digest {
                QueueStatus::DigestPending
            } else {
                QueueStatus::Suppressed
            };
            reason = "quiet_receive";
        }
        // 3. 正常结束聚合同actor；异常/未知独立，关闭聚合仍保留3秒。
        if status == QueueStatus::Queued
            && c.config.policy.aggregate_finishes
            && order == OrderingEvidence::Ordered
            && aggregation::aggregation_window(n).is_some()
        {
            let (f, total, d) =
                aggregation::merge(tx, actor.as_ref().expect("reliable actor"), e.received_at)?;
            first = Some(f);
            count = total;
            due = d;
        }
        let (actor, turn, revision) = if let Some((a, t, r)) = activity {
            (Some(a.to_owned()), Some(t.to_owned()), Some(r))
        } else {
            (actor, turn, None)
        };
        storage(tx.execute("INSERT INTO policy_notifications(id,actor_key,turn_key,kind,activity_revision,reason,aggregate_first,aggregate_count) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
params![e.ingress_id.to_string(),
actor,
turn,
enum_name(n.kind)?,
revision,
reason,
first,
count]))?;
        if status == QueueStatus::DigestPending {
            let i = interval.expect("quiet digest");
            add_digest(tx, e, &i.key, i.end)?;
        }
        Ok(NotificationPlan {
            status,
            not_before: due,
        })
    }
}
impl Policy {
    /// 发送前复查持久决定、TTL、revision和免打扰；仅领取时预留一次配额。
    fn recheck(
        &self,
        tx: &Transaction<'_>,
        id: Uuid,
        now: DateTime<Utc>,
        reserve: bool,
    ) -> Result<bool, QueueError> {
        let c = self.current_context();
        let row:Option<(String,
i64,
String,
Option<String>,
Option<String>,
Option<i64>)>=storage(tx.query_row("SELECT n.status,n.expires_at,p.kind,p.actor_key,p.turn_key,p.activity_revision FROM notifications n JOIN policy_notifications p ON p.id=n.id WHERE n.id=?1",
[id.to_string()],
|r|Ok((r.get(0)?,
r.get(1)?,
r.get(2)?,
r.get(3)?,
r.get(4)?,
r.get(5)?))).optional())?;
        let Some((status, expiry, kind, actor, turn, revision)) = row else {
            return Ok(true);
        };
        if !matches!(status.as_str(), "queued" | "retry_wait" | "leased") {
            return Ok(false);
        }
        if expiry <= now.timestamp_millis() {
            suppress(tx, id, "expired")?;
            return Ok(false);
        }
        let inst: String = storage(tx.query_row(
            "SELECT instance_id FROM ingress WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        ))?;
        watchdog::invalidate_clock(
            tx,
            storage(Uuid::parse_str(&inst))?,
            &c.snapshot,
            self.runtime,
            c.runtime_continuity,
        )?;
        if kind == "digest" && !digest::refresh(tx, id, now)? {
            suppress(tx, id, "suppressed")?;
            return Ok(false);
        }
        if kind == "stalled" {
            let valid:bool=storage(tx.query_row("SELECT EXISTS(SELECT 1 FROM activity_state WHERE actor_key=?1 AND turn_key=?2 AND revision=?3 AND state='active')",
params![actor,
turn,
revision],
|r|r.get(0)))?;
            if !valid {
                suppress(tx, id, "suppressed")?;
                return Ok(false);
            }
        }
        let raw: Vec<u8> = storage(tx.query_row(
            "SELECT metadata FROM ingress WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        ))?;
        let e: IngressEnvelopeV1 = storage(serde_json::from_slice(&raw))?;
        if !c.config.enabled
            || (kind != "digest"
                && classify(&c.config, &e.adapter_output) == QueueStatus::Suppressed)
        {
            suppress(tx, id, "suppressed")?;
            return Ok(false);
        }
        if let Some(i) = quiet::interval(&c.config.quiet, now, &c.system_zone)? {
            if c.config.quiet.behavior == QuietBehavior::Digest && kind != "digest" {
                add_digest(tx, &e, &i.key, i.end)?;
                suppress(tx, id, "digest_pending")?;
            } else {
                suppress(tx, id, "suppressed")?;
            }
            return Ok(false);
        }
        // 旧auto_action：离席优先发送；仍活跃则最多等待事件接收后的idle_seconds。
        // DATA前重新活跃也退回待处理，不把策略等待计作投递失败或SMTP尝试。
        let away_only = c.config.presence.mode == PresenceMode::AwayOnly;
        if away_only {
            let away = c.presence.as_ref().and_then(|p| p.is_away(c.config.presence.idle_seconds));
            if away != Some(true) {
                let deadline = e.received_at + Duration::seconds(c.config.presence.idle_seconds as i64);
                if now >= deadline {
                    suppress(tx, id, "suppressed")?;
                } else {
                    let due = (now + Duration::seconds(5)).min(deadline);
                    // before_send调用方在同一IMMEDIATE事务已验证lease token及expected stage；
                    // 本更新再限制prepared/connecting/envelope，禁止DATA开始后回退。
                    storage(tx.execute("UPDATE notifications SET status=CASE WHEN status='leased' THEN 'queued' ELSE status END,due_at=?2,attempts=CASE WHEN status='leased' THEN max(0,attempts-1) ELSE attempts END,lease_until=NULL,lease_token=NULL,send_stage='prepared' WHERE id=?1 AND send_stage IN('prepared','connecting','envelope') AND (status IN('queued','retry_wait') OR (status='leased' AND lease_token IS NOT NULL))",
                        params![id.to_string(), due.timestamp_millis()]))?;
                }
                return Ok(false);
            }
        }
        // AwayOnly到DATA前确认无需继续等待才占用配额；Always保留领取时预留。
        if (reserve && !away_only) || (!reserve && away_only) {
            // 全局滚动60秒计入重试；预留后不因网络结果回退配额。
            storage(tx.execute(
                "DELETE FROM policy_rate WHERE attempted_at<=?1",
                [now.timestamp_millis() - 60000],
            ))?;
            let count: i64 =
                storage(tx.query_row("SELECT count(*) FROM policy_rate", [], |r| r.get(0)))?;
            if count >= 10 {
                if kind == "digest" {
                    storage(tx.execute(
                        "UPDATE notifications SET due_at=?2 WHERE id=?1",
                        params![
                            id.to_string(),
                            (now + Duration::minutes(1)).timestamp_millis()
                        ],
                    ))?;
                } else {
                    let end = utc((now.timestamp_millis().div_euclid(60000) + 1) * 60000)?;
                    add_digest(
                        tx,
                        &e,
                        &format!("rate:{}", now.timestamp_millis().div_euclid(60000)),
                        end,
                    )?;
                    suppress(tx, id, "digest_pending")?;
                }
                return Ok(false);
            }
            storage(tx.execute(
                "INSERT INTO policy_rate(attempted_at) VALUES(?1)",
                [now.timestamp_millis()],
            ))?;
        }
        Ok(true)
    }
}
impl TransactionParticipant for Policy {
    fn apply_replayed(
        &self,
        tx: &Transaction<'_>,
        e: &IngressEnvelopeV1,
        order: OrderingEvidence,
    ) -> Result<NotificationPlan, QueueError> {
        let plan = self.apply(tx, e, order)?;
        // 重放上下文来自队列，不依赖可后跳的墙钟；新的实时Progress/Resumed才确认连续活动。
        if e.adapter_output
            .lifecycle
            .as_ref()
            .is_some_and(|s| s.kind != LifecycleKind::SessionClosed)
        {
            watchdog::suspend_instance(tx, crate::queue::instance(e))?;
        }
        Ok(plan)
    }

    fn apply(
        &self,
        tx: &Transaction<'_>,
        e: &IngressEnvelopeV1,
        order: OrderingEvidence,
    ) -> Result<NotificationPlan, QueueError> {
        self.observe_lifecycle(tx, e, order)?;
        self.plan_notification(tx, e, order, None)
    }
    fn before_lease(
        &self,
        tx: &Transaction<'_>,
        id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<bool, QueueError> {
        self.recheck(tx, id, now, true)
    }
    fn before_send(
        &self,
        tx: &Transaction<'_>,
        id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<bool, QueueError> {
        self.recheck(tx, id, now, false)
    }
}
fn classify(c: &ConfigV1, o: &AdapterOutput) -> QueueStatus {
    if !c.enabled {
        return QueueStatus::Suppressed;
    }
    let Some(n) = &o.notification else {
        return QueueStatus::Suppressed;
    };
    if n.kind == EventKind::AttentionRequired && !c.policy.notify_attention {
        return QueueStatus::Suppressed;
    }
    if o.source_evidence.origin_evidence == OriginEvidence::VerifiedAdapter
        && o.source_evidence.source_class == SourceClass::Subagent
        && !c.policy.notify_subagents
    {
        return QueueStatus::Suppressed;
    }
    // 错误/未知结果不套用内部成功静默。
    if c.policy.suppress_internal_success
        && verified_internal(&o.source_evidence)
        && o.source_evidence.background_result == BackgroundResult::Success
        && matches!(
            n.kind,
            EventKind::ResponseFinished | EventKind::TaskCompleted
        )
    {
        return QueueStatus::Suppressed;
    }
    QueueStatus::Queued
}
fn verified_internal(e: &SourceEvidence) -> bool {
    e.origin_evidence == OriginEvidence::VerifiedAdapter
        && e.source_class == SourceClass::InternalBackground
}
fn enum_name<T: serde::Serialize>(v: T) -> Result<String, QueueError> {
    let v = storage(serde_json::to_value(v))?;
    v.as_str()
        .map(str::to_owned)
        .ok_or_else(|| error(QueueErrorCode::InvalidInput))
}
fn utc(ms: i64) -> Result<DateTime<Utc>, QueueError> {
    DateTime::from_timestamp_millis(ms).ok_or_else(|| error(QueueErrorCode::InvalidInput))
}
fn stable_id(key: &str) -> Uuid {
    let hash = Sha256::digest(key.as_bytes());
    Uuid::from_bytes(hash[..16].try_into().expect("fixed digest"))
}
/// 保持DATA后的既有发送事实，取消前置状态时清理token。
fn suppress(tx: &Transaction<'_>, id: Uuid, status: &str) -> Result<(), QueueError> {
    storage(tx.execute("UPDATE notifications SET status=?2,body=NULL,lease_until=NULL,lease_token=NULL WHERE id=?1 AND (status IN ('queued','retry_wait','digest_pending') OR (status='leased' AND send_stage IN ('prepared','connecting','envelope')))",
params![id.to_string(),
status]))?;
    Ok(())
}
fn insert_generated(tx: &Transaction<'_>, e: &IngressEnvelopeV1) -> Result<(), QueueError> {
    e.validate()
        .map_err(|_| error(QueueErrorCode::InvalidInput))?;
    storage(tx.execute(
        "INSERT OR IGNORE INTO ingress VALUES(?1,?2,NULL,?1,?3,?4,1)",
        params![
            e.ingress_id.to_string(),
            crate::queue::instance(e).to_string(),
            e.received_at.timestamp_millis(),
            storage(serde_json::to_vec(e))?
        ],
    ))?;
    Ok(())
}
fn insert_notification(
    tx: &Transaction<'_>,
    id: Uuid,
    plan: NotificationPlan,
    c: &ConfigV1,
    now: DateTime<Utc>,
) -> Result<(), QueueError> {
    storage(tx.execute(
        "INSERT OR IGNORE INTO notifications(id,status,due_at,expires_at) VALUES(?1,?2,?3,?4)",
        params![
            id.to_string(),
            enum_name(plan.status)?,
            plan.not_before.timestamp_millis(),
            (now + Duration::hours(i64::from(c.queue_ttl_hours.min(24)))).timestamp_millis()
        ],
    ))?;
    Ok(())
}

/// 共享契约的无状态策略入口；持久去重、最后结束时间和revision由事务参与者补全。
impl crate::contracts::Policy for Policy {
    fn decide(
        &self,
        event: &NormalizedEventV1,
        evidence: &SourceEvidence,
        config: &ConfigV1,
        now: DateTime<Utc>,
    ) -> crate::contracts::PolicyDecision {
        use crate::contracts::PolicyDecision;
        if config.validate().is_err() {
            return PolicyDecision::Suppress {
                reason: "invalid_config".into(),
            };
        }
        let output = AdapterOutput {
            notification: Some(event.clone()),
            lifecycle: None,
            source_evidence: evidence.clone(),
        };
        if classify(config, &output) == QueueStatus::Suppressed {
            return PolicyDecision::Suppress {
                reason: "user_policy".into(),
            };
        }
        let zone = self.context.read().unwrap().system_zone.clone();
        // 接收/发送任一静默即不发即时邮件；跳过决定由事务永久保存。
        for instant in [event.received_at, now] {
            match quiet::interval(&config.quiet, instant, &zone) {
                Ok(Some(i)) => {
                    return if config.quiet.behavior == QuietBehavior::Digest {
                        PolicyDecision::Digest { window_key: i.key }
                    } else {
                        PolicyDecision::Suppress {
                            reason: "quiet".into(),
                        }
                    };
                }
                Err(_) => {
                    return PolicyDecision::Suppress {
                        reason: "invalid_timezone".into(),
                    };
                }
                Ok(None) => {}
            }
        }
        let normal = matches!(
            event.kind,
            EventKind::ResponseFinished | EventKind::TaskCompleted
        );
        let wait = if normal
            && config.policy.aggregate_finishes
            && aggregation::aggregation_window(event).is_some()
        {
            10
        } else if normal {
            3
        } else {
            0
        };
        PolicyDecision::Notify {
            not_before: event.received_at + Duration::seconds(wait),
        }
    }
}
