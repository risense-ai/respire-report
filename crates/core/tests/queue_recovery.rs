mod support_queue;
use agentnotify_core::{contracts::*, event::*, queue::*};
use support_queue::*;

use chrono::Duration;
use uuid::Uuid;
#[test]
fn concurrent_enqueue_single_row() {
    let f = Fixture::new();
    let outcomes = std::thread::scope(|scope| {
        let mut joins = Vec::new();
        for _ in 0..8 {
            let q = f.queue.clone();
            let o = f.output("same");
            joins.push(scope.spawn(move || q.accept(o)));
        }
        joins
            .into_iter()
            .map(|j| j.join().unwrap())
            .collect::<Vec<_>>()
    });
    // admission竞争超过预算时明确拒绝；重试同一稳定事件也不能产生第二行。
    assert!(outcomes.iter().any(Result::is_ok));
    for result in outcomes {
        if let Err(err) = result {
            assert_eq!(err.code, QueueErrorCode::AdmissionTimeout);
            f.queue.accept(f.output("same")).unwrap();
        }
    }
    assert_eq!(f.count("notifications"), 1);
    assert_eq!(f.count("ingress"), 1);
}
#[test]
fn separate_instances_not_deduped() {
    let f = Fixture::new();
    f.accept("same");
    f.queue.accept(output(Uuid::new_v4(), "same")).unwrap();
    assert_eq!(f.count("notifications"), 2);
}
#[test]
fn locked_db_falls_back_to_spool() {
    let f = Fixture::new();
    let db = f.db();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let start = std::time::Instant::now();
    assert!(matches!(f.accept("locked"), EnqueueOutcome::Spooled { .. }));
    assert!(start.elapsed() >= std::time::Duration::from_millis(900));
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
    db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(f.drain().imported, 1);
    assert_eq!(f.count("notifications"), 1);
}
#[test]
fn spool_backlog_prevents_db_overtake() {
    let f = Fixture::new();
    f.queue.set_maintenance(true).unwrap();
    f.accept("first");
    f.queue.set_maintenance(false).unwrap();
    assert!(matches!(f.accept("second"), EnqueueOutcome::Spooled { .. }));
    assert_eq!(f.count("notifications"), 0);
    let r = f.drain();
    assert_eq!(r.imported, 2);
    assert_eq!(f.count("notifications"), 2);
    assert!(matches!(f.accept("third"), EnqueueOutcome::Queued { .. }));
}
#[test]
fn lifecycle_and_event_atomic() {
    let f = Fixture::configured(Default::default(), FailOnce::at(FaultPoint::BeforeCommit));
    let mut out = f.output("event");
    out.lifecycle = Some(LifecycleSignal {
        instance_id: Uuid::nil(),
        session_id: "s".into(),
        actor_id: None,
        actor_scope: ActorScope::Unknown,
        turn_id: None,
        upstream_id: None,
        kind: LifecycleKind::Started,
        received_at: now(),
    });
    assert!(matches!(
        f.queue.accept(out).unwrap(),
        EnqueueOutcome::Spooled { .. }
    ));
    assert_eq!(f.count("ingress"), 0);
    assert_eq!(f.count("notifications"), 0);
    assert_eq!(f.drain().imported, 1);
    assert_eq!(f.count("fixture_observed"), 1);
    assert_eq!(f.count("lifecycle"), 1);
}
#[test]
fn locked_started_stop_replay_order() {
    let f = Fixture::new();
    let db = f.db();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut out = f.output("start");
    out.notification = None;
    out.lifecycle = Some(LifecycleSignal {
        instance_id: Uuid::nil(),
        session_id: "s".into(),
        actor_id: None,
        actor_scope: ActorScope::Main,
        turn_id: None,
        upstream_id: Some("start".into()),
        kind: LifecycleKind::Started,
        received_at: now(),
    });
    f.queue.accept(out).unwrap();
    f.accept("stop");
    db.execute_batch("ROLLBACK").unwrap();
    f.drain();
    let kinds: Vec<String> = f
        .db()
        .prepare("SELECT kind FROM fixture_observed ORDER BY rowid")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(kinds, ["Started", "event"]);
}
#[test]
fn gap_degrades_without_infinite_wait() {
    let f = Fixture::configured(
        Default::default(),
        FailOnce::at(FaultPoint::AfterSequenceRename),
    );
    f.accept("unknown");
    f.accept("gap");
    f.drain();
    assert!(
        f.db()
            .query_row(
                "SELECT count(*) FROM diagnostics WHERE code IN ('order_unknown','sequence_gap')",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap()
            >= 1
    );
    assert_eq!(f.count("notifications"), 2);
}
#[test]
fn unknown_order_never_cancels_other_event() {
    let f = Fixture::new();
    for id in ["a", "b"] {
        let e = IngressEnvelopeV1 {
            presence_suppressed: false,
            schema_version: 1,
            ingress_id: Uuid::new_v4(),
            received_at: now(),
            per_instance_sequence: None,
            adapter_output: f.output(id),
        };
        f.queue.ingest(e).unwrap();
    }
    assert_eq!(f.count("notifications"), 2);
    assert_eq!(
        f.db()
            .query_row(
                "SELECT count(*) FROM fixture_observed WHERE ordered=0",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
}
#[test]
fn concurrent_spool_import_does_not_double_apply() {
    let f = Fixture::new();
    f.queue.set_maintenance(true).unwrap();
    f.accept("one");
    f.queue.set_maintenance(false).unwrap();
    std::thread::scope(|s| {
        for _ in 0..3 {
            let q = &f.queue;
            s.spawn(move || q.import_spool().unwrap());
        }
    });
    assert_eq!(f.count("ingress"), 1);
    assert_eq!(f.count("fixture_observed"), 1);
}
#[test]
fn lease_recovery_respects_send_stage() {
    let f = Fixture::new();
    for name in ["pre", "data", "accepted"] {
        f.accept(name);
    }
    let leases = f.queue.lease_due(now(), 10).unwrap();
    assert_eq!(leases.len(), 3);
    f.queue
        .record_send_stage(&leases[1], SendStage::Prepared, SendStage::DataStarted)
        .unwrap();
    f.queue
        .record_send_stage(&leases[2], SendStage::Prepared, SendStage::DataStarted)
        .unwrap();
    f.queue
        .record_send_stage(&leases[2], SendStage::DataStarted, SendStage::Accepted)
        .unwrap();
    f.queue
        .recover_leases(now() + Duration::seconds(61))
        .unwrap();
    assert_eq!(
        f.queue.notification_status(leases[0].id).unwrap(),
        QueueStatus::Queued
    );
    assert_eq!(
        f.queue.notification_status(leases[1].id).unwrap(),
        QueueStatus::DeliveryUnknown
    );
    assert_eq!(
        f.queue.notification_status(leases[2].id).unwrap(),
        QueueStatus::Sent
    );
}
#[test]
fn state_transition_matrix() {
    use QueueStatus::*;
    let all = [
        Queued,
        Leased,
        Sent,
        Suppressed,
        DigestPending,
        RetryWait,
        AuthBlocked,
        DeliveryUnknown,
        Expired,
        PermanentFailed,
    ];
    for from in all {
        for to in all {
            let expected = matches!(
                (from, to),
                (
                    Queued,
                    Leased | Suppressed | DigestPending | Expired | PermanentFailed
                ) | (
                    Leased,
                    Sent | RetryWait | AuthBlocked | DeliveryUnknown | Expired | PermanentFailed
                ) | (DigestPending, Queued | Suppressed | Expired)
                    | (RetryWait, Leased | Suppressed | Expired | PermanentFailed)
                    | (AuthBlocked, Queued | Expired)
                    | (DeliveryUnknown, Queued)
            );
            assert_eq!(allowed_transition(from, to), expected, "{from:?}->{to:?}");
        }
    }
    let f = Fixture::new();
    let id = id(f.accept("ttl"));
    assert!(f.queue.transition(id, Queued, Sent).is_err());
    *f.clock.0.lock().unwrap() = now() + Duration::hours(24);
    assert_eq!(
        f.queue
            .lease_due(now() + Duration::hours(24), 10)
            .unwrap()
            .len(),
        0
    );
    assert_eq!(f.queue.notification_status(id).unwrap(), Expired);
}
#[test]
fn auth_repair_never_crosses_ttl() {
    let f = Fixture::new();
    let id = id(f.accept("auth"));
    f.queue.lease_due(now(), 1).unwrap();
    f.queue
        .transition(id, QueueStatus::Leased, QueueStatus::AuthBlocked)
        .unwrap();
    *f.clock.0.lock().unwrap() = now() + Duration::hours(25);
    f.queue
        .transition(id, QueueStatus::AuthBlocked, QueueStatus::Queued)
        .unwrap();
    assert_eq!(
        f.queue.notification_status(id).unwrap(),
        QueueStatus::Expired
    );
}
#[test]
fn import_crash_after_commit_is_idempotent() {
    let f = Fixture::configured(
        Default::default(),
        FailOnce::at(FaultPoint::AfterImportCommit),
    );
    f.queue.set_maintenance(true).unwrap();
    f.accept("one");
    f.queue.set_maintenance(false).unwrap();
    assert!(f.queue.import_spool().is_err());
    assert_eq!(f.count("notifications"), 1);
    f.drain();
    assert_eq!(f.count("notifications"), 1);
    assert_eq!(f.count("fixture_observed"), 1);
}
#[test]
fn transaction_faults_leave_no_partial_lifecycle_or_checkpoint() {
    for point in [
        FaultPoint::BeforeLifecycle,
        FaultPoint::AfterLifecycle,
        FaultPoint::AfterNotification,
        FaultPoint::BeforeCheckpoint,
        FaultPoint::BeforeCommit,
        FaultPoint::BeforeImportCommit,
    ] {
        let f = Fixture::configured(Default::default(), FailOnce::at(point));
        f.queue.set_maintenance(true).unwrap();
        f.accept("atomic");
        f.queue.set_maintenance(false).unwrap();
        assert!(f.queue.import_spool().is_err(), "{point:?}");
        assert_eq!(f.count("ingress"), 0);
        assert_eq!(f.count("notifications"), 0);
        assert_eq!(f.count("checkpoints"), 0);
        f.drain();
        assert_eq!(f.count("fixture_observed"), 1);
    }
}
#[test]
fn sqlite_wal_unique_index_and_restart_are_real() {
    let f = Fixture::new();
    let id = id(f.accept("persist"));
    assert_eq!(
        f.db()
            .query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    let path = f.root.path().canonicalize().unwrap();
    let reopened = SqliteQueue::open(
        &path,
        Default::default(),
        QueueDependencies {
            clock: f.clock.clone(),
            security: std::sync::Arc::new(IsolatedDirectory(path.clone())),
            protector: Some(std::sync::Arc::new(FixtureAead)),
            participant: std::sync::Arc::new(FixtureParticipant),
            faults: std::sync::Arc::new(NoFaults),
        },
    )
    .unwrap();
    assert_eq!(
        reopened.notification_status(id).unwrap(),
        QueueStatus::Queued
    );
    assert!(matches!(
        reopened.accept(f.output("persist")).unwrap(),
        EnqueueOutcome::Duplicate { .. }
    ));
    assert_eq!(f.count("notifications"), 1);
    f.accept("later");
    assert_eq!(
        f.db()
            .query_row("SELECT count(*) FROM diagnostics", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn late_started_never_rewrites_existing_order_or_identity() {
    let f = Fixture::new();
    f.accept("first");
    let before: Vec<u8> = f
        .db()
        .query_row("SELECT metadata FROM ingress", [], |r| r.get(0))
        .unwrap();
    let mut out = f.output("late");
    out.notification = None;
    out.lifecycle = Some(LifecycleSignal {
        instance_id: Uuid::nil(),
        session_id: "s".into(),
        actor_id: None,
        actor_scope: ActorScope::Main,
        turn_id: None,
        upstream_id: Some("late".into()),
        kind: LifecycleKind::Started,
        received_at: now(),
    });
    let envelope = IngressEnvelopeV1 {
        presence_suppressed: false,
        schema_version: 1,
        ingress_id: Uuid::new_v4(),
        received_at: now(),
        per_instance_sequence: Some(1),
        adapter_output: out,
    };
    f.queue.ingest(envelope.clone()).unwrap();
    f.queue.ingest(envelope).unwrap();
    assert_eq!(f.count("fixture_observed"), 2);
    assert_eq!(f.count("lifecycle"), 1);
    assert_eq!(
        before,
        f.db()
            .query_row(
                "SELECT metadata FROM ingress ORDER BY rowid LIMIT 1",
                [],
                |r| r.get::<_, Vec<u8>>(0)
            )
            .unwrap()
    );
    assert_eq!(
        f.db()
            .query_row(
                "SELECT ordered FROM fixture_observed WHERE kind='Started'",
                [],
                |r| r.get::<_, bool>(0)
            )
            .unwrap(),
        false
    );
}
#[test]
fn retries_are_bounded_jittered_and_data_never_retried() {
    let f = Fixture::new();
    let id = id(f.accept("retry"));
    let base = now();
    for (i, delay) in [30, 120, 600, 1800].into_iter().enumerate() {
        let current = *f.clock.0.lock().unwrap();
        let lease = f.queue.lease_due(current, 1).unwrap().pop().unwrap();
        assert_eq!(lease.id, id);
        f.queue.retry_temporary(&lease, current, 20).unwrap();
        let due: i64 = f
            .db()
            .query_row("SELECT due_at FROM notifications", [], |r| r.get(0))
            .unwrap();
        assert_eq!(due, current.timestamp_millis() + delay * 1200);
        *f.clock.0.lock().unwrap() = chrono::DateTime::from_timestamp_millis(due).unwrap();
        assert_eq!(
            f.queue.notification_status(id).unwrap(),
            QueueStatus::RetryWait,
            "attempt {i}"
        );
    }
    let lease = f
        .queue
        .lease_due(*f.clock.0.lock().unwrap(), 1)
        .unwrap()
        .pop()
        .unwrap();
    f.queue
        .retry_temporary(&lease, *f.clock.0.lock().unwrap(), 0)
        .unwrap();
    assert_eq!(
        f.queue.notification_status(id).unwrap(),
        QueueStatus::PermanentFailed
    );
    assert!((*f.clock.0.lock().unwrap() - base).num_hours() < 24);
    let f = Fixture::new();
    f.accept("data");
    let lease = f.queue.lease_due(now(), 1).unwrap().pop().unwrap();
    f.queue
        .record_send_stage(&lease, SendStage::Prepared, SendStage::DataStarted)
        .unwrap();
    f.queue.retry_temporary(&lease, now(), 0).unwrap();
    assert_eq!(
        f.queue.notification_status(lease.id).unwrap(),
        QueueStatus::DeliveryUnknown
    );
}
#[test]
fn expired_worker_is_fenced_after_recovery_and_releasing() {
    let f = Fixture::new();
    f.accept("worker");
    let first = f.queue.lease_due(now(), 1).unwrap().pop().unwrap();
    let future = now() + Duration::seconds(61);
    *f.clock.0.lock().unwrap() = future;
    f.queue.recover_leases(future).unwrap();
    let second = f.queue.lease_due(future, 1).unwrap().pop().unwrap();
    assert!(f.queue.complete_lease(&first, QueueStatus::Sent).is_err());
    assert_eq!(
        f.queue.notification_status(second.id).unwrap(),
        QueueStatus::Leased
    );
}
#[test]
fn sender_exclusivity_is_os_lock_backed() {
    let f = Fixture::new();
    let guard = f.queue.acquire_sender().unwrap();
    assert!(f.queue.acquire_sender().is_err());
    drop(guard);
    assert!(f.queue.acquire_sender().is_ok());
}
#[test]
fn newer_database_schema_fails_closed() {
    let f = Fixture::new();
    f.db().pragma_update(None, "user_version", 99).unwrap();
    let path = f.root.path().canonicalize().unwrap();
    let result = SqliteQueue::open(
        &path,
        Default::default(),
        QueueDependencies {
            clock: f.clock.clone(),
            security: std::sync::Arc::new(IsolatedDirectory(path.clone())),
            protector: None,
            participant: std::sync::Arc::new(FixtureParticipant),
            faults: std::sync::Arc::new(NoFaults),
        },
    );
    assert!(matches!(
        result,
        Err(QueueError {
            code: QueueErrorCode::UnsupportedSchema
        })
    ));
}

#[test]
fn import_replay_keeps_ordered_checkpoint_and_admission_timeout_is_bounded() {
    let f = Fixture::configured(
        Default::default(),
        FailOnce::at(FaultPoint::AfterImportCommit),
    );
    f.queue.set_maintenance(true).unwrap();
    f.accept("once");
    f.queue.set_maintenance(false).unwrap();
    assert!(f.queue.import_spool().is_err());
    f.drain();
    assert!(
        f.db()
            .query_row("SELECT ordered FROM checkpoints", [], |r| r
                .get::<_, bool>(0))
            .unwrap()
    );
    let gate = AdmissionGate::new(
        f.root.path().canonicalize().unwrap(),
        std::sync::Arc::new(IsolatedDirectory(f.root.path().canonicalize().unwrap())),
    );
    let _guard = gate.enter().unwrap();
    let start = std::time::Instant::now();
    assert_eq!(
        f.queue.accept(f.output("blocked")).unwrap_err().code,
        QueueErrorCode::AdmissionTimeout
    );
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
}
#[test]
fn explicit_transition_cannot_retry_after_data_started() {
    let f = Fixture::new();
    let id = id(f.accept("unsafe"));
    let lease = f.queue.lease_due(now(), 1).unwrap().pop().unwrap();
    f.queue
        .record_send_stage(&lease, SendStage::Prepared, SendStage::DataStarted)
        .unwrap();
    f.queue
        .transition(id, QueueStatus::Leased, QueueStatus::RetryWait)
        .unwrap();
    assert_eq!(
        f.queue.notification_status(id).unwrap(),
        QueueStatus::DeliveryUnknown
    );
}
#[test]
fn watchdog_transaction_waits_for_earlier_spool() {
    let f = Fixture::new();
    f.queue.set_maintenance(true).unwrap();
    f.accept("stop");
    f.queue.set_maintenance(false).unwrap();
    let work = |tx: &rusqlite::Transaction<'_>| {
        tx.execute_batch("CREATE TABLE due_fixture (n INTEGER)")
            .unwrap();
        Ok(7)
    };
    assert_eq!(
        f.queue
            .with_instance_transaction(Uuid::nil(), work)
            .unwrap(),
        None
    );
    f.drain();
    assert_eq!(
        f.queue
            .with_instance_transaction(Uuid::nil(), work)
            .unwrap(),
        Some(7)
    );
}
#[test]
fn child_process_ingress() {
    let Ok(path) = std::env::var("AGENTNOTIFY_ISOLATED_QUEUE_TEST") else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    let path = path.canonicalize().unwrap();
    let queue = SqliteQueue::open(
        &path,
        Default::default(),
        QueueDependencies {
            clock: std::sync::Arc::new(TestClock(std::sync::Mutex::new(now()))),
            security: std::sync::Arc::new(IsolatedDirectory(path.clone())),
            protector: None,
            participant: std::sync::Arc::new(FixtureParticipant),
            faults: std::sync::Arc::new(NoFaults),
        },
    )
    .unwrap();
    queue
        .accept(output(Uuid::nil(), "process-stable-event"))
        .unwrap();
}
#[test]
fn separate_processes_share_admission_and_idempotence() {
    let f = Fixture::new();
    let executable = std::env::current_exe().unwrap();
    let mut children = Vec::new();
    for _ in 0..4 {
        children.push(
            std::process::Command::new(&executable)
                .args(["--exact", "child_process_ingress"])
                .env("AGENTNOTIFY_ISOLATED_QUEUE_TEST", f.root.path())
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    assert_eq!(f.count("ingress"), 1);
    assert_eq!(f.count("notifications"), 1);
    assert_eq!(f.count("fixture_observed"), 1);
}

#[test]
fn ttl_is_never_extended_beyond_twenty_four_hours() {
    let mut cfg = agentnotify_core::config::ConfigV1::default();
    cfg.queue_ttl_hours = 48;
    let f = Fixture::configured(cfg, std::sync::Arc::new(NoFaults));
    let id = id(f.accept("ttl-cap"));
    *f.clock.0.lock().unwrap() = now() + Duration::hours(24);
    assert!(
        f.queue
            .lease_due(*f.clock.0.lock().unwrap(), 1)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        f.queue.notification_status(id).unwrap(),
        QueueStatus::Expired
    );
}

#[test]
fn transition_to_leased_has_recoverable_expiry() {
    let f = Fixture::new();
    let id = id(f.accept("logical-lease"));
    f.queue
        .transition(id, QueueStatus::Queued, QueueStatus::Leased)
        .unwrap();
    assert_eq!(
        f.queue
            .recover_leases(now() + Duration::seconds(61))
            .unwrap(),
        1
    );
    assert_eq!(
        f.queue.notification_status(id).unwrap(),
        QueueStatus::Queued
    );
}
#[test]
fn every_status_transition_is_persisted_or_rejected() {
    use QueueStatus::*;
    let states = [
        Queued,
        Leased,
        Sent,
        Suppressed,
        DigestPending,
        RetryWait,
        AuthBlocked,
        DeliveryUnknown,
        Expired,
        PermanentFailed,
    ];
    let f = Fixture::new();
    let id = id(f.accept("matrix"));
    // Rows are an independent specification table: each column names an allowed destination.
    let allowed: &[&[QueueStatus]] = &[
        &[Leased, Suppressed, DigestPending, Expired, PermanentFailed],
        &[
            Sent,
            RetryWait,
            AuthBlocked,
            DeliveryUnknown,
            Expired,
            PermanentFailed,
        ],
        &[],
        &[],
        &[Queued, Suppressed, Expired],
        &[Leased, Suppressed, Expired, PermanentFailed],
        &[Queued, Expired],
        &[Queued],
        &[],
        &[],
    ];
    for (index, from) in states.into_iter().enumerate() {
        for to in states {
            let status = serde_json::to_value(from)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned();
            let stage = if from == Leased && to == Sent {
                "accepted"
            } else {
                "prepared"
            };
            f.db()
                .execute(
                    "UPDATE notifications SET status=?1,send_stage=?2",
                    rusqlite::params![status, stage],
                )
                .unwrap();
            let result = f.queue.transition(id, from, to);
            assert_eq!(
                result.is_ok(),
                allowed[index].contains(&to),
                "{from:?}->{to:?}"
            );
            assert_eq!(
                f.queue.notification_status(id).unwrap(),
                if result.is_ok() { to } else { from }
            );
        }
    }
}

#[test]
fn round1_unknown_sequence_degrades_following_events_before_any_participant() {
    for concurrent in [false, true] {
        let f = Fixture::configured(
            Default::default(),
            FailOnce::at(FaultPoint::BeforeSequenceRename),
        );
        f.accept("unknown-a");
        f.accept("following-b");
        f.accept("following-c");
        if concurrent {
            // 独立队列句柄具有不同扫描游标，不靠同一进程内import Mutex保证排序安全。
            let path = f.root.path().canonicalize().unwrap();
            let queues = (0..3)
                .map(|_| {
                    SqliteQueue::open(
                        &path,
                        Default::default(),
                        QueueDependencies {
                            clock: f.clock.clone(),
                            security: std::sync::Arc::new(IsolatedDirectory(path.clone())),
                            protector: None,
                            participant: std::sync::Arc::new(FixtureParticipant),
                            faults: std::sync::Arc::new(NoFaults),
                        },
                    )
                    .unwrap()
                })
                .collect::<Vec<_>>();
            std::thread::scope(|scope| {
                for q in queues {
                    scope.spawn(move || q.import_spool().unwrap());
                }
            });
        }
        f.drain();
        assert_eq!(f.count("fixture_observed"), 3);
        assert_eq!(
            f.db()
                .query_row(
                    "SELECT count(*) FROM fixture_observed WHERE ordered=1",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0,
            "later lifecycle was applied with false order evidence, concurrent={concurrent}"
        );
        assert!(
            !f.db()
                .query_row("SELECT ordered FROM checkpoints", [], |r| r
                    .get::<_, bool>(0))
                .unwrap()
        );
    }
}
#[test]
fn round1_initialized_queue_opens_under_real_sqlite_writer_lock() {
    let f = Fixture::new();
    let db = f.db();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let other = f.db();
    other
        .busy_timeout(std::time::Duration::from_millis(5))
        .unwrap();
    let err = other.pragma_update(None, "user_version", 2).unwrap_err();
    assert!(
        matches!(err,rusqlite::Error::SqliteFailure(code,_) if code.code==rusqlite::ErrorCode::DatabaseBusy),
        "even an unchanged user_version requires the locked writer slot"
    );
    let path = f.root.path().canonicalize().unwrap();
    let reopened = SqliteQueue::open(
        &path,
        Default::default(),
        QueueDependencies {
            clock: f.clock.clone(),
            security: std::sync::Arc::new(IsolatedDirectory(path.clone())),
            protector: None,
            participant: std::sync::Arc::new(FixtureParticipant),
            faults: std::sync::Arc::new(NoFaults),
        },
    )
    .expect("an initialized reader must not attempt schema writes");
    assert!(matches!(
        reopened.accept(f.output("new-hook")).unwrap(),
        EnqueueOutcome::Spooled { .. }
    ));
    db.execute_batch("ROLLBACK").unwrap();
    f.drain();
    f.drain();
    assert_eq!(f.count("notifications"), 1);
    assert_eq!(f.count("fixture_observed"), 1);
}
#[test]
fn round1_new_process_spools_under_existing_sqlite_writer_lock() {
    let f = Fixture::new();
    let db = f.db();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_process_ingress"])
        .env("AGENTNOTIFY_ISOLATED_QUEUE_TEST", f.root.path())
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    db.execute_batch("ROLLBACK").unwrap();
    assert!(status.success());
    assert_eq!(f.queue.pending_spool().unwrap(), 1);
    f.drain();
    f.drain();
    assert_eq!(f.count("notifications"), 1);
    assert_eq!(f.count("fixture_observed"), 1);
}
#[test]
fn round1_import_admission_wait_uses_only_remaining_batch_budget() {
    let f = Fixture::new();
    f.queue.set_maintenance(true).unwrap();
    f.accept("blocked-import");
    f.queue.set_maintenance(false).unwrap();
    let path = f.root.path().canonicalize().unwrap();
    let gate = AdmissionGate::new(path.clone(), std::sync::Arc::new(IsolatedDirectory(path)));
    let guard = gate.enter().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let q = f.queue.clone();
    let worker = std::thread::spawn(move || send.send(q.import_spool()).unwrap());
    let result = receive.recv_timeout(std::time::Duration::from_millis(250));
    drop(guard);
    worker.join().unwrap();
    let report=result.expect("importer must return its backlog while admission remains locked, not wait the Hook's 700ms").unwrap();
    assert_eq!(report.imported, 0);
    assert!(report.remaining > 0);
    assert_eq!(f.count("notifications"), 0);
    f.drain();
    assert_eq!(f.count("notifications"), 1);
}
#[test]
fn round1_import_scan_deadline_resumes_without_starving_legal_backlog() {
    struct SlowAead;
    impl BodyProtector for SlowAead {
        fn seal(&self, bytes: &[u8], aad: &[u8]) -> Result<SealedBody, QueueError> {
            FixtureAead.seal(bytes, aad)
        }
        fn open(&self, body: &SealedBody, aad: &[u8]) -> Result<Vec<u8>, QueueError> {
            std::thread::sleep(std::time::Duration::from_millis(60));
            FixtureAead.open(body, aad)
        }
    }
    let mut cfg = agentnotify_core::config::ConfigV1::default();
    cfg.include_answer = true;
    let f = Fixture::configured(cfg.clone(), FailOnce::at(FaultPoint::BeforeSequenceRename));
    f.queue.set_maintenance(true).unwrap();
    for index in 0..8 {
        let mut o = f.output(&format!("slow-{index}"));
        let n = o.notification.as_mut().unwrap();
        n.answer = Some("isolated fixture body".into());
        n.content_key_available = true;
        f.queue.accept(o).unwrap();
    }
    f.queue.set_maintenance(false).unwrap();
    let path = f.root.path().canonicalize().unwrap();
    let q = SqliteQueue::open(
        &path,
        cfg,
        QueueDependencies {
            clock: f.clock.clone(),
            security: std::sync::Arc::new(IsolatedDirectory(path.clone())),
            protector: Some(std::sync::Arc::new(SlowAead)),
            participant: std::sync::Arc::new(FixtureParticipant),
            faults: std::sync::Arc::new(NoFaults),
        },
    )
    .unwrap();
    let start = std::time::Instant::now();
    let first = q.import_spool().unwrap();
    assert!(
        start.elapsed() < std::time::Duration::from_millis(220),
        "deadline must include scan/decryption, not start after all eight slow opens"
    );
    assert_eq!(first.imported, 0);
    assert!(first.remaining > 0);
    let mut drained = false;
    for _ in 0..30 {
        let r = q.import_spool().unwrap();
        if r.remaining == 0 {
            drained = true;
            break;
        }
    }
    assert!(
        drained,
        "restarting the entire scan every batch would starve this valid spool"
    );
    assert_eq!(f.count("notifications"), 8);
    assert_eq!(f.count("fixture_observed"), 8);
    assert_eq!(
        f.db()
            .query_row(
                "SELECT count(*) FROM fixture_observed WHERE ordered=1",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0,
        "unfinished scanning must never expose later events as Ordered"
    );
}
