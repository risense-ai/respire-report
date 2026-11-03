//! 常驻宿主运行时：仅非秘密配置落文件，密钥仍由系统存储按随机引用读取。
use crate::{PlatformError, Result, credentials::NativeCredentialStore, paths::PlatformPaths};
use agentnotify_core::{
    clock::{Clock, ClockSnapshot, RuntimeClockEvidence},
    config::ConfigV1,
    contracts::*,
    policy::Policy,
    queue::*,
};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct HostSettings {
    pub content_key: Option<CredentialRef>,
    pub delivery: Option<DeliverySettings>,
    #[serde(default)]
    pub instances: Vec<RegisteredInstance>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisteredInstance {
    pub id: uuid::Uuid,
    pub adapter_id: String,
    pub capabilities: agentnotify_core::config::AdapterCapabilities,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliverySettings {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub tls: String,
    pub oauth: bool,
    pub from: String,
    pub recipients: Vec<String>,
    pub credential: CredentialRef,
}
impl HostSettings {
    pub fn load(root: &Path) -> Result<Self> {
        let p = root.join("host.json");
        if !p.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read(p).map_err(|_| PlatformError("host_config_unavailable"))?;
        if raw.len() > 65536 {
            return Err(PlatformError("host_config_too_large"));
        }
        serde_json::from_slice(&raw).map_err(|_| PlatformError("host_config_invalid"))
    }
}
/// UTC只用于业务；本进程Instant与平台working-time共同检出休眠/长停顿。
pub struct NativeClock {
    start: Instant,
    state: Mutex<TimeState>,
    power_available: AtomicBool,
}
struct TimeState {
    last: Instant,
    utc: i64,
    elapsed: u64,
    working: u64,
    epoch: u64,
}
impl NativeClock {
    pub fn new() -> Self {
        let (e, w) = native_times();
        Self {
            start: Instant::now(),
            power_available: AtomicBool::new(false),
            state: Mutex::new(TimeState {
                last: Instant::now(),
                utc: chrono::Utc::now().timestamp_millis(),
                elapsed: e,
                working: w,
                epoch: 0,
            }),
        }
    }
    pub fn set_power_available(&self, available: bool) {
        self.power_available.store(available, Ordering::SeqCst);
    }
    pub fn continuity_available(&self) -> bool {
        let s = self.state.lock().unwrap();
        s.elapsed > 0 && s.working > 0
    }
    /// 每秒采样；异常先变更epoch，调用方先挂起旧活动再接受新的实时证明。
    pub fn sample(&self) -> (ClockSnapshot, bool) {
        let mut s = self.state.lock().unwrap();
        let now = Instant::now();
        let utc = chrono::Utc::now();
        let (e, w) = native_times();
        let delta = now.duration_since(s.last).as_millis() as i64;
        let broken = e == 0
            || w == 0
            || delta > 5000
            || e < s.elapsed
            || w < s.working
            || e.saturating_sub(s.elapsed)
                .abs_diff(w.saturating_sub(s.working))
                > 2000
            || (utc.timestamp_millis() - s.utc - delta).abs() > 5000;
        if broken {
            s.epoch = s.epoch.saturating_add(1);
        }
        s.last = now;
        s.utc = utc.timestamp_millis();
        s.elapsed = e;
        s.working = w;
        (
            ClockSnapshot {
                utc,
                monotonic: self.start.elapsed(),
                boot_id: "unknown".into(),
                resume_epoch: s.epoch.saturating_add(crate::power::current_epoch()),
            },
            broken,
        )
    }
}
impl RuntimeClockEvidence for NativeClock {
    fn runtime_snapshot(&self) -> (ClockSnapshot, bool) {
        let (snapshot, _) = self.sample();
        (
            snapshot,
            self.power_available.load(Ordering::SeqCst) && self.continuity_available(),
        )
    }
}
impl Default for NativeClock {
    fn default() -> Self {
        Self::new()
    }
}
impl Clock for NativeClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }
    fn monotonic(&self) -> Duration {
        self.start.elapsed()
    }
}
fn native_times() -> (u64, u64) {
    #[cfg(windows)]
    unsafe {
        let mut working = 0;
        if windows_sys::Win32::System::WindowsProgramming::QueryUnbiasedInterruptTime(&mut working)
            == 0
        {
            return (0, 0);
        }
        (
            windows_sys::Win32::System::SystemInformation::GetTickCount64(),
            working / 10000,
        )
    }
    #[cfg(not(windows))]
    {
        (0, 0)
    }
}
pub struct HostRuntime {
    pub paths: PlatformPaths,
    pub queue: Arc<SqliteQueue>,
    pub policy: Arc<Policy>,
    pub clock: Arc<NativeClock>,
    pub settings: HostSettings,
    pub config: ConfigV1,
    pub content_unavailable: bool,
    pub security: Arc<dyn DirectorySecurity>,
}
impl HostRuntime {
    /// 不创建内容key，不在读取失败时覆盖旧key；正文不可用时有效配置降为元数据。
    pub fn open(paths: PlatformPaths, create: bool) -> Result<Self> {
        let security = paths.secure(create)?;
        let config_path = paths.data.join("config.json");
        let mut config = if config_path.exists() {
            ConfigV1::load(&config_path).map_err(|_| PlatformError("config_invalid"))?
        } else {
            ConfigV1::default()
        };
        let settings = HostSettings::load(&paths.data)?;
        let store = NativeCredentialStore::default();
        let protector = settings
            .content_key
            .as_ref()
            .and_then(|r| store.load_content_key(r).ok())
            .map(|p| Arc::new(p) as Arc<dyn BodyProtector>);
        let content_unavailable = protector.is_none();
        if content_unavailable {
            config.include_question = false;
            config.include_answer = false;
        }
        let clock = Arc::new(NativeClock::new());
        let (snapshot, _) = clock.sample();
        let timezone = iana_time_zone::get_timezone()
            .map_err(|_| PlatformError("system_timezone_unavailable"))?;
        let policy = Arc::new(
            Policy::new(config.clone(), &timezone, snapshot)
                .map_err(|_| PlatformError("policy_invalid"))?,
        );
        policy.set_presence(Arc::new(crate::presence::NativePresence));
        for i in &settings.instances {
            policy.register_instance(i.id, i.capabilities.clone());
        }
        let queue = Arc::new(
            SqliteQueue::open(
                &paths.data,
                config.clone(),
                QueueDependencies {
                    clock: clock.clone(),
                    security: security.clone(),
                    protector,
                    participant: policy.clone(),
                    faults: Arc::new(NoFaults),
                },
            )
            .map_err(|_| PlatformError("queue_unavailable"))?,
        );
        Ok(Self {
            paths,
            queue,
            policy,
            clock,
            settings,
            config,
            content_unavailable,
            security,
        })
    }
}
