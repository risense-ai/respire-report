//! 本机桌面托盘：菜单只操作当前数据根，复用配置CAS和后台归属检查。
use crate::commands;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{io::Read, sync::{Mutex, atomic::{AtomicBool, Ordering}}};
use tauri::{AppHandle, Emitter, Manager, Wry, menu::{Menu, MenuItem, CheckMenuItem, PredefinedMenuItem}, tray::{TrayIconBuilder, TrayIconEvent, MouseButton, MouseButtonState}};

#[derive(Default)]
pub struct ShellState {
    ready: AtomicBool,
    busy: AtomicBool,
    issue: Mutex<Option<String>>,
}
#[derive(Serialize)]
pub struct DesktopInfo {
    version: &'static str,
    build_id: String,
    executable: String,
    data_dir: String,
    runtime_issue: Option<String>,
}
/// 运行身份来自当前可执行文件及启动参数；不读取邮件内容或凭据。
#[tauri::command]
pub fn desktop_info(app: AppHandle) -> DesktopInfo {
    let executable = std::env::current_exe().ok();
    let build_id = executable.as_ref().and_then(|path| {
        let mut file = std::fs::File::open(path).ok()?;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 65536];
        loop { let count = file.read(&mut buffer).ok()?; if count == 0 { break; } hash.update(&buffer[..count]); }
        Some(format!("{:x}", hash.finalize())[..8].to_owned())
    }).unwrap_or_else(|| "unavailable".into());
    DesktopInfo {
        version: env!("CARGO_PKG_VERSION"), build_id,
        executable: executable.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        data_dir: commands::paths().map(|p| p.data.to_string_lossy().into_owned()).unwrap_or_default(),
        runtime_issue: app.state::<ShellState>().issue.lock().ok().and_then(|v| v.clone()),
    }
}
/// 打开现有设置窗口，不启动发送或修改任何配置。
pub(crate) fn show(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        if window.show().is_err() { eprintln!("gui_show_failed"); }
        if window.unminimize().is_err() { eprintln!("gui_unminimize_failed"); }
        if window.set_focus().is_err() { eprintln!("gui_focus_failed"); }
    }
}
/// 固定错误码持久留在当前进程，前端未监听时仍可通过desktop_info回读。
fn report(app: &AppHandle, code: String) {
    if let Ok(mut issue) = app.state::<ShellState>().issue.lock() { *issue = Some(code.clone()); }
    show(app);
    let _ = app.emit("agentnotify-runtime-error", serde_json::json!({"code": code}));
}
/// 从磁盘重新加载真实偏好，失败时禁用开关，避免默认值误导用户。
fn refresh(enabled: &CheckMenuItem<Wry>, quiet: &CheckMenuItem<Wry>) {
    match commands::load_config() {
        Ok(snapshot) => {
            let _ = enabled.set_checked(snapshot.config.enabled);
            let _ = quiet.set_checked(snapshot.config.quiet.enabled);
            let _ = enabled.set_enabled(true); let _ = quiet.set_enabled(true);
        },
        Err(_) => { let _ = enabled.set_enabled(false); let _ = quiet.set_enabled(false); },
    }
}
/// 通知开关串行化；保存后采用现有后台重载。退出界面不进入此配置路径。
fn perform(app: &AppHandle, action: &str) -> Result<(), String> {
    // 1. 使用磁盘当前版本作CAS，只改变用户选择的单个偏好。
    let mut snapshot = commands::load_config()?;
    match action {
        "enabled" => snapshot.config.enabled = !snapshot.config.enabled,
        "quiet" => snapshot.config.quiet.enabled = !snapshot.config.quiet.enabled,
        _ => return Ok(()),
    }
    let saved = commands::save_config_inner(snapshot.config, snapshot.revision, true)?;
    let _ = app.emit("agentnotify-config-changed", ());
    if !saved.active { return Err(saved.reason); }
    Ok(())
}
/// 仅成功创建托盘后启用关闭隐藏，失败则保留普通可关闭窗口。
pub fn setup(app: &tauri::App) {
    let handle = app.handle();
    if let Some(window) = handle.get_webview_window("main") {
        let window = window.as_ref().window();
        crate::titlebar::apply(&window);
        crate::titlebar::apply_taskbar_icon(&window);
    }
    let result = (|| -> tauri::Result<()> {
        let open = MenuItem::with_id(handle, "open", "打开设置", true, None::<&str>)?;
        let enabled = CheckMenuItem::with_id(handle, "enabled", "启用通知", true, false, None::<&str>)?;
        let quiet = CheckMenuItem::with_id(handle, "quiet", "启用免打扰时段", true, false, None::<&str>)?;
        let quit = MenuItem::with_id(handle, "quit", "退出程序", true, None::<&str>)?;
        let separator = PredefinedMenuItem::separator(handle)?;
        refresh(&enabled, &quiet);
        let menu = Menu::with_items(handle, &[&open, &enabled, &quiet, &separator, &quit])?;
        let menu_enabled = enabled.clone(); let menu_quiet = quiet.clone();
        let click_enabled = enabled.clone(); let click_quiet = quiet.clone();
        // 无外部图像依赖的A标识；正式窗口图标存在时优先使用同一图标。
        let icon = handle.default_window_icon().cloned().unwrap_or_else(|| {
            let mut rgba = vec![0; 32 * 32 * 4];
            for y in 0..32usize { for x in 0..32usize {
                let i = (y * 32 + x) * 4;
                let a = (y >= 7 && y <= 25) && ((x.abs_diff(16) >= (25-y)/3 && x.abs_diff(16) <= (25-y)/3+2) || (y >= 18 && y <= 20 && (10..=22).contains(&x)));
                rgba[i..i+4].copy_from_slice(if a { &[255,255,255,255] } else { &[101,88,217,255] });
            }}
            tauri::image::Image::new_owned(rgba, 32, 32)
        });
        TrayIconBuilder::with_id("agentnotify")
            .icon(icon).tooltip("AgentNotify · 任务邮件通知").menu(&menu).show_menu_on_left_click(false)
            .on_tray_icon_event(move |tray, event| {
                if matches!(event, TrayIconEvent::Click { .. }) { refresh(&click_enabled, &click_quiet); }
                if matches!(event, TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }) { show(tray.app_handle()); }
            })
            .on_menu_event(move |app, event| {
                let action = event.id().as_ref();
                if action == "open" { show(app); return; }
                // 退出仅结束设置窗口与托盘，不依赖配置可读或修改独立后台。
                if action == "quit" { show(app); let _ = app.emit("agentnotify-quit-requested", ()); return; }
                if !matches!(action, "enabled" | "quiet") { return; }
                if app.state::<ShellState>().busy.swap(true, Ordering::AcqRel) { return; }
                let app = app.clone(); let action = action.to_owned();
                let enabled = menu_enabled.clone(); let quiet = menu_quiet.clone();
                std::thread::spawn(move || {
                    if let Err(code) = perform(&app, &action) { report(&app, code); }
                    refresh(&enabled, &quiet);
                    app.state::<ShellState>().busy.store(false, Ordering::Release);
                });
            }).build(handle)?;
        Ok(())
    })();
    match result {
        Ok(()) => handle.state::<ShellState>().ready.store(true, Ordering::Release),
        Err(_) => report(handle, "tray_unavailable".into()),
    }
}
/// 关闭窗口仅收进已成功初始化的托盘，不等于关闭通知。
pub fn window_event(window: &tauri::Window, event: &tauri::WindowEvent) {
    if matches!(event, tauri::WindowEvent::ThemeChanged(_) | tauri::WindowEvent::Focused(true)) { crate::titlebar::apply(window); }
    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
        api.prevent_close(); let _=window.emit("agentnotify-close-requested", ());
    }
}

/// 前端完成未保存草稿确认后才隐藏窗口；后台生命周期不变。
#[tauri::command]
pub fn confirm_close(app:AppHandle)->Result<(),String>{if !app.state::<ShellState>().ready.load(Ordering::Acquire){app.exit(0);return Ok(());}if let Some(w)=app.get_webview_window("main"){w.hide().map_err(|_|"window_hide_failed")?;}Ok(())}
/// 退出仅结束GUI/托盘，不保存或修改通知配置，不停止独立后台。
#[tauri::command]
pub fn confirm_quit(app:AppHandle){app.exit(0);}
/// 前端注册草稿守卫后报告就绪；仅写入当前安全根的PID供首次切根确认。
#[tauri::command]
pub fn desktop_ready()->Result<(),String>{
    use std::io::Write;
    let p=commands::paths()?;let _guard=p.secure(true).map_err(|e|e.0.to_owned())?;
    let dest=p.data.join("gui.ready.json");agentnotify_core::queue::reject_links(&dest).map_err(|_|"unsafe_gui_identity")?;
    let mut f=tempfile::NamedTempFile::new_in(&p.data).map_err(|_|"gui_ready_write_failed")?;
    f.write_all(std::process::id().to_string().as_bytes()).and_then(|_|f.as_file().sync_all()).map_err(|_|"gui_ready_write_failed")?;
    f.persist(dest).map_err(|_|"gui_ready_write_failed")?;Ok(())
}