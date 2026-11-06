//! 保留系统标题栏行为，仅在Windows支持时设置与桌面界面一致的颜色。
#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    #[repr(C)]
    struct HighContrast { size: u32, flags: u32, scheme: *mut u16 }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn LoadIconW(instance: *mut c_void, resource: *const u16) -> *mut c_void;
        fn SendMessageW(window: *mut c_void, message: u32, wparam: usize, lparam: isize) -> isize;
        fn SystemParametersInfoW(action: u32, parameter: u32, value: *mut c_void, flags: u32) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    }
    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(window: *mut c_void, attribute: u32, value: *const c_void, size: u32) -> i32;
    }
    /// 用 EXE 的共享资源设置大窗口图标。资源不可用时由系统保留原有回退行为。
    pub(super) fn apply_taskbar_icon(window: &tauri::Window) {
        const APP_ICON_RESOURCE: usize = 32512; // Tauri Windows 构建写入的图标组 ID。
        const WM_SETICON: u32 = 0x0080;
        const ICON_BIG: usize = 1;

        let Ok(handle) = window.hwnd() else { return; };
        // 1. 读取当前模块资源。LoadIconW 返回共享句柄，系统管理其生命周期。
        let module = unsafe { GetModuleHandleW(std::ptr::null()) };
        if module.is_null() { return; }
        let icon = unsafe { LoadIconW(module, std::ptr::without_provenance(APP_ICON_RESOURCE)) };
        if icon.is_null() { return; }
        // 2. 只设置任务栏使用的大图标，保留 Tauri 的标题栏小图标和托盘。
        unsafe { SendMessageW(handle.0, WM_SETICON, ICON_BIG, icon as isize); }
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

/// 初始化原生任务栏图标；其他平台由窗口系统沿用默认图标。
pub fn apply_taskbar_icon(window: &tauri::Window) {
    #[cfg(windows)] windows::apply_taskbar_icon(window);
    #[cfg(not(windows))] let _ = window;
}