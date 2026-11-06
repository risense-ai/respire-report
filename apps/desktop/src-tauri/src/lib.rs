pub mod startup;
mod commands;
mod onboarding;
mod desktop_shell;
mod titlebar;

pub fn run(instance: agentnotify_platform::gui_instance::GuiInstance) -> tauri::Result<()> {
    tauri::Builder::default()
        .manage(desktop_shell::ShellState::default())
        .setup(move |app| {
            desktop_shell::setup(app);
            startup::stage(startup::Stage::Interface);
            let handle = app.handle().clone();
            // 窗口创建后才消费请求；线程持有GUI锁，不持有后台发送者锁。
            std::thread::spawn(move || loop {
                match instance.wait_for_activation() {
                    Ok(true) => { eprintln!("gui_activation_received"); let target = handle.clone(); if handle.run_on_main_thread(move || { eprintln!("gui_activation_dispatched"); desktop_shell::show(&target); }).is_err() { eprintln!("gui_activation_dispatch_failed"); } },
                    Ok(false) => {},
                    Err(_) => { eprintln!("gui_event_wait_failed"); loop { std::thread::park(); } },
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| desktop_shell::window_event(window, event))
        .manage(commands::AppState::default())
        .invoke_handler(tauri::generate_handler![
            desktop_shell::desktop_info,
            desktop_shell::confirm_close,
            desktop_shell::desktop_ready,
            desktop_shell::confirm_quit,
            onboarding::installation_info,
            onboarding::select_data_directory,
            onboarding::start_integration_verification,
            onboarding::poll_integration_verification,
            commands::get_health,
            commands::update_availability,
            commands::load_config,
            commands::save_config,
            commands::load_mail,
            commands::save_mail,
            commands::inspect_integrations,
            commands::inspect_legacy_preferences,
            commands::plan_legacy_preferences,
            commands::apply_legacy_preferences,
            commands::plan_integration,
            commands::apply_integration,
            commands::get_history,
            commands::get_diagnostics,
            commands::create_agent_bundle,
            commands::preview_test,
            commands::send_test,
            commands::control_background,
            commands::preview_uninstall,
            commands::apply_uninstall,
        ])
        .run(tauri::generate_context!())
}
