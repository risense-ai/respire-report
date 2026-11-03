//! 电源回调只增加进程静态epoch，不访问可释放上下文或执行存储/网络。
use std::sync::atomic::{AtomicU64, Ordering};
static EPOCH: AtomicU64 = AtomicU64::new(0);
pub fn current_epoch() -> u64 {
    EPOCH.load(Ordering::SeqCst)
}
pub struct PowerWatcher {
    #[cfg(windows)]
    handle: *mut std::ffi::c_void,
    pub available: bool,
    #[cfg(windows)]
    _subscription: Box<windows_sys::Win32::System::Power::DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS>,
}
impl PowerWatcher {
    pub fn register() -> Self {
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::System::Power::*;
            let p = Box::new(DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
                Callback: Some(callback),
                Context: std::ptr::null_mut(),
            });
            let mut handle = std::ptr::null_mut();
            let result = PowerRegisterSuspendResumeNotification(
                windows_sys::Win32::UI::WindowsAndMessaging::DEVICE_NOTIFY_CALLBACK,
                (p.as_ref() as *const DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS)
                    .cast_mut()
                    .cast(),
                &mut handle,
            );
            Self {
                handle,
                available: result == 0,
                _subscription: p,
            }
        }
        #[cfg(not(windows))]
        {
            Self { available: false }
        }
    }
    pub fn epoch(&self) -> u64 {
        EPOCH.load(Ordering::SeqCst)
    }
}
#[cfg(windows)]
unsafe extern "system" fn callback(
    _: *const std::ffi::c_void,
    kind: u32,
    _: *const std::ffi::c_void,
) -> u32 {
    if matches!(kind, 4 | 7 | 18) {
        EPOCH.fetch_add(1, Ordering::SeqCst);
    }
    0
}
impl Drop for PowerWatcher {
    fn drop(&mut self) {
        #[cfg(windows)]
        if self.available {
            unsafe {
                windows_sys::Win32::System::Power::PowerUnregisterSuspendResumeNotification(
                    self.handle as isize,
                );
            }
        }
    }
}
