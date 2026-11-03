//! 当前用户平台边界；真实注册仅由显式service命令触发。
pub mod credentials;
pub mod ipc;
pub mod gui_instance;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod paths;
pub mod power;
pub mod presence;
pub mod runtime;
pub mod service;
pub mod update;
#[cfg(windows)]
pub mod windows;
#[derive(Debug, Clone)]
pub struct PlatformError(pub &'static str);
impl std::fmt::Display for PlatformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for PlatformError {}
pub type Result<T> = std::result::Result<T, PlatformError>;

pub mod uninstall_receipt;
