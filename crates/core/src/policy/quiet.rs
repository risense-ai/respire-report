//! 真实IANA规则；重叠取最早开始/最晚结束，缺口取下一个有效分钟。
use crate::{
    config::QuietConfig,
    contracts::{QueueError, QueueErrorCode},
    queue::error,
};
use chrono::{DateTime, Duration, LocalResult, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuietInterval {
    pub key: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}
/// system_zone来自平台注入的IANA名称；本模块不猜测操作系统时区。
pub fn interval(
    c: &QuietConfig,
    now: DateTime<Utc>,
    system_zone: &str,
) -> Result<Option<QuietInterval>, QueueError> {
    if !c.enabled {
        return Ok(None);
    }
    let zone: Tz = if c.timezone == "system" {
        system_zone
    } else {
        &c.timezone
    }
    .parse()
    .map_err(|_| error(QueueErrorCode::InvalidInput))?;
    let start = NaiveTime::parse_from_str(&c.start, "%H:%M")
        .map_err(|_| error(QueueErrorCode::InvalidInput))?;
    let end = NaiveTime::parse_from_str(&c.end, "%H:%M")
        .map_err(|_| error(QueueErrorCode::InvalidInput))?;
    // 今日和前一日开始的区间：跨午夜和DST重复小时同属一个实际UTC区间。
    let date = now.with_timezone(&zone).date_naive();
    for days in [-1, 0] {
        let day = date + Duration::days(days);
        let finish = if start >= end {
            day + Duration::days(1)
        } else {
            day
        };
        let a = resolve(zone, day.and_time(start), false)?;
        let b = resolve(zone, finish.and_time(end), true)?;
        if a <= now && now < b {
            return Ok(Some(QuietInterval {
                key: format!("quiet:{}:{}:{}", zone, a.timestamp(), b.timestamp()),
                start: a,
                end: b,
            }));
        }
    }
    Ok(None)
}
/// 有界推进覆盖整日时区跳变，不使用固定UTC小时差。
fn resolve(zone: Tz, mut local: NaiveDateTime, end: bool) -> Result<DateTime<Utc>, QueueError> {
    for _ in 0..=2880 {
        match zone.from_local_datetime(&local) {
            LocalResult::Single(t) => return Ok(t.with_timezone(&Utc)),
            LocalResult::Ambiguous(a, b) => {
                return Ok(if end { a.max(b) } else { a.min(b) }.with_timezone(&Utc));
            }
            LocalResult::None => local += Duration::minutes(1),
        }
    }
    Err(error(QueueErrorCode::InvalidInput))
}
