pub mod uninstall;
mod commands;
mod migration_secret;
mod update_commands;
mod agent_bundle_commands;
mod host_commands;
mod integration_commands;
mod output;
/// 执行既有 CLI 协议，保留调用者的标准输入输出，不初始化桌面运行时。
pub fn run(args: Vec<String>) -> i32 {
    // 宿主Hook即使参数错误也不能用exit 2阻断AI；诊断只写stderr且不回显输入。
    let hook_invocation = commands::is_hook_invocation(&args);
    let code = match commands::parse(&args) {
        Ok(invocation) => {
            if matches!(invocation.command,commands::Command::UninstallHelper(_)){return commands::execute(invocation);}
            let paths = invocation.data_dir.as_ref().map(|p| agentnotify_platform::paths::PlatformPaths::override_data(p)).unwrap_or_else(agentnotify_platform::paths::PlatformPaths::discover);
            match paths.and_then(|p| {
                // 诊断在合法根解析后、安装转发前；无法验证安全根时不落文件。
                if hook_invocation { host_commands::hook_diagnostic(&p, "entry", "ok"); }
                let result = agentnotify_platform::service::dispatch_installed_cli(&p, &args);
                if let Err(e) = &result { if hook_invocation { host_commands::hook_diagnostic(&p, "rejected", e.0); } }
                result
            }) {
                Ok(Some(code)) => code,
                Ok(None) => commands::execute(invocation),
                Err(e) => { eprintln!("{}", e.0); if hook_invocation { 0 } else { 4 } }
            }
        },
        Err(error) if hook_invocation => {
            let report = agentnotify_core::contracts::CommandReport::<serde_json::Value>::failure(
                &error.code,
                "Hook参数无效，事件未持久化。",
                None,
            );
            if let Ok(json) = serde_json::to_string(&report) {
                eprintln!("{json}");
            }
            0
        }
        Err(error) => output::report(2, &error.code, "命令参数无效，请使用 --help。", None),
    };
    code
}
