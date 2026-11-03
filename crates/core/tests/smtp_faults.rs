mod support_policy;
mod support_smtp;
use agentnotify_core::{
    clock::Clock,
    config::*,
    contracts::*,
    delivery::{mail::*, smtp::*},
    event::*,
};
use support_policy::{Fixture, output, signal};
use support_smtp::*;
fn run(
    scenario: Scenario,
    fail: Option<SendStage>,
    starttls: bool,
    untrusted: bool,
) -> (DeliveryOutcome, std::sync::Arc<std::sync::Mutex<Seen>>) {
    let server = Server::start(scenario, starttls, None);
    let journal = Journal::new(fail);
    let id = uuid::Uuid::new_v4();
    let tls = if untrusted {
        public_tls()
    } else {
        server.tls.clone()
    };
    let outcome = SmtpTransport::new(server.config(starttls), tls, &journal, id)
        .send(&mail(id), &credentials());
    (outcome, server.finish())
}
#[test]
fn smtp_5xx_permanent() {
    assert!(matches!(
        run(Scenario::Permanent, None, false, false).0,
        DeliveryOutcome::PermanentFailure { .. }
    ));
}
#[test]
fn auth_blocks_until_repaired() {
    assert!(matches!(
        run(Scenario::Auth, None, false, false).0,
        DeliveryOutcome::AuthBlocked { .. }
    ));
}
#[test]
fn plaintext_auth_forbidden() {
    assert!(SmtpConfig::new("localhost", 25, "user", "plain").is_err());
    let (_, s) = run(Scenario::NoStartTls, None, true, false);
    assert!(!s.lock().unwrap().auth);
}
#[test]
fn invalid_certificate_fails() {
    let (o, s) = run(Scenario::Ok, None, false, true);
    assert!(matches!(o, DeliveryOutcome::PermanentFailure { .. }));
    assert!(!s.lock().unwrap().auth);
}
#[test]
fn journal_failure_prevents_data() {
    let (o, s) = run(Scenario::Ok, Some(SendStage::DataStarted), false, false);
    assert!(matches!(o, DeliveryOutcome::Retryable { .. }));
    assert!(!s.lock().unwrap().data);
}
#[test]
fn data_disconnect_unknown_no_retry() {
    assert!(matches!(
        run(Scenario::Disconnect, None, false, false).0,
        DeliveryOutcome::Uncertain { .. }
    ));
    assert!(matches!(
        run(Scenario::HalfReply, None, false, false).0,
        DeliveryOutcome::Uncertain { .. }
    ));
}
#[test]
fn accepted_record_failure_unknown() {
    assert!(matches!(
        run(Scenario::Ok, Some(SendStage::Accepted), false, false).0,
        DeliveryOutcome::Uncertain { .. }
    ));
}
#[test]
fn starttls_success_and_message_id_not_dedup_guarantee() {
    let (o, s) = run(Scenario::Ok, None, true, false);
    assert!(matches!(o, DeliveryOutcome::Accepted { .. }));
    assert!(s.lock().unwrap().body);
}
fn config() -> MailConfig {
    MailConfig {
        from: "sender@example.invalid".into(),
        recipients: vec!["recipient@example.invalid".into()],
        privacy: ConfigV1::default(),
    }
}
fn send_real(
    f: &Fixture,
    lease: &agentnotify_core::queue::Lease,
    scenario: Scenario,
) -> (DeliveryOutcome, bool) {
    send_with_hook(f, lease, scenario, None)
}
struct HookJournal<'a> {
    inner: QueueSendJournal<'a>,
    before_data: Option<Box<dyn Fn() + 'a>>,
}
impl SendJournal for HookJournal<'_> {
    fn record(&self, id: &str, s: SendStage) -> Result<(), JournalError> {
        if s == SendStage::DataStarted {
            if let Some(f) = &self.before_data {
                f()
            }
        }
        self.inner.record(id, s)
    }
    fn rejected(&self, id: &str, c: u16) -> Result<(), JournalError> {
        self.inner.rejected(id, c)
    }
    fn refresh_mail(&self) -> Result<Option<PreparedMail>, JournalError> {
        self.inner.refresh_mail()
    }
}
fn send_with_hook<'a>(
    f: &'a Fixture,
    lease: &'a agentnotify_core::queue::Lease,
    scenario: Scenario,
    before_data: Option<Box<dyn Fn() + 'a>>,
) -> (DeliveryOutcome, bool) {
    let queue = f.queue.clone();
    let id = lease.id;
    let server = Server::start(
        scenario,
        false,
        Some(Box::new(move || {
            let db = rusqlite::Connection::open(queue.database_path()).unwrap();
            let stage: String = db
                .query_row(
                    "SELECT send_stage FROM notifications WHERE id=?1",
                    [id.to_string()],
                    |r| r.get(0),
                )
                .unwrap();
            stage == "data_started"
        })),
    );
    let cfg = config();
    let journal = HookJournal {
        inner: QueueSendJournal::new(&f.queue, lease, &f.policy, &cfg),
        before_data,
    };
    let m = prepare_queued_mail(&f.queue, &f.policy, lease, &cfg).unwrap();
    let outcome = SmtpTransport::new(server.config(false), server.tls.clone(), &journal, lease.id)
        .send(&m, &credentials());
    let seen = server.finish();
    let seen = seen.lock().unwrap();
    if seen.data {
        assert!(seen.durable)
    }
    (outcome, seen.data)
}
#[test]
fn smtp_4xx_backoff_bounded() {
    let f = Fixture::new();
    let id = f.accept(output("a", None, EventKind::ExecutionFailed));
    let mut t = 0;
    for attempt in 0..5 {
        f.time(t);
        let l = f.queue.lease_due(f.clock.now(), 1).unwrap().pop().unwrap();
        let (o, _) = send_real(&f, &l, Scenario::DataTemporary);
        assert!(matches!(o, DeliveryOutcome::Retryable { .. }));
        if attempt < 4 {
            assert_eq!(f.status(id), QueueStatus::RetryWait);
            let due: i64 = f
                .db()
                .query_row(
                    "SELECT due_at FROM notifications WHERE id=?1",
                    [id.to_string()],
                    |r| r.get(0),
                )
                .unwrap();
            let delay = due - f.clock.now().timestamp_millis();
            let expected = [30, 120, 600, 1800][attempt] * 1000;
            assert!(delay >= expected && delay <= expected * 120 / 100);
            t += delay / 1000 + 1;
        } else {
            assert_eq!(f.status(id), QueueStatus::PermanentFailed)
        }
    }
    assert!(f.queue.lease_due(f.clock.now(), 1).unwrap().is_empty());
}
#[test]
fn definite_data_5xx_permanent_and_terminal_body_removed() {
    let f = Fixture::with_bodies(support_policy::config());
    let id = f.accept(support_policy::body_output(
        "a",
        None,
        EventKind::ExecutionFailed,
        "sensitive fixture",
    ));
    let l = f.queue.lease_due(f.clock.now(), 1).unwrap().pop().unwrap();
    let (o, saw) = send_real(&f, &l, Scenario::DataPermanent);
    assert!(saw && matches!(o, DeliveryOutcome::PermanentFailure { .. }));
    assert_eq!(f.status(id), QueueStatus::PermanentFailed);
    let absent: bool = f
        .db()
        .query_row(
            "SELECT body IS NULL FROM notifications WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(absent);
}
#[test]
fn crash_after_data_unknown() {
    let f = Fixture::new();
    let id = f.accept(output("a", None, EventKind::ExecutionFailed));
    let l = f.queue.lease_due(f.clock.now(), 1).unwrap().pop().unwrap();
    let (o, _) = send_real(&f, &l, Scenario::Disconnect);
    assert!(matches!(o, DeliveryOutcome::Uncertain { .. }));
    f.time(61);
    f.queue.recover_leases(f.clock.now()).unwrap();
    assert_eq!(f.status(id), QueueStatus::DeliveryUnknown);
    assert!(f.queue.lease_due(f.clock.now(), 1).unwrap().is_empty());
}
#[test]
fn stalled_send_rechecks_activity_revision() {
    let f = Fixture::new();
    f.accept(signal("a", Some("t"), LifecycleKind::Started));
    f.time(60);
    assert_eq!(f.due(), 1);
    let l = f.queue.lease_due(f.clock.now(), 1).unwrap().pop().unwrap();
    let (_, saw) = send_with_hook(
        &f,
        &l,
        Scenario::Ok,
        Some(Box::new(|| {
            f.time(61);
            f.accept(signal("a", Some("t"), LifecycleKind::Progress));
        })),
    );
    assert!(!saw);
    assert_eq!(f.status(l.id), QueueStatus::Suppressed);
}
#[test]
fn stalled_uses_same_quiet_gate() {
    let f = Fixture::new();
    f.accept(signal("a", Some("t"), LifecycleKind::Started));
    f.time(60);
    f.due();
    let l = f.queue.lease_due(f.clock.now(), 1).unwrap().pop().unwrap();
    let mut c = support_policy::config();
    c.quiet.enabled = true;
    c.quiet.start = "00:00".into();
    c.quiet.end = "23:59".into();
    f.policy.set_config(c).unwrap();
    let (_, saw) = send_real(&f, &l, Scenario::Ok);
    assert!(!saw);
    assert_eq!(f.status(l.id), QueueStatus::Suppressed);
}

#[test]
fn stalled_uses_same_ttl_and_rate_limit() {
    let f = Fixture::new();
    for n in 0..11 {
        f.accept(signal(
            &format!("actor{n}"),
            Some("t"),
            LifecycleKind::Started,
        ));
    }
    f.time(60);
    assert_eq!(f.due(), 11);
    assert_eq!(f.queue.lease_due(f.clock.now(), 20).unwrap().len(), 10);
    let digest: i64 = f
        .db()
        .query_row(
            "SELECT count(*) FROM notifications WHERE status='digest_pending'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(digest, 1);
    let f = Fixture::new();
    f.accept(signal("a", Some("t"), LifecycleKind::Started));
    f.time(60);
    f.due();
    let lease = f.queue.lease_due(f.clock.now(), 1).unwrap().pop().unwrap();
    let (_, saw) = send_with_hook(
        &f,
        &lease,
        Scenario::Ok,
        Some(Box::new(|| {
            f.time(60 + 86401);
            f.queue.maintain_retention(f.clock.now()).unwrap();
        })),
    );
    assert!(!saw);
    assert_eq!(f.status(lease.id), QueueStatus::Expired);
}
#[test]
fn missing_secure_store_blocks_without_plaintext_fallback() {
    struct Missing;
    impl CredentialStore for Missing {
        fn get(&self, _: &CredentialRef) -> Result<CredentialHandle, CredentialError> {
            Err(CredentialError {
                code: CredentialErrorCode::Locked,
            })
        }
        fn put(&self, _: &CredentialRef, _: SecretInput) -> Result<(), CredentialError> {
            panic!("no fallback save")
        }
        fn delete(&self, _: &CredentialRef) -> Result<(), CredentialError> {
            panic!("no credential reset")
        }
    }
    let f = Fixture::new();
    let id = f.accept(output("a", None, EventKind::ExecutionFailed));
    let lease = f.queue.lease_due(f.clock.now(), 1).unwrap().pop().unwrap();
    let smtp = SmtpConfig::new(
        "never-connect.invalid",
        465,
        "sender@example.invalid",
        "tls",
    )
    .unwrap();
    let o = deliver_lease(
        &f.queue,
        &f.policy,
        &lease,
        &config(),
        smtp,
        &Missing,
        &CredentialRef {
            id: uuid::Uuid::new_v4(),
        },
        &*f.clock,
    )
    .unwrap();
    assert!(matches!(o, DeliveryOutcome::AuthBlocked { .. }));
    assert_eq!(f.status(id), QueueStatus::AuthBlocked);
    assert!(f.queue.lease_due(f.clock.now(), 1).unwrap().is_empty());
    f.queue
        .transition(id, QueueStatus::AuthBlocked, QueueStatus::Queued)
        .unwrap();
    assert_eq!(f.queue.lease_due(f.clock.now(), 1).unwrap().len(), 1);
}
