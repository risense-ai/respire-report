mod support_queue;
use agentnotify_core::{contracts::*, queue::*};
use std::sync::Arc;
use support_queue::*;
#[test]
fn spool_full_is_error() {
    let f = Fixture::new();
    f.queue.set_maintenance(true).unwrap();
    let path = f.queue.spool_path().join("quota.quarantine");
    std::fs::write(path, vec![0; MAX_SPOOL_BYTES]).unwrap();
    assert_eq!(
        f.queue.accept(f.output("full")).unwrap_err().code,
        QueueErrorCode::SpoolFull
    );
    assert_eq!(f.count("ingress"), 0);
}
#[test]
fn disk_full_never_acknowledged() {
    for point in [
        FaultPoint::BeforeSpoolTemp,
        FaultPoint::AfterSpoolWrite,
        FaultPoint::AfterSpoolSync,
        FaultPoint::BeforeSpoolRename,
    ] {
        let f = Fixture::configured(Default::default(), FailOnce::at(point));
        f.queue.set_maintenance(true).unwrap();
        assert!(f.queue.accept(f.output("disk")).is_err(), "{point:?}");
        assert_eq!(f.count("ingress"), 0);
        assert_eq!(f.queue.pending_spool().unwrap(), 0);
    }
}
#[test]
fn sequence_faults_degrade_to_emergency_spool() {
    for p in [
        FaultPoint::BeforeSequenceWrite,
        FaultPoint::AfterSequenceWrite,
        FaultPoint::AfterSequenceSync,
        FaultPoint::BeforeSequenceRename,
        FaultPoint::AfterSequenceRename,
    ] {
        let f = Fixture::configured(Default::default(), FailOnce::at(p));
        assert!(matches!(
            f.accept("emergency"),
            EnqueueOutcome::Spooled { .. }
        ));
        f.queue.import_spool().unwrap();
        assert_eq!(f.count("notifications"), 1);
        assert_eq!(
            f.db()
                .query_row(
                    "SELECT count(*) FROM ingress WHERE order_known=0",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
    }
}
#[test]
fn symlink_or_shared_directory_rejected() {
    let root = tempfile::tempdir().unwrap();
    assert!(SystemDirectorySecurity.verify(root.path()).is_err());
    let child = root.path().join("unowned");
    std::fs::create_dir(&child).unwrap();
    assert!(
        IsolatedDirectory(root.path().canonicalize().unwrap())
            .verify(&child)
            .is_err()
    );
    #[cfg(windows)]
    {
        let link = root.path().join("junction");
        let output = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&child)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(reject_links(&link).is_err());
    }
}
#[test]
fn sealed_body_is_binary_bounded_and_authenticated() {
    let mut cfg = agentnotify_core::config::ConfigV1::default();
    cfg.include_question = true;
    cfg.include_answer = true;
    let f = Fixture::configured(cfg, Arc::new(NoFaults));
    f.queue.set_maintenance(true).unwrap();
    let mut o = f.output("body");
    let e = o.notification.as_mut().unwrap();
    e.question = Some("Q_PRIVATE_TEST".repeat(1500));
    e.answer = Some("A_PRIVATE_TEST".repeat(1500));
    e.content_key_available = true;
    f.queue.accept(o).unwrap();
    let entry = std::fs::read_dir(f.queue.spool_path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut bytes = std::fs::read(&entry).unwrap();
    assert!(bytes.len() <= MAX_ITEM_BYTES);
    assert!(!String::from_utf8_lossy(&bytes).contains("PRIVATE_TEST"));
    let restored = f.queue.inspect_spool(&entry).unwrap();
    assert!(
        restored
            .adapter_output
            .notification
            .unwrap()
            .question
            .unwrap()
            .len()
            <= 16384
    );
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    std::fs::write(&entry, bytes).unwrap();
    f.queue.set_maintenance(false).unwrap();
    let r = f.queue.import_spool().unwrap();
    assert_eq!(r.quarantined, 1);
    assert_eq!(f.count("notifications"), 0);
}
#[test]
fn malformed_and_unknown_spool_are_quarantined() {
    let f = Fixture::new();
    std::fs::write(f.queue.spool_path().join("broken.spool"), b"unknown").unwrap();
    let r = f.queue.import_spool().unwrap();
    assert_eq!(r.quarantined, 1);
    assert_eq!(f.count("ingress"), 0);
}
#[test]
fn terminal_state_clears_ciphertext() {
    let mut cfg = agentnotify_core::config::ConfigV1::default();
    cfg.include_answer = true;
    let f = Fixture::configured(cfg, Arc::new(NoFaults));
    let mut o = f.output("body");
    o.notification.as_mut().unwrap().answer = Some("PRIVATE".into());
    o.notification.as_mut().unwrap().content_key_available = true;
    let id = id(f.queue.accept(o).unwrap());
    assert!(
        f.db()
            .query_row("SELECT body IS NOT NULL FROM notifications", [], |r| r
                .get::<_, bool>(0))
            .unwrap()
    );
    f.queue
        .transition(id, QueueStatus::Queued, QueueStatus::Suppressed)
        .unwrap();
    assert!(
        f.db()
            .query_row("SELECT body IS NULL FROM notifications", [], |r| r
                .get::<_, bool>(0))
            .unwrap()
    );
}
#[test]
fn after_rename_failure_retains_only_durable_spool_for_recovery() {
    let f = Fixture::configured(
        Default::default(),
        FailOnce::at(FaultPoint::AfterSpoolRename),
    );
    f.queue.set_maintenance(true).unwrap();
    assert!(f.queue.accept(f.output("durable")).is_err());
    assert_eq!(f.queue.pending_spool().unwrap(), 1);
    f.queue.set_maintenance(false).unwrap();
    f.drain();
    assert_eq!(f.count("notifications"), 1);
}
#[test]
fn no_body_protector_does_not_persist_or_ack_body() {
    let f = Fixture::new();
    let path = f.root.path().canonicalize().unwrap();
    let mut cfg = agentnotify_core::config::ConfigV1::default();
    cfg.include_answer = true;
    let q = SqliteQueue::open(
        &path,
        cfg,
        QueueDependencies {
            clock: f.clock.clone(),
            security: Arc::new(IsolatedDirectory(path.clone())),
            protector: None,
            participant: Arc::new(FixtureParticipant),
            faults: Arc::new(NoFaults),
        },
    )
    .unwrap();
    let mut o = f.output("body");
    o.notification.as_mut().unwrap().answer = Some("NO_PLAINTEXT_FALLBACK".into());
    o.notification.as_mut().unwrap().content_key_available = true;
    assert_eq!(
        q.accept(o).unwrap_err().code,
        QueueErrorCode::BodyProtectionUnavailable
    );
    assert_eq!(f.count("ingress"), 0);
    assert_eq!(f.queue.pending_spool().unwrap(), 0);
}
#[test]
fn framing_handles_worst_case_utf8_without_json_ciphertext_expansion() {
    let mut cfg = agentnotify_core::config::ConfigV1::default();
    cfg.include_question = true;
    cfg.include_answer = true;
    let f = Fixture::configured(cfg, Arc::new(NoFaults));
    f.queue.set_maintenance(true).unwrap();
    let mut o = f.output("escape");
    let n = o.notification.as_mut().unwrap();
    n.question = Some("\0".repeat(16384));
    n.answer = Some("密".repeat(5461));
    n.content_key_available = true;
    f.queue.accept(o).unwrap();
    let path = std::fs::read_dir(f.queue.spool_path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(std::fs::metadata(&path).unwrap().len() < 44000);
    let e = f.queue.inspect_spool(&path).unwrap();
    assert_eq!(
        e.adapter_output
            .notification
            .unwrap()
            .question
            .unwrap()
            .len(),
        16384
    );
}
