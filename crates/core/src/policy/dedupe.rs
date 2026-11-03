//! 只使用中立且可靠身份；Unknown无稳定event_id时不进行内容去重。
use crate::{
    contracts::{QueueError, QueueErrorCode},
    event::*,
    queue::{error, storage},
};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{Transaction, params};
use sha2::{Digest, Sha256};
#[derive(Debug, Clone)]
pub struct DedupeWindow {
    pub ingress_id: uuid::Uuid,
    pub ordered: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DedupeKey {
    pub key: String,
    pub expires_at: DateTime<Utc>,
}
/// 稳定事件/轮次保留30天；可靠actor无轮次时，只有完整非空正文证据才做10秒短窗。
pub fn dedupe_key(
    e: &NormalizedEventV1,
    generation: Option<u64>,
    window: &DedupeWindow,
) -> Result<Option<DedupeKey>, QueueError> {
    if generation.is_some_and(|g| g > crate::contracts::MAX_SAFE_INTEGER) {
        return Err(error(QueueErrorCode::InvalidInput));
    }
    let actor = ActorKey::from_event(e, window.ingress_id);
    let identity = (
        &e.adapter_id,
        e.instance_id,
        &e.session_id,
        e.actor_scope,
        &e.actor_id,
    );
    let (raw, seconds) = if let Some(id) = &e.event_id {
        (
            storage(serde_json::to_vec(&(identity, "event", id)))?,
            30 * 86400,
        )
    } else if !window.ordered || matches!(actor, ActorKey::Unknown { .. }) {
        return Ok(None);
    } else if let Some(t) = &e.turn_id {
        (
            storage(serde_json::to_vec(&(identity, "turn", t, e.kind)))?,
            30 * 86400,
        )
    } else if let Some(g) = generation {
        (
            storage(serde_json::to_vec(&(identity, "generation", g, e.kind)))?,
            30 * 86400,
        )
    } else if e.question.as_deref().is_none_or(str::is_empty)
        || e.answer.as_deref().is_none_or(str::is_empty)
    {
        // 隐私清除后的None/空字段不是内容一致证明；缺完整正文证据宁可保留通知。
        // 不读取隐私清理前输入，不额外收集或落盘用户关闭的正文。
        return Ok(None);
    } else {
        (
            storage(serde_json::to_vec(&(
                identity,
                "short",
                e.kind,
                &e.question,
                &e.answer,
            )))?,
            10,
        )
    };
    Ok(Some(DedupeKey {
        key: format!("{:x}", Sha256::digest(raw)),
        expires_at: e.received_at + Duration::seconds(seconds),
    }))
}
/// 唯一索引与IMMEDIATE事务保证并发去重；短窗不随重复事件延长。
pub(super) fn seen(
    tx: &Transaction<'_>,
    key: Option<DedupeKey>,
    now: DateTime<Utc>,
) -> Result<bool, QueueError> {
    let Some(key) = key else { return Ok(false) };
    storage(tx.execute(
        "DELETE FROM policy_dedupe WHERE expires_at<=?1",
        [now.timestamp_millis()],
    ))?;
    Ok(storage(tx.execute(
        "INSERT OR IGNORE INTO policy_dedupe VALUES(?1,?2)",
        params![key.key, key.expires_at.timestamp_millis()],
    ))? == 0)
}

/// 保存已处理Started的可靠身份，不随当前轮次切换丢失。
/// 使用与通知摘要哈希不相交的命名空间；i64::MAX为内部“不按短窗过期”哨兵，非外部时间字段。
/// 同一IMMEDIATE事务保证“检查全部身份后一起记录”，失败与generation/活动写入共同回滚。
pub(super) fn record_start(
    tx: &Transaction<'_>,
    actor: &str,
    signal: &LifecycleSignal,
) -> Result<bool, QueueError> {
    let mut keys = Vec::new();
    // turn与稳定上游开始ID分别去重；同turn换重入ID也不能变成新轮。
    for (kind, value) in [
        ("turn", signal.turn_id.as_ref()),
        ("upstream", signal.upstream_id.as_ref()),
    ] {
        if let Some(value) = value {
            keys.push(format!(
                "lifecycle_start:{}",
                storage(serde_json::to_string(&(actor, kind, value)))?
            ));
        }
    }
    for key in &keys {
        let exists: bool = storage(tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM policy_dedupe WHERE dedupe_key=?1)",
            [key],
            |r| r.get(0),
        ))?;
        if exists {
            return Ok(true);
        }
    }
    for key in keys {
        storage(tx.execute(
            "INSERT INTO policy_dedupe(dedupe_key,expires_at) VALUES(?1,?2)",
            params![key, i64::MAX],
        ))?;
    }
    Ok(false)
}
