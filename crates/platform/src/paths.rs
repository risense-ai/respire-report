//! 位置发现不创建目录；安全对象覆盖所有后续存储访问的生命周期。
use crate::{PlatformError, Result};
use agentnotify_core::queue::DirectorySecurity;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
#[derive(Debug, Clone)]
pub struct PlatformPaths {
    pub data: PathBuf,
}
impl PlatformPaths {
    pub fn discover() -> Result<Self> {
        #[cfg(windows)]
        {
            static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
            if let Some(root) = ROOT.get() {
                return Self::override_data(root);
            }
            let found = discover_windows()?;
            let _ = ROOT.set(found.data.clone());
            Ok(found)
        }
        #[cfg(target_os = "linux")]
        {
            crate::linux::paths()
        }
        #[cfg(target_os = "macos")]
        {
            crate::macos::paths()
        }
    }
    pub fn override_data(path: &Path) -> Result<Self> {
        if !path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(PlatformError("unsafe_data_dir"));
        }
        Ok(Self {
            data: path.to_owned(),
        })
    }
    /// 新目录创建即设权限，既有目录验证失败时不修改或接管。
    pub fn secure(&self, create: bool) -> Result<Arc<dyn DirectorySecurity>> {
        #[cfg(windows)]
        {
            Ok(Arc::new(crate::windows::SecureDirectory::open(
                &self.data, create,
            )?))
        }
        #[cfg(not(windows))]
        {
            let _ = create;
            Err(PlatformError("native_directory_security_pending"))
        }
    }
}

/// 无参数入口只发现当前用户的安全根；多个安装不能静默选择并显示另一份设置。
#[cfg(windows)]
fn discover_windows() -> Result<PlatformPaths> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    // 1. 默认位置与旧 profile 位置只作为候选；计划任务用于找到用户自定义目录。
    let local = crate::windows::local_data()?.join("AgentNotify");
    let profile = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .ok_or(PlatformError("profile_unavailable"))?
        .join("AgentNotify");
    let mut candidates = vec![profile.clone(), local];
    // 系统查询有总期限；文件输出不依赖继承管道 EOF，超时终止并回收查询进程。
    use std::io::{Read, Seek, SeekFrom};
    use std::time::{Duration, Instant};
    let output =
        tempfile::tempfile().map_err(|_| PlatformError("installation_discovery_failed"))?;
    // 未安装后台任务属于正常状态；查询显式 exit 0，多动作歧义仍 exit 4。
    let mut child=Command::new("powershell.exe").args(["-NoProfile","-NonInteractive","-Command",
        "$ErrorActionPreference='Stop'; $sid=[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value; $t=Get-ScheduledTask -TaskName ('AgentNotify.'+$sid) -ErrorAction SilentlyContinue; if($t){if(@($t.Actions).Count-ne 1){exit 4}; $t.Actions[0].WorkingDirectory | ConvertTo-Json -Compress}; exit 0"])
        .stdin(Stdio::null()).stderr(Stdio::null()).stdout(output.try_clone().map_err(|_|PlatformError("installation_discovery_failed"))?).creation_flags(0x08000000).spawn()
        .map_err(|_|PlatformError("installation_discovery_failed"))?;
    let end = Instant::now() + Duration::from_secs(10);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(25)),
            other => {
                if child.kill().is_err() && child.try_wait().ok().flatten().is_none() {
                    return Err(PlatformError("installation_discovery_cleanup_failed"));
                }
                child
                    .wait()
                    .map_err(|_| PlatformError("installation_discovery_cleanup_failed"))?;
                return Err(PlatformError(if other.is_err() {
                    "installation_discovery_failed"
                } else {
                    "installation_discovery_timeout"
                }));
            }
        }
    };
    if !status.success() {
        return Err(PlatformError("installation_discovery_failed"));
    }
    let mut output = output;
    output
        .seek(SeekFrom::Start(0))
        .map_err(|_| PlatformError("installation_discovery_failed"))?;
    let mut bytes = Vec::new();
    output
        .take(32769)
        .read_to_end(&mut bytes)
        .map_err(|_| PlatformError("installation_discovery_failed"))?;
    if bytes.len() > 32768 {
        return Err(PlatformError("installation_discovery_failed"));
    }
    if !bytes.iter().all(u8::is_ascii_whitespace) {
        let text =
            String::from_utf8(bytes).map_err(|_| PlatformError("installation_discovery_failed"))?;
        let root: String = serde_json::from_str(text.trim_start_matches('\u{feff}').trim())
            .map_err(|_| PlatformError("installation_discovery_failed"))?;
        candidates.push(PathBuf::from(root));
    }
    // 2. 路径规范化去重；现有配置也必须可安全访问，异常不回落新空目录。
    let mut found: Vec<(PathBuf, PathBuf)> = Vec::new();
    for root in candidates {
        if !root.join("installation.json").exists() && !root.join("config.json").exists() {
            continue;
        }
        let paths = PlatformPaths::override_data(&root)?;
        paths.secure(false)?;
        if root.join("installation.json").exists() {
            crate::service::read_installation(&paths)?;
        }
        let canonical =
            std::fs::canonicalize(&root).map_err(|_| PlatformError("unsafe_data_dir"))?;
        if !found.iter().any(|(p, _)| p == &canonical) {
            found.push((canonical, root));
        }
    }
    if found.len() > 1 {
        return Err(PlatformError("multiple_installations_use_data_dir"));
    }
    PlatformPaths::override_data(found.first().map(|(_, root)| root).unwrap_or(&profile))
}
