//! macOS路径发现；Keychain/LaunchAgent实机待办，不假称Windows证据通用。
use crate::{PlatformError, Result, paths::PlatformPaths};
pub fn paths() -> Result<PlatformPaths> {
    let home = std::path::PathBuf::from(
        std::env::var_os("HOME").ok_or(PlatformError("home_unavailable"))?,
    );
    if !home.is_absolute() {
        return Err(PlatformError("invalid_home"));
    }
    Ok(PlatformPaths {
        data: home.join("Library/Application Support/AgentNotify"),
    })
}
