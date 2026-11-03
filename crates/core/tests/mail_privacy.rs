mod support_policy;
use agentnotify_core::{config::ConfigV1, delivery::mail::*, event::*};
use support_policy::*;
fn mail_config() -> MailConfig {
    MailConfig {
        from: "sender@example.invalid".into(),
        recipients: vec!["recipient@example.invalid".into()],
        privacy: ConfigV1::default(),
    }
}
#[test]
fn header_crlf_rejected() {
    let mut e = output("a", None, EventKind::ResponseFinished)
        .notification
        .unwrap();
    e.project_label = Some("x\r\nBcc: attacker".into());
    assert!(prepare_mail(&e, &mail_config()).is_err());
}
#[test]
fn unsafe_scheme_rejected() {
    let mut e = output("a", None, EventKind::ResponseFinished)
        .notification
        .unwrap();
    e.resume_url = Some("javascript:evil()".into());
    assert!(prepare_mail(&e, &mail_config()).is_err());
}
#[test]
fn default_body_absent() {
    let e = body_output("a", None, EventKind::ResponseFinished, "private fixture")
        .notification
        .unwrap();
    let m = prepare_mail(&e, &mail_config()).unwrap();
    m.body
        .with_secret(|b| assert!(!String::from_utf8_lossy(b).contains("private fixture")));
}
#[test]
fn actual_aggregate_count_in_template() {
    let f = Fixture::new();
    f.accept(output("a", None, EventKind::ResponseFinished));
    f.time(1);
    let id = f.accept(output("a", None, EventKind::ResponseFinished));
    f.time(10);
    let l = f.queue.lease_due(f.clock.now(), 1).unwrap().pop().unwrap();
    assert_eq!(id, l.id);
    let m = prepare_queued_mail(&f.queue, &f.policy, &l, &mail_config()).unwrap();
    m.body
        .with_secret(|b| assert!(String::from_utf8_lossy(b).contains("2")));
}
use agentnotify_core::clock::Clock;

#[test]
fn actual_digest_uses_summary_template_not_representative_finish() {
    let mut c = config();
    c.quiet.enabled = true;
    c.quiet.start = "11:00".into();
    c.quiet.end = "12:01".into();
    c.quiet.behavior = agentnotify_core::config::QuietBehavior::Digest;
    let f = Fixture::with_bodies(c);
    for actor in ["a", "b"] {
        f.accept(body_output(
            actor,
            None,
            EventKind::ResponseFinished,
            "private body must not appear",
        ));
    }
    f.time(60);
    assert_eq!(
        f.policy.flush_digests(&f.queue, uuid::Uuid::nil()).unwrap(),
        Some(1)
    );
    let l = f.queue.lease_due(f.clock.now(), 1).unwrap().pop().unwrap();
    let m = prepare_queued_mail(&f.queue, &f.policy, &l, &mail_config()).unwrap();
    assert!(m.subject.contains("摘要"));
    m.body.with_secret(|b| {
        let text = String::from_utf8_lossy(b);
        assert!(text.contains("2 条"));
        assert!(!text.contains("本轮处理结束"));
        assert!(!text.contains("private body"));
    });
}
#[test]
fn encrypted_spool_no_cleartext() {
    use agentnotify_core::{contracts::*, queue::*};
    use std::sync::Arc;
    let root = tempfile::tempdir().unwrap();
    let key =
        CredentialHandle::from_secret(SecretInput::new(uuid::Uuid::new_v4().as_bytes().repeat(2)));
    let protector = Arc::new(AeadBodyProtector::from_key(key).unwrap());
    let mut c = config();
    c.include_answer = true;
    let clock = Arc::new(TestClock(std::sync::Mutex::new(base())));
    struct Security(std::path::PathBuf);
    impl DirectorySecurity for Security {
        fn verify(&self, p: &std::path::Path) -> Result<(), QueueError> {
            if p == self.0 {
                Ok(())
            } else {
                Err(error(QueueErrorCode::StorageUnavailable))
            }
        }
    }
    let policy =
        Arc::new(agentnotify_core::policy::Policy::new(c.clone(), "UTC", snapshot(0)).unwrap());
    let q = SqliteQueue::open(
        &root.path().canonicalize().unwrap(),
        c,
        QueueDependencies {
            clock,
            security: Arc::new(Security(root.path().canonicalize().unwrap())),
            protector: Some(protector.clone()),
            participant: policy,
            faults: Arc::new(NoFaults),
        },
    )
    .unwrap();
    let db = rusqlite::Connection::open(q.database_path()).unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut o = body_output(
        "a",
        None,
        EventKind::ExecutionFailed,
        "PRIVATE_AEAD_FIXTURE_936",
    );
    o.notification.as_mut().unwrap().question = None;
    let id = match q.accept(o).unwrap() {
        EnqueueOutcome::Spooled { id } => id,
        _ => panic!("expected spool"),
    };
    let path = q.spool_path().join(format!("{id}.spool"));
    let files: Vec<_> = std::fs::read_dir(q.spool_path())
        .unwrap()
        .map(|p| p.unwrap().path())
        .collect();
    let path = if path.exists() {
        path
    } else {
        files[0].clone()
    };
    let bytes = std::fs::read(&path).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("PRIVATE_AEAD"));
    assert!(
        q.inspect_spool(&path)
            .unwrap()
            .adapter_output
            .notification
            .unwrap()
            .answer
            .unwrap()
            .contains("PRIVATE_AEAD")
    );
    let sealed = protector.seal(b"plain", b"metadata").unwrap();
    assert!(protector.open(&sealed, b"changed").is_err());
    let other = protector.seal(b"plain", b"metadata").unwrap();
    assert_ne!(sealed.nonce, other.nonce);
    db.execute_batch("ROLLBACK").unwrap();
}
#[test]
fn retention_expires_body_and_prunes_metadata() {
    let f = Fixture::with_bodies(config());
    let id = f.accept(body_output(
        "a",
        None,
        EventKind::ExecutionFailed,
        "private retention fixture",
    ));
    f.time(86401);
    f.queue.maintain_retention(f.clock.now()).unwrap();
    assert_eq!(
        f.status(id),
        agentnotify_core::contracts::QueueStatus::Expired
    );
    let no_body: bool = f
        .db()
        .query_row(
            "SELECT body IS NULL FROM notifications WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(no_body);
    f.time(31 * 86400);
    assert_eq!(f.queue.maintain_retention(f.clock.now()).unwrap(), 1);
    assert_eq!(f.count("ingress"), 0);
}
