//! Linux路径发现；原生目录安全与systemd用户服务尚待原生实现和实机验证。
use crate::{PlatformError, Result, paths::PlatformPaths};
pub fn paths() -> Result<PlatformPaths> {
    let root = if let Some(p) = std::env::var_os("XDG_DATA_HOME") {
        std::path::PathBuf::from(p)
    } else {
        std::path::PathBuf::from(std::env::var_os("HOME").ok_or(PlatformError("home_unavailable"))?)
            .join(".local/share")
    };
    if !root.is_absolute() {
        return Err(PlatformError("invalid_xdg_path"));
    }
    Ok(PlatformPaths {
        data: root.join("AgentNotify"),
    })
}
