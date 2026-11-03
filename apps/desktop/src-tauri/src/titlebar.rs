//! 保留系统标题栏行为，仅在Windows支持时设置与桌面界面一致的颜色。
#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    #[repr(C)]
    struct HighContrast { size: u32, flags: u32, scheme: *mut u16 }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn SystemParametersInfoW(action: u32, parameter: u32, value: *mut c_void, flags: u32) -> i32;
    }
    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(window: *mut c_void, attribute: u32, value: *const c_void, size: u32) -> i32;
    }
    /// 高对比或查询失败时恢复系统颜色；旧系统不支持颜色属性时由DWM保持原生默认。
    pub(super) fn apply(window: &tauri::Window) {
        let Ok(handle) = window.hwnd() else { return; };
        let mut contrast = HighContrast { size: std::mem::size_of::<HighContrast>() as u32, flags: 0, scheme: std::ptr::null_mut() };
        // 1. 尊重系统辅助功能，未知状态也不强制自定义配色。
        let use_system = unsafe { SystemParametersInfoW(0x0042, contrast.size, (&mut contrast as *mut HighContrast).cast(), 0) } == 0 || contrast.flags & 1 != 0;
        let caption: u32 = if use_system { 0xffff_ffff } else { 0x00fb_f7f6 }; // #f6f7fb，以Win32 COLORREF的BGR顺序传入。
        let text: u32 = if use_system { 0xffff_ffff } else { 0x003b_2825 }; // #25283b，保留系统按钮与命中区域。
        // 2. 只设置非客户区颜色，不接管拖动、系统菜单或最小化/最大化/关闭按钮。
        unsafe {
            let _ = DwmSetWindowAttribute(handle.0, 35, (&caption as *const u32).cast(), 4);
            let _ = DwmSetWindowAttribute(handle.0, 36, (&text as *const u32).cast(), 4);
        }
    }
}
/// 其他系统保持原生标题栏，Windows在获得焦点或主题变化时重新尊重高对比设置。
pub fn apply(window: &tauri::Window) {
    #[cfg(windows)] windows::apply(window);
    #[cfg(not(windows))] let _ = window;
}
