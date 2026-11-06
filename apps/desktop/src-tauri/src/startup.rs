//! GUI 启动错误不依赖 WebView 显示；只呈现固定阶段/错误码，绝不格式化原始错误或 panic 内容。
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
static STAGE: AtomicU8 = AtomicU8::new(0);
static REPORTING: AtomicBool = AtomicBool::new(false);
#[derive(Clone, Copy)]
pub enum Stage {
    Discover = 0,
    Directory = 1,
    WebView = 2,
    Interface = 3,
}
pub fn stage(value: Stage) {
    STAGE.store(value as u8, Ordering::Relaxed);
}
/// 仅在 main 确定为 GUI 后安装；CLI、Hook、daemon 不改 panic 路由也不弹窗。
pub fn install() {
    stage(Stage::Discover);
    std::panic::set_hook(Box::new(|_| {
        show("gui_panic");
    }));
}
fn explanation(code: &str) -> (&'static str, &'static str) {
    match code {
        "installation_discovery_timeout" => (
            "installation_discovery_timeout",
            "系统安装位置查询超时。请记录本提示，检查 Windows 任务计划程序与 PowerShell 是否可正常使用。",
        ),
        "installation_discovery_cleanup_failed" => (
            "installation_discovery_cleanup_failed",
            "系统查询进程未能确认回收。请将此错误码交给协助 Agent，核对本次 PowerShell 查询进程后再重试。",
        ),
        "installation_discovery_failed" => (
            "installation_discovery_failed",
            "无法查询安装位置。请检查 Windows PowerShell/任务计划程序是否被系统策略阻止，并把此错误码交给协助 Agent。",
        ),
        "multiple_installations_use_data_dir" => (
            "multiple_installations_use_data_dir",
            "发现多个数据位置。请使用原安装入口，或由协助 Agent 核对正确的数据目录；不要直接删除旧数据。",
        ),
        "unsafe_owner_or_acl" | "unsafe_acl" | "unsafe_data_dir" | "unsafe_gui_lock" => (
            "startup_directory_access_denied",
            "数据目录未通过权限或路径安全校验。请由协助 Agent 核对当前用户与目录权限；不要关闭安全校验或批量修改系统权限。",
        ),
        "local_fixed_disk_required" => (
            "local_fixed_disk_required",
            "数据目录必须位于本机固定磁盘。请由协助 Agent 核对用户目录是否重定向到网络或移动磁盘。",
        ),
        "webview_runtime_unavailable" => (
            "webview_runtime_unavailable",
            "无法检测可用的 Microsoft Edge WebView2 Runtime。请安装或修复微软官方 Evergreen x64 Runtime 后重试；不需要安装开发工具。",
        ),
        "gui_existing_instance_unavailable" | "gui_activate_failed" => (
            "gui_activation_unavailable",
            "已有设置窗口未能响应激活。请检查系统托盘或任务管理器，并将此错误码交给协助 Agent；不要停止通知后台来代替定位。",
        ),
        "desktop_runtime_failed" => (
            "desktop_runtime_failed",
            "桌面窗口初始化失败。请核对 WebView2 Runtime 和系统应用控制记录，并将此阶段/错误码交给协助 Agent。",
        ),
        "gui_panic" => (
            "gui_panic",
            "桌面程序发生异常。请保留本提示并联系协助 Agent；当前提示不包含会话内容或凭据。",
        ),
        _ => (
            "gui_startup_failed",
            "无法完成桌面启动。请将本提示交给协助 Agent，并提供 Windows 架构及是否存在 AgentNotify 进程；不要先重装或清空数据。",
        ),
    }
}
/// 未知上游消息只映射固定错误码，避免路径、会话内容或凭据混入原生对话框。
pub fn show(code: &str) {
    if REPORTING.swap(true, Ordering::AcqRel) {
        return;
    }
    let (code, next) = explanation(code);
    let phase = match STAGE.load(Ordering::Relaxed) {
        0 => "发现安装位置",
        1 => "检查数据目录与已有窗口",
        2 => "初始化桌面运行时",
        _ => "运行设置界面",
    };
    let text = format!(
        "AgentNotify 未能正常打开或继续运行。\n\n阶段：{phase}\n错误码：{code}\n程序版本：{}（Windows x64）\n\n{next}\n\n可按 Ctrl+C 复制此提示；请保留截图供排查。",
        env!("CARGO_PKG_VERSION")
    );
    #[cfg(windows)]
    {
        use std::ffi::c_void;
        #[link(name = "user32")]
        unsafe extern "system" {
            fn MessageBoxW(
                window: *mut c_void,
                text: *const u16,
                title: *const u16,
                flags: u32,
            ) -> i32;
        }
        let text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let title: Vec<u16> = "AgentNotify 启动诊断"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        // MB_OK | MB_ICONERROR | MB_SETFOREGROUND；无需 WebView、磁盘日志或后台运行。
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                0x00010010,
            );
        }
    }
    #[cfg(not(windows))]
    eprintln!("{text}");
    REPORTING.store(false, Ordering::Release);
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_details_are_not_displayed() {
        let (code, next) = explanation("secret/path/user-content");
        assert_eq!(code, "gui_startup_failed");
        assert!(!next.contains("secret"));
        assert_eq!(
            explanation("webview_runtime_unavailable").0,
            "webview_runtime_unavailable"
        );
    }
}
