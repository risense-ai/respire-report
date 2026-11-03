//! 正常结束仅同可靠actor聚合；异常和未知actor独立。
use crate::{contracts::QueueError, event::*, queue::storage};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{OptionalExtension, Transaction, params};
pub type AggregationKey = ActorKey;
pub fn aggregation_window(e: &NormalizedEventV1) -> Option<AggregationKey> {
    if !matches!(
        e.kind,
        EventKind::ResponseFinished | EventKind::TaskCompleted
    ) {
        return None;
    }
    let key = ActorKey::from_event(e, uuid::Uuid::nil());
    if matches!(key, ActorKey::Unknown { .. }) {
        None
    } else {
        Some(key)
    }
}
/// 返回首条时间、总计数、最早发送；合并项保留事实并清理待发送正文。
pub(super) fn merge(
    tx: &Transaction<'_>,
    actor: &str,
    now: DateTime<Utc>,
) -> Result<(i64, i64, DateTime<Utc>), QueueError> {
    let old:Option<(String,
i64,
i64)>=storage(tx.query_row("SELECT p.id,p.aggregate_first,p.aggregate_count FROM policy_notifications p JOIN notifications n ON n.id=p.id WHERE p.actor_key=?1 AND p.kind IN ('response_finished','task_completed') AND n.status='queued' AND p.reason='queued' AND n.due_at>?2 ORDER BY p.aggregate_first LIMIT 1",
params![actor,
now.timestamp_millis()],
|r|Ok((r.get(0)?,
r.get(1)?,
r.get(2)?))).optional())?;
    let (first, count) = if let Some((id, first, count)) = old {
        storage(tx.execute(
            "UPDATE notifications SET status='suppressed',body=NULL WHERE id=?1",
            [&id],
        ))?;
        storage(tx.execute(
            "UPDATE policy_notifications SET reason='aggregated' WHERE id=?1",
            [id],
        ))?;
        (first, count + 1)
    } else {
        (now.timestamp_millis(), 1)
    };
    let due = (now + Duration::seconds(3))
        .max(DateTime::from_timestamp_millis(first).expect("stored UTC") + Duration::seconds(10));
    Ok((first, count, due))
}
