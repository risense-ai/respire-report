#![cfg_attr(windows, windows_subsystem = "windows")]
fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let paths = if args.is_empty() {
        agentnotify_platform::paths::PlatformPaths::discover()
    } else if args.len() == 2 && args[0] == "--data-dir" {
        agentnotify_platform::paths::PlatformPaths::override_data(std::path::Path::new(&args[1]))
    } else {
        Err(agentnotify_platform::PlatformError("invalid_arguments"))
    };
    let result = paths.and_then(agentnotifyd::run);
    if let Err(e) = result {
        eprintln!("{}", e.0);
        std::process::exit(4);
    }
}
