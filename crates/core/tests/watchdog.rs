mod support_policy;
use agentnotify_core::{config::*, contracts::Ingress, event::*, policy::Policy};
use std::sync::Arc;
use support_policy::*;
use uuid::Uuid;
#[test]
fn long_job_with_progress_not_inactive() {
    let mut c = config();
    c.watchdog.mode = WatchdogMode::Inactivity;
    let f = Fixture::configured(c);
    f.accept(signal("a", Some("t"), LifecycleKind::Started));
    f.time(50);
    f.accept(signal("a", Some("t"), LifecycleKind::Progress));
    f.time(65);
    assert_eq!(f.due(), 0);
    f.time(115);
    assert_eq!(f.due(), 1);
}
#[test]
fn start_end_only_disables_inactivity() {
    let mut c = config();
    c.watchdog.mode = WatchdogMode::Inactivity;
    let f = Fixture::configured(c);
    let mut cp = caps();
    cp.reliable_progress = false;
    f.policy.register_instance(Uuid::nil(), cp);
    f.accept(signal("a", Some("t"), LifecycleKind::Started));
    f.time(100);
    assert_eq!(f.due(), 0);
}
#[test]
fn unknown_actor_no_watchdog() {
    let f = Fixture::new();
    let mut o = signal("a", Some("t"), LifecycleKind::Started);
    o.lifecycle.as_mut().unwrap().actor_scope = ActorScope::Unknown;
    f.accept(o);
    assert_eq!(f.count("activity_state"), 0);
}
#[test]
fn restart_sleep_clock_jump_suspend() {
    for mode in 0..3 {
        let f = Fixture::new();
        f.accept(signal("a", Some("t"), LifecycleKind::Started));
        let mut s = snapshot(100);
        match mode {
            0 => s.boot_id = "next-boot".into(),
            1 => s.resume_epoch = 1,
            _ => s.monotonic = std::time::Duration::from_secs(1),
        };
        f.policy.set_snapshot(s);
        assert_eq!(f.due(), 0);
        let state: String = f
            .db()
            .query_row("SELECT state FROM activity_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(state, "suspended_unknown");
    }
}
#[test]
fn new_progress_resumes_without_alert_storm() {
    let f = Fixture::new();
    f.accept(signal("a", Some("t"), LifecycleKind::Started));
    let p = Arc::new(Policy::new(config(), "UTC", snapshot(100)).unwrap());
    p.register_instance(Uuid::nil(), caps());
    let q = Fixture::open(f.root.path(), config(), f.clock.clone(), p.clone());
    assert_eq!(p.evaluate_due(&q, Uuid::nil()).unwrap(), Some(0));
    *f.clock.0.lock().unwrap() = snapshot(100).utc;
    let mut o = signal("a", Some("t"), LifecycleKind::Progress);
    o.lifecycle.as_mut().unwrap().received_at = snapshot(100).utc;
    q.accept(o).unwrap();
    p.set_snapshot(snapshot(120));
    assert_eq!(p.evaluate_due(&q, Uuid::nil()).unwrap(), Some(0));
    p.set_snapshot(snapshot(165));
    assert_eq!(p.evaluate_due(&q, Uuid::nil()).unwrap(), Some(1));
}
#[test]
fn session_closed_clears_only_its_session() {
    let f = Fixture::new();
    f.accept(signal("a", Some("t"), LifecycleKind::Started));
    let mut other = signal("b", Some("t"), LifecycleKind::Started);
    other.lifecycle.as_mut().unwrap().session_id = "other".into();
    f.accept(other);
    f.accept(signal("a", None, LifecycleKind::SessionClosed));
    f.time(65);
    assert_eq!(f.due(), 1);
}
#[test]
fn stop_timeout_race_atomic() {
    let f = Fixture::new();
    f.accept(signal("a", Some("t"), LifecycleKind::Started));
    f.time(65);
    let q = f.queue.clone();
    let p = f.policy.clone();
    let handle = std::thread::spawn(move || p.evaluate_due(&q, Uuid::nil()).unwrap());
    f.accept(output("a", Some("t"), EventKind::ResponseFinished));
    handle.join().unwrap();
    let count:i64=f.db().query_row("SELECT count(*) FROM notifications n JOIN policy_notifications p ON n.id=p.id WHERE p.kind='stalled' AND n.status IN ('queued','leased','retry_wait')",[],|r|r.get(0)).unwrap();
    assert_eq!(count, 0);
}
#[test]
fn spooled_stop_precedes_timeout() {
    let f = Fixture::new();
    f.accept(signal("a", Some("t"), LifecycleKind::Started));
    let db = f.db();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    f.time(65);
    let mut o = output("a", Some("t"), EventKind::ResponseFinished);
    o.notification.as_mut().unwrap().received_at = snapshot(65).utc;
    assert!(matches!(
        f.queue.accept(o).unwrap(),
        agentnotify_core::contracts::EnqueueOutcome::Spooled { .. }
    ));
    db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(f.policy.evaluate_due(&f.queue, Uuid::nil()).unwrap(), None);
    for _ in 0..20 {
        if f.queue.import_spool().unwrap().remaining == 0 {
            break;
        }
    }
    assert_eq!(f.due(), 0);
}
#[test]
fn one_alert_per_turn_with_stable_id() {
    let f = Fixture::new();
    f.accept(signal("a", Some("t"), LifecycleKind::Started));
    f.time(65);
    assert_eq!(f.due(), 1);
    f.time(80);
    assert_eq!(f.due(), 0);
    f.accept(signal("a", Some("t"), LifecycleKind::Progress));
    f.time(145);
    assert_eq!(f.due(), 0);
    assert_eq!(f.count("notifications"), 1);
}
#[test]
fn default_disabled() {
    let f = Fixture::configured(ConfigV1::default());
    f.accept(signal("a", Some("t"), LifecycleKind::Started));
    f.time(2000);
    assert_eq!(f.due(), 0);
    assert_eq!(ConfigV1::default().watchdog.threshold_seconds, 1800);
    assert_eq!(WatchdogConfig::TICK_SECONDS, 15);
}
#[test]
fn leased_stalled_rechecks_restart_before_data() {
    let f = Fixture::new();
    f.accept(signal("a", Some("t"), LifecycleKind::Started));
    f.time(65);
    f.due();
    let lease = f.queue.lease_due(snapshot(65).utc, 10).unwrap().remove(0);
    let p = Arc::new(Policy::new(config(), "UTC", snapshot(65)).unwrap());
    p.register_instance(Uuid::nil(), caps());
    let q = Fixture::open(f.root.path(), config(), f.clock.clone(), p);
    assert!(
        q.record_send_stage(
            &lease,
            agentnotify_core::contracts::SendStage::Prepared,
            agentnotify_core::contracts::SendStage::DataStarted
        )
        .is_err()
    );
    assert_eq!(
        f.status(lease.id),
        agentnotify_core::contracts::QueueStatus::Suppressed
    );
}
#[test]
fn flushed_stalled_digest_rechecks_progress() {
    let mut c = config();
    c.quiet.enabled = true;
    c.quiet.start = "12:00".into();
    c.quiet.end = "12:02".into();
    c.quiet.behavior = QuietBehavior::Digest;
    let f = Fixture::configured(c);
    f.accept(signal("a", Some("t"), LifecycleKind::Started));
    f.time(65);
    f.due();
    f.time(120);
    assert_eq!(
        f.policy.flush_digests(&f.queue, Uuid::nil()).unwrap(),
        Some(1)
    );
    f.accept(signal("a", Some("t"), LifecycleKind::Progress));
    assert!(f.queue.lease_due(snapshot(120).utc, 10).unwrap().is_empty());
}
#[test]
fn old_spooled_start_on_restart_stays_suspended() {
    let f = Fixture::new();
    let db = f.db();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    f.accept(signal("a", Some("t"), LifecycleKind::Started));
    db.execute_batch("ROLLBACK").unwrap();
    let p = Arc::new(Policy::new(config(), "UTC", snapshot(100)).unwrap());
    p.register_instance(Uuid::nil(), caps());
    let q = Fixture::open(f.root.path(), config(), f.clock.clone(), p.clone());
    for _ in 0..20 {
        if q.import_spool().unwrap().remaining == 0 {
            break;
        }
    }
    p.set_snapshot(snapshot(200));
    assert_eq!(p.evaluate_due(&q, Uuid::nil()).unwrap(), Some(0));
    let state: String = f
        .db()
        .query_row("SELECT state FROM activity_state", [], |r| r.get(0))
        .unwrap();
    assert_eq!(state, "suspended_unknown");
}
#[test]
fn progress_between_lease_and_data_invalidates_token() {
    let f = Fixture::new();
    f.accept(signal("a", Some("t"), LifecycleKind::Started));
    f.time(65);
    f.due();
    let lease = f.queue.lease_due(snapshot(65).utc, 10).unwrap().remove(0);
    f.accept(signal("a", Some("t"), LifecycleKind::Progress));
    assert!(
        f.queue
            .record_send_stage(
                &lease,
                agentnotify_core::contracts::SendStage::Prepared,
                agentnotify_core::contracts::SendStage::DataStarted
            )
            .is_err()
    );
}
#[test]
fn data_started_cannot_be_retracted_by_progress() {
    let f = Fixture::new();
    f.accept(signal("a", Some("t"), LifecycleKind::Started));
    f.time(65);
    f.due();
    let lease = f.queue.lease_due(snapshot(65).utc, 10).unwrap().remove(0);
    f.queue
        .record_send_stage(
            &lease,
            agentnotify_core::contracts::SendStage::Prepared,
            agentnotify_core::contracts::SendStage::DataStarted,
        )
        .unwrap();
    f.accept(signal("a", Some("t"), LifecycleKind::Progress));
    assert_eq!(
        f.status(lease.id),
        agentnotify_core::contracts::QueueStatus::Leased
    );
}
#[test]
fn watchdog_enforces_fifteen_second_tick() {
    let f = Fixture::new();
    f.accept(signal("a", Some("t"), LifecycleKind::Started));
    f.time(59);
    assert_eq!(f.due(), 0);
    f.time(60);
    assert_eq!(f.due(), 0);
    f.time(74);
    assert_eq!(f.due(), 1);
}
#[test]
fn replayed_old_progress_does_not_confirm_new_runtime() {
    let f = Fixture::new();
    let db = f.db();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    f.accept(signal("a", Some("t"), LifecycleKind::Started));
    f.time(1);
    f.accept(signal("a", Some("t"), LifecycleKind::Progress));
    db.execute_batch("ROLLBACK").unwrap();
    let p = Arc::new(Policy::new(config(), "UTC", snapshot(100)).unwrap());
    p.register_instance(Uuid::nil(), caps());
    let q = Fixture::open(f.root.path(), config(), f.clock.clone(), p.clone());
    for _ in 0..30 {
        if q.import_spool().unwrap().remaining == 0 {
            break;
        }
    }
    p.set_snapshot(snapshot(200));
    assert_eq!(p.evaluate_due(&q, Uuid::nil()).unwrap(), Some(0));
}
#[test]
fn replay_context_handles_backward_wall_clock() {
    let f = Fixture::new();
    f.time(100);
    let db = f.db();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    f.accept(signal("a", Some("t"), LifecycleKind::Started));
    f.accept(signal("a", Some("t"), LifecycleKind::Progress));
    db.execute_batch("ROLLBACK").unwrap();
    let p = Arc::new(Policy::new(config(), "UTC", snapshot(0)).unwrap());
    p.register_instance(Uuid::nil(), caps());
    let q = Fixture::open(f.root.path(), config(), f.clock.clone(), p.clone());
    for _ in 0..30 {
        if q.import_spool().unwrap().remaining == 0 {
            break;
        }
    }
    p.set_snapshot(snapshot(65));
    assert_eq!(p.evaluate_due(&q, Uuid::nil()).unwrap(), Some(0));
}
#[test]
fn subagent_user_rule_applies_to_watchdog() {
    let f = Fixture::new();
    let mut o = signal("child", Some("t"), LifecycleKind::Started);
    o.source_evidence.source_class = SourceClass::Subagent;
    o.source_evidence.origin_evidence = OriginEvidence::VerifiedAdapter;
    f.accept(o);
    let mut c = config();
    c.policy.notify_subagents = false;
    f.policy.set_config(c).unwrap();
    f.time(65);
    f.due();
    assert!(f.queue.lease_due(snapshot(65).utc, 10).unwrap().is_empty());
}

#[test]
fn fix1_historical_started_cannot_replace_current_turn_after_reopen() {
    let f = Fixture::new();
    f.accept(signal("a", Some("t1"), LifecycleKind::Started));
    f.time(1);
    f.accept(signal("a", Some("t2"), LifecycleKind::Started));
    f.time(66);
    assert_eq!(f.due(), 1);
    let current_before: (u64, String, bool) = f
        .db()
        .query_row(
            "SELECT generation,current_turn,running FROM actor_state",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    let activity_before: (String, i64, i64) = f
        .db()
        .query_row(
            "SELECT state,revision,alerted FROM activity_state WHERE turn_key='t:t2'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    let alert: String = f
        .db()
        .query_row(
            "SELECT id FROM policy_notifications WHERE kind='stalled'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let fresh = Arc::new(Policy::new(config(), "UTC", snapshot(66)).unwrap());
    fresh.register_instance(Uuid::nil(), caps());
    let reopened = Fixture::open(f.root.path(), config(), f.clock.clone(), fresh);
    let mut duplicate = signal("a", Some("t1"), LifecycleKind::Started);
    duplicate.lifecycle.as_mut().unwrap().received_at = snapshot(66).utc;
    reopened.accept(duplicate).unwrap();
    let current_after: (u64, String, bool) = f
        .db()
        .query_row(
            "SELECT generation,current_turn,running FROM actor_state",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(current_after, current_before);
    let activity_after: (String, i64, i64) = f
        .db()
        .query_row(
            "SELECT state,revision,alerted FROM activity_state WHERE turn_key='t:t2'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(activity_after, activity_before);
    assert_eq!(
        f.status(Uuid::parse_str(&alert).unwrap()),
        agentnotify_core::contracts::QueueStatus::Queued
    );
}
#[test]
fn fix1_historical_started_is_idempotent_when_watchdog_disabled() {
    let mut c = config();
    c.watchdog.enabled = false;
    let f = Fixture::configured(c.clone());
    f.accept(signal("a", Some("t1"), LifecycleKind::Started));
    f.accept(signal("a", Some("t2"), LifecycleKind::Started));
    let fresh = Arc::new(Policy::new(c.clone(), "UTC", snapshot(0)).unwrap());
    fresh.register_instance(Uuid::nil(), caps());
    let reopened = Fixture::open(f.root.path(), c, f.clock.clone(), fresh);
    reopened
        .accept(signal("a", Some("t1"), LifecycleKind::Started))
        .unwrap();
    let row: (u64, String) = f
        .db()
        .query_row("SELECT generation,current_turn FROM actor_state", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(row, (2, "t:t2".into()));
    assert_eq!(f.count("activity_state"), 0);
}
