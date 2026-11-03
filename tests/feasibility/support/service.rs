use std::{
    fs,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// The test's launcher exits before its detached worker writes a completion marker.
#[cfg(windows)]
pub fn service_independence() -> Result<(), String> {
    // 1. Use a unique temporary directory and only this test executable.
    let directory = tempfile::tempdir().map_err(|_| "create isolated process directory")?;
    let status = Command::new(std::env::current_exe().map_err(|_| "current test executable")?)
        .args([
            "--exact",
            "service_probe_launcher",
            "--ignored",
            "--nocapture",
        ])
        .env("AGENTNOTIFY_SERVICE_PROBE_DIR", directory.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| "launch isolated user entry")?;
    if !status.success() {
        return Err("isolated launcher failed".into());
    }
    fs::write(directory.path().join("launcher-exited"), b"confirmed")
        .map_err(|_| "launcher exit marker")?;
    // 2. The worker has no GUI and continues after the launch entry has exited.
    let deadline = Instant::now() + Duration::from_secs(8);
    while !directory.path().join("worker-done").exists() {
        if Instant::now() >= deadline {
            return Err("independent worker timeout".into());
        }
        thread::sleep(Duration::from_millis(25));
    }
    let pid = fs::read_to_string(directory.path().join("worker-pid"))
        .map_err(|_| "worker identity missing")?;
    if pid.parse::<u32>().is_err() {
        return Err("worker pid invalid".into());
    }
    super::record(
        "service_gui_independence",
        "Windows detached current-user process",
        "Rust std=1.95.0",
        &["DETACHED_PROCESS", "CREATE_NEW_PROCESS_GROUP"],
        "launcher exited; unique worker subsequently completed with GUI absent; no installed service or login task",
    )
}

#[cfg(not(windows))]
pub fn service_independence() -> Result<(), String> {
    Err("blocked: Windows process probe requires a native Windows host".into())
}
