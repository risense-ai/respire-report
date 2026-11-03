#[test]
fn service_gui_independence() {
    agentnotify_feasibility::service_independence()
        .expect("user launch entry must create a process independent from launcher and GUI");
}

#[test]
#[ignore = "invoked only by the isolated process probe"]
fn service_probe_launcher() {
    #[cfg(windows)]
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    let directory = std::env::var_os("AGENTNOTIFY_SERVICE_PROBE_DIR")
        .expect("isolated test directory required");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "service_probe_worker",
            "--ignored",
            "--nocapture",
        ])
        .env("AGENTNOTIFY_SERVICE_PROBE_DIR", &directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    command.creation_flags(0x0000_0008 | 0x0000_0200);
    let mut child = command.spawn().unwrap();
    let directory = std::path::PathBuf::from(directory);
    std::fs::write(directory.join("worker-pid"), child.id().to_string()).unwrap();
    // Wait only for readiness, then exit while the worker is deliberately waiting.
    for _ in 0..100 {
        if directory.join("worker-ready").exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("worker did not become ready");
}

#[test]
#[ignore = "invoked only by the isolated process probe"]
fn service_probe_worker() {
    let directory =
        std::path::PathBuf::from(std::env::var_os("AGENTNOTIFY_SERVICE_PROBE_DIR").unwrap());
    std::fs::write(directory.join("worker-ready"), b"ready").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    while !directory.join("launcher-exited").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "launcher exit was not confirmed"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    std::fs::write(directory.join("worker-done"), b"completed").unwrap();
}
