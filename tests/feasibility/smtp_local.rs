#[test]
fn smtp_local_tls_probe() {
    agentnotify_feasibility::smtp_tls_probe()
        .expect("trusted CA succeeds; untrusted CA and STARTTLS downgrade fail closed");
}

#[test]
fn smtp_data_stage_observable() {
    agentnotify_feasibility::smtp_stage_probe()
        .expect("durable DATA marker must precede DATA and survive ambiguous result");
}
