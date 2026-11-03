#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
fn main() {
    agentnotify_platform::service::register_unified_process();
    // 1. 在创建 Tauri/WebView 前分流；CLI 继承 Hook 的 stdin/stdout/stderr。
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (root, mode) = if args.first().is_some_and(|s| s == "--data-dir") && args.len() >= 2 {
        (Some(std::path::Path::new(&args[1])), &args[2..])
    } else { (None, args.as_slice()) };
    if mode.is_empty() || mode == ["daemon"] {
        let paths = root.map(agentnotify_platform::paths::PlatformPaths::override_data)
            .unwrap_or_else(agentnotify_platform::paths::PlatformPaths::discover);
        let paths = match paths { Ok(p) => p, Err(e) => { eprintln!("{}", e.0); std::process::exit(4); } };
        if mode == ["daemon"] {
            if let Err(e) = agentnotifyd::run(paths) { eprintln!("{}", e.0); std::process::exit(4); }
            return;
        }
        // 2. GUI 同样使用已解析的根目录，避免界面和命令后台分裂到不同数据目录。
        match agentnotify_platform::gui_instance::GuiInstance::claim(&paths) {
            Ok(Some(instance)) => agentnotify_desktop_lib::run(instance),
            Ok(None) => {},
            Err(e) => { eprintln!("{}", e.0); std::process::exit(4); }
        }
        return;
    }
    std::process::exit(agentnotify_cli::run(args));
}
