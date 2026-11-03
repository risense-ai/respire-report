mod health;
mod runner;
/// 在独立进程中运行既有后台循环，不依赖 GUI 生命周期。
pub fn run(paths: agentnotify_platform::paths::PlatformPaths) -> agentnotify_platform::Result<()> { runner::run(paths) }
