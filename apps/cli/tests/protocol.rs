use std::io::Write;
use std::process::{Command, Stdio};
fn run(args: &[&str], input: Option<&[u8]>) -> (i32, String, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_agentnotify"));
    c.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    if let Some(data) = input {
        child.stdin.take().unwrap().write_all(data).unwrap();
    } else {
        drop(child.stdin.take());
    }
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}
#[test]
fn help_is_real_and_no_password_option() {
    let (code, out, _) = run(&["--help"], None);
    assert_eq!(code, 0);
    assert!(out.contains("--data-dir") && out.contains("不会绕过权限"));
    assert!(!out.contains("--password"));
}
#[test]
fn unavailable_never_reports_success() {
    for args in [
        vec!["doctor", "--json"],
        vec!["integration", "inspect", "--json"],
        vec!["service", "status"],
        vec!["test", "--send"],
    ] {
        let (code, out, _) = run(&args, None);
        assert_eq!(code, 4);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["code"], "unavailable");
    }
}
#[test]
fn invalid_inputs_are_structured_and_redacted() {
    for args in [
        vec!["--password", "TOPSECRET"],
        vec!["integration", "verify"],
        vec!["service", "unknown"],
        vec!["--data-dir", "relative", "doctor", "--json"],
    ] {
        let (code, out, err) = run(&args, None);
        assert_eq!(code, 2);
        assert!(!out.contains("TOPSECRET") && !err.contains("TOPSECRET"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&out).unwrap()["ok"],
            false
        );
    }
}
#[test]
fn dry_run_reports_missing_config_without_sending() {
    let (code, out, _) = run(&["test", "--dry-run"], None);
    assert_eq!(code, 3);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["error"]["code"], "configuration_required");
    assert_eq!(v["data"]["sent"], false);
}
#[test]
fn hook_is_nonblocking_and_emits_no_host_directives() {
    let (code, out, err) = run(
        &[
            "hook",
            "generic",
            "--instance-id",
            "00000000-0000-0000-0000-000000000000",
        ],
        Some(b"SECRET"),
    );
    assert_eq!(code, 0);
    assert!(out.is_empty());
    assert!(!err.contains("SECRET") && !err.contains("continue") && !err.contains("blocker"));
}
#[test]
fn emit_is_bounded_and_not_acknowledged_without_ingress() {
    let (code, out, _) = run(&["emit", "--stdin"], Some(br#"{"schema_version":2}"#));
    assert_eq!(code, 2);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&out).unwrap()["error"]["code"],
        "unsupported_schema"
    );
}
#[test]
fn malformed_hook_never_blocks_host_or_writes_stdout() {
    for args in [
        vec!["hook", "generic", "--instance-id", "bad"],
        vec![
            "hook",
            "unknown",
            "--instance-id",
            "00000000-0000-0000-0000-000000000000",
        ],
        vec!["hook", "generic", "--password", "TOPSECRET"],
    ] {
        let (code, out, err) = run(&args, None);
        assert_eq!(code, 0);
        assert!(out.is_empty());
        assert!(!err.contains("TOPSECRET"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&err).unwrap()["ok"],
            false
        );
    }
}
#[test]
fn all_frozen_commands_parse_before_unavailable_backend() {
    let dir = std::env::temp_dir().join(format!("agentnotify-protocol-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("request.json");
    std::fs::write(&path, br#"{"schema_version":1}"#).unwrap();
    let file = path.to_str().unwrap();
    let out_dir = dir.to_str().unwrap();
    for args in [
        vec!["integration", "plan", "--request", file],
        vec!["integration", "apply", "--plan", file],
        vec!["integration", "verify", "--operation-id", "op1"],
        vec!["integration", "rollback", "--operation-id", "op1"],
        vec!["agent-bundle", "create", "--task", file, "--out", out_dir],
        vec!["service", "install"],
        vec!["service", "start"],
        vec!["service", "stop"],
        vec!["service", "uninstall"],
    ] {
        let (code, out, _) = run(&args, None);
        assert_eq!(code, 4);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&out).unwrap()["error"]["code"],
            "unavailable"
        );
    }
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(dir).unwrap();
}
#[test]
fn emit_read_budget_and_valid_input_do_not_leak_bodies() {
    let oversized = vec![b' '; 1024 * 1024 + 1];
    let (code, out, err) = run(&["emit", "--stdin"], Some(&oversized));
    assert_eq!(code, 2);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&out).unwrap()["error"]["code"],
        "input_too_large"
    );
    assert!(err.is_empty());
    let raw=br#"{"schema_version":1,"adapter_id":"generic","instance_id":"00000000-0000-0000-0000-000000000000","kind":"response_finished","completion_basis":"response_stop","question":"PRIVATE_QUESTION","answer":"PRIVATE_ANSWER"}"#;
    let (code, out, err) = run(&["emit", "--stdin"], Some(raw));
    assert_eq!(code, 4);
    assert!(!out.contains("PRIVATE") && !err.contains("PRIVATE"));
}

#[test]
fn repeated_global_options_never_route_hook_errors_to_stdout() {
    for args in [
        vec![
            "--data-dir",
            "D:/isolated-one",
            "--data-dir",
            "D:/isolated-two",
            "hook",
            "generic",
            "--instance-id",
            "bad",
        ],
        vec![
            "--data-dir",
            "D:/isolated-one",
            "hook",
            "generic",
            "--data-dir",
            "D:/isolated-two",
            "--instance-id",
            "bad",
        ],
        vec![
            "hook",
            "--data-dir",
            "D:/isolated-one",
            "generic",
            "--instance-id",
            "bad",
            "--data-dir",
            "D:/isolated-two",
        ],
    ] {
        let (code, out, err) = run(&args, None);
        assert_eq!(code, 0);
        assert!(out.is_empty());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&err).unwrap()["ok"],
            false
        );
    }
    // 参数值恰好叫hook时仍是doctor错误，不能把目录值误识别为宿主命令。
    let (code, out, _) = run(&["--data-dir", "hook", "doctor", "--json"], None);
    assert_eq!(code, 2);
    assert!(!out.is_empty());
}
