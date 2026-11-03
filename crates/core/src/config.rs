//! 配置只包含非敏感偏好，不接收密码、token或安全内容密钥。
use crate::event::contract_enum;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::Path,
};
use ts_rs::TS;
contract_enum! { /// 看门狗模式；inactivity必须具备可靠Progress能力。
pub enum WatchdogMode { ElapsedWithoutFinish, Inactivity } }
contract_enum! { /// 静默区间行为；默认跳过，不在早晨突发补发。
pub enum QuietBehavior { Skip, Digest } }
contract_enum! { /// 通知发送条件；AwayOnly复用旧Windows离席/锁屏语义。
pub enum PresenceMode { Always, AwayOnly } }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct PresenceConfig {
    pub mode: PresenceMode,
    /// 最近输入距今的秒数；锁屏无需等到该阈值。
    pub idle_seconds: u32,
}
impl Default for PresenceConfig {
    fn default() -> Self { Self { mode: PresenceMode::Always, idle_seconds: 300 } }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct QuietConfig {
    pub enabled: bool,
    pub start: String,
    pub end: String,
    pub timezone: String,
    pub behavior: QuietBehavior,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct WatchdogConfig {
    pub enabled: bool,
    pub mode: WatchdogMode,
    #[schemars(range(min = 60, max = 86400))]
    pub threshold_seconds: u32,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
/// 能力来自已核验适配器注册，不由事件正文声明。
pub struct AdapterCapabilities {
    pub reliable_started: bool,
    pub reliable_progress: bool,
    pub reliable_actor: bool,
    pub reliable_turn_or_generation: bool,
}
impl WatchdogConfig {
    pub const TICK_SECONDS: u32 = 15;
    /// 开关不等于具备能力；缺少可靠主体/开始/轮次时拒绝启用。
    pub fn validate_capabilities(&self, c: &AdapterCapabilities) -> Result<(), ConfigError> {
        if !(60..=86400).contains(&self.threshold_seconds) {
            return Err(ConfigError::new("invalid_watchdog"));
        }
        if !self.enabled {
            return Ok(());
        }
        if !c.reliable_started || !c.reliable_actor || !c.reliable_turn_or_generation {
            return Err(ConfigError::new("started_capability_required"));
        }
        if self.mode == WatchdogMode::Inactivity && !c.reliable_progress {
            return Err(ConfigError::new("progress_capability_required"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
/// 中立用户规则；旧配置缺省时采用新装默认，原有字段保持原值。
pub struct NotificationRules {
    /// 正常结束聚合；关闭时仍等待3秒。
    pub aggregate_finishes: bool,
    /// 已核验内部后台且成功证据同时成立才允许过滤。
    pub suppress_internal_success: bool,
    /// 子代理默认保留，不依据正文猜测来源。
    pub notify_subagents: bool,
    /// 等待输入/批准默认关闭。
    pub notify_attention: bool,
}
impl Default for NotificationRules {
    fn default() -> Self {
        Self {
            aggregate_finishes: true,
            suppress_internal_success: true,
            notify_subagents: true,
            notify_attention: false,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
/// 新装隐私默认关闭正文；迁移必须显式保留原选择，不用本默认值覆盖。
pub struct ConfigV1 {
    #[schemars(range(min = 1, max = 1))]
    pub schema_version: u8,
    pub enabled: bool,
    pub include_question: bool,
    pub include_answer: bool,
    pub quiet: QuietConfig,
    pub metadata_retention_days: u32,
    pub queue_ttl_hours: u32,
    pub watchdog: WatchdogConfig,
    #[serde(default)]
    pub policy: NotificationRules,
    #[serde(default)]
    pub presence: PresenceConfig,
}
impl Default for ConfigV1 {
    fn default() -> Self {
        Self {
            schema_version: 1,
            enabled: true,
            include_question: false,
            include_answer: false,
            quiet: QuietConfig {
                enabled: true,
                start: "23:00".into(),
                end: "07:30".into(),
                timezone: "system".into(),
                behavior: QuietBehavior::Skip,
            },
            metadata_retention_days: 30,
            queue_ttl_hours: 24,
            policy: NotificationRules::default(),
            presence: PresenceConfig::default(),
            watchdog: WatchdogConfig {
                enabled: false,
                mode: WatchdogMode::ElapsedWithoutFinish,
                threshold_seconds: 1800,
            },
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub code: &'static str,
}
impl ConfigError {
    pub fn new(code: &'static str) -> Self {
        Self { code }
    }
}
impl ConfigV1 {
    /// 配置检查不启用watchdog；能力校验由具体已登记适配器调用。
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !(1..=86400).contains(&self.presence.idle_seconds) { return Err(ConfigError::new("invalid_presence")); }
        if self.schema_version != 1 {
            return Err(ConfigError::new("unsupported_schema"));
        }
        let time_ok = |s: &str| {
            let b = s.as_bytes();
            b.len() == 5
                && b[2] == b':'
                && b[0..2].iter().all(u8::is_ascii_digit)
                && b[3..5].iter().all(u8::is_ascii_digit)
                && s[0..2].parse::<u8>().is_ok_and(|h| h < 24)
                && s[3..5].parse::<u8>().is_ok_and(|m| m < 60)
        };
        if !time_ok(&self.quiet.start)
            || !time_ok(&self.quiet.end)
            || (self.quiet.timezone != "system"
                && self.quiet.timezone.parse::<chrono_tz::Tz>().is_err())
            || self.metadata_retention_days == 0
            || self.queue_ttl_hours == 0
        {
            return Err(ConfigError::new("invalid_config"));
        }
        if !(60..=86400).contains(&self.watchdog.threshold_seconds) {
            return Err(ConfigError::new("invalid_watchdog"));
        }
        Ok(())
    }
    /// 有界读取并检查版本；错误不包含配置原文或路径。
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let file = std::fs::File::open(path).map_err(|_| ConfigError::new("config_unavailable"))?;
        let mut raw = Vec::new();
        file.take(1024 * 1024 + 1)
            .read_to_end(&mut raw)
            .map_err(|_| ConfigError::new("config_unavailable"))?;
        if raw.len() > 1024 * 1024 {
            return Err(ConfigError::new("input_too_large"));
        }
        let value: serde_json::Value =
            serde_json::from_slice(&raw).map_err(|_| ConfigError::new("invalid_config"))?;
        if value.get("schema_version").and_then(|v| v.as_u64()) != Some(1) {
            return Err(ConfigError::new("unsupported_schema"));
        }
        let cfg: Self =
            serde_json::from_value(value).map_err(|_| ConfigError::new("invalid_config"))?;
        cfg.validate()?;
        Ok(cfg)
    }
    /// 同目录临时文件先sync再原子替换；校验失败保持原文件。
    pub fn save_atomic(&self, path: &Path) -> Result<(), ConfigError> {
        // 1. 先校验，避免失败输入破坏原设置。
        self.validate()?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        // 2. 临时文件权限由tempfile限制；仅写非敏感偏好，凭据不属于此结构。
        let mut temp = tempfile::NamedTempFile::new_in(parent)
            .map_err(|_| ConfigError::new("config_write_failed"))?;
        let bytes =
            serde_json::to_vec_pretty(self).map_err(|_| ConfigError::new("invalid_config"))?;
        temp.write_all(&bytes)
            .and_then(|_| temp.as_file().sync_all())
            .map_err(|_| ConfigError::new("config_write_failed"))?;
        // 3. persist使用平台原子替换，失败保留原文件，不使用先删后写。
        temp.persist(path)
            .map_err(|_| ConfigError::new("config_write_failed"))?;
        Ok(())
    }
}
