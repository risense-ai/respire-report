#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
fn main() { std::process::exit(agentnotify_cli::run(std::env::args().skip(1).collect())); }
