#[test]
fn credential_real_roundtrip() {
    agentnotify_feasibility::credential_roundtrip()
        .expect("native credential backend must write/read/delete a random entry");
}
