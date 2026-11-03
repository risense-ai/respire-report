//! GUI 专用单实例门：安全根上的系统文件锁与无负载激活事件，绝不参与后台/Hook互斥。
use crate::{paths::PlatformPaths, PlatformError, Result};
use std::fs::File;
#[cfg(windows)]
use crate::windows::{Handle, wide};

pub struct GuiInstance {
    _lock: File,
    #[cfg(windows)]
    event: Handle,
}
impl GuiInstance {
    /// 首个GUI持有锁直到进程结束；后续进程仅请求激活，成功返回None。
    pub fn claim(paths: &PlatformPaths) -> Result<Option<Self>> {
        #[cfg(not(windows))]
        { let _ = paths; Err(PlatformError("gui_single_instance_native_pending")) }
        #[cfg(windows)]
        {
            use sha2::{Digest, Sha256};
            use windows_sys::Win32::{Foundation::*, Security::SECURITY_ATTRIBUTES, System::Threading::*};
            // 1. 安全目录与规范化身份统一下载包、安装入口和不同斜杠表示。
            let _security = paths.secure(true)?;
            let root = std::fs::canonicalize(&paths.data).map_err(|_| PlatformError("gui_root_unavailable"))?;
            let sid = crate::windows::current_sid()?;
            let digest = Sha256::digest(format!("{}|{}", sid, root.display()).as_bytes());
            let name = wide(format!("Local\\AgentNotify.Gui.{}", digest.iter().map(|b| format!("{b:02x}")).collect::<String>()));
            let lock_path = paths.data.join("gui.lock");
            agentnotify_core::queue::reject_links(&lock_path).map_err(|_| PlatformError("unsafe_gui_lock"))?;
            let lock = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false)
                .open(lock_path).map_err(|_| PlatformError("gui_lock_unavailable"))?;
            match lock.try_lock() {
                Ok(()) => {},
                Err(std::fs::TryLockError::WouldBlock) => {
                    // 2. 首窗口可能尚未创建事件；有界等待，失败绝不退回第二个窗口。
                    for _ in 0..30 {
                        let event = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr()) };
                        if !event.is_null() {
                            let event = Handle(event);
                            // 让用户双击产生的前台激活权限仅转给该GUI进程，不向任意进程开放。
                            let pid_path = paths.data.join("gui.pid.json");
                            agentnotify_core::queue::reject_links(&pid_path).map_err(|_| PlatformError("unsafe_gui_identity"))?;
                            let raw = std::fs::read(&pid_path).map_err(|_| PlatformError("gui_identity_unavailable"))?;
                            if raw.len() > 32 { return Err(PlatformError("gui_identity_invalid")); }
                            let pid: u32 = serde_json::from_slice(&raw).map_err(|_| PlatformError("gui_identity_invalid"))?;
                            unsafe { windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(pid); }
                            if unsafe { SetEvent(event.0) } == 0 { return Err(PlatformError("gui_activate_failed")); }
                            return Ok(None);
                        }
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    return Err(PlatformError("gui_existing_instance_unavailable"));
                },
                Err(_) => return Err(PlatformError("gui_lock_failed")),
            }
            // 3. 当前用户/SYSTEM/管理员ACL沿用本机IPC边界；事件无参数，能力仅为show。
            crate::update::write_json(&paths.data.join("gui.pid.json"), &std::process::id())?;
            let descriptor = crate::windows::descriptor(&sid)?;
            let attributes = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: descriptor.0, bInheritHandle: 0 };
            let event = unsafe { CreateEventW(&attributes, 0, 0, name.as_ptr()) };
            if event.is_null() { return Err(PlatformError("gui_event_unavailable")); }
            let event = Handle(event);
            if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS { return Err(PlatformError("gui_event_conflict")); }
            Ok(Some(Self { _lock: lock, event }))
        }
    }
    /// 短等待供GUI监听线程调用；自动复位事件在窗口ready前也保留一次激活请求。
    pub fn wait_for_activation(&self) -> Result<bool> {
        #[cfg(windows)]
        {
            use windows_sys::Win32::{Foundation::*, System::Threading::WaitForSingleObject};
            match unsafe { WaitForSingleObject(self.event.0, 250) } {
                WAIT_OBJECT_0 => Ok(true), WAIT_TIMEOUT => Ok(false), _ => Err(PlatformError("gui_event_wait_failed")),
            }
        }
        #[cfg(not(windows))]
        { Err(PlatformError("gui_single_instance_native_pending")) }
    }
}
