//! 可注入时间源；UTC用于展示/业务，单调时间用于运行预算。
use chrono::{DateTime, Utc};
use std::time::Duration;
/// 不依赖平台实现，测试可注入FakeClock而不等待真实TTL。
pub trait Clock {
    fn now(&self) -> DateTime<Utc>;
    fn monotonic(&self) -> Duration;
}
#[derive(Debug, Clone, PartialEq, Eq)]
/// 同一快照同时记录开机及恢复代次，后续watchdog据此识别不可靠经过时间。
pub struct ClockSnapshot {
    pub utc: DateTime<Utc>,
    pub monotonic: Duration,
    pub boot_id: String,
    pub resume_epoch: u64,
}

/// 常驻宿主提供即时连续性证据；仅读取时钟/原子状态，不允许网络或文件IO。
/// bool表示本进程观测可信，独立于可能unknown的OS boot_id。
pub trait RuntimeClockEvidence: Send + Sync {
    fn runtime_snapshot(&self) -> (ClockSnapshot, bool);
}
