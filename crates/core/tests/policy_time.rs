mod support_policy;
use agentnotify_core::{contracts::QueueStatus, event::*, policy::quiet};
use support_policy::*;
#[test]
fn quiet_equal_means_all_day() {
    let mut c = config();
    c.quiet.enabled = true;
    c.quiet.start = "07:00".into();
    c.quiet.end = "07:00".into();
    assert!(quiet::interval(&c.quiet, base(), "UTC").unwrap().is_some());
}
#[test]
fn dst_overlap_digest_once() {
    let mut c = config();
    c.quiet.enabled = true;
    c.quiet.timezone = "America/New_York".into();
    c.quiet.start = "01:00".into();
    c.quiet.end = "02:30".into();
    let a = quiet::interval(&c.quiet, at("2026-11-01T05:30:00Z"), "UTC")
        .unwrap()
        .unwrap();
    let b = quiet::interval(&c.quiet, at("2026-11-01T06:30:00Z"), "UTC")
        .unwrap()
        .unwrap();
    assert_eq!(a.key, b.key);
    assert_eq!(a.end, at("2026-11-01T07:30:00Z"));
}
#[test]
fn dst_gap_next_valid_instant() {
    let mut c = config();
    c.quiet.enabled = true;
    c.quiet.timezone = "America/New_York".into();
    c.quiet.start = "23:00".into();
    c.quiet.end = "02:30".into();
    let i = quiet::interval(&c.quiet, at("2026-03-08T06:30:00Z"), "UTC")
        .unwrap()
        .unwrap();
    assert_eq!(i.end, at("2026-03-08T07:00:00Z"));
}
#[test]
fn quiet_receive_or_send_suppresses() {
    let mut c = config();
    c.quiet.enabled = true;
    c.quiet.start = "11:00".into();
    c.quiet.end = "13:00".into();
    let f = Fixture::configured(c);
    let id = f.accept(output("a", None, EventKind::ResponseFinished));
    assert_eq!(f.status(id), QueueStatus::Suppressed);
    f.time(7200);
    assert!(
        f.queue
            .lease_due(snapshot(7200).utc, 10)
            .unwrap()
            .is_empty()
    );
}
#[test]
fn system_zone_change_rechecks_pending() {
    let mut c = config();
    c.quiet.enabled = true;
    let f = Fixture::configured(c);
    let id = f.accept(output("a", None, EventKind::ResponseFinished));
    f.time(11);
    f.policy.set_system_timezone("Pacific/Auckland").unwrap();
    assert!(f.queue.lease_due(snapshot(11).utc, 10).unwrap().is_empty());
    assert_eq!(f.status(id), QueueStatus::Suppressed);
}
#[test]
fn stop_progress_cancels() {
    let f = Fixture::new();
    f.accept(signal("a", Some("t"), LifecycleKind::Started));
    let id = f.accept(output("a", Some("t"), EventKind::ResponseFinished));
    f.time(2);
    f.accept(signal("a", Some("t"), LifecycleKind::Progress));
    assert_eq!(f.status(id), QueueStatus::Suppressed);
}
#[test]
fn aggregate_wait_max_three_and_ten() {
    let f = Fixture::new();
    let a = f.accept(output("a", Some("1"), EventKind::ResponseFinished));
    f.time(9);
    let b = f.accept(output("a", Some("2"), EventKind::ResponseFinished));
    assert_eq!(f.status(a), QueueStatus::Suppressed);
    assert!(f.queue.lease_due(snapshot(11).utc, 10).unwrap().is_empty());
    assert_eq!(f.queue.lease_due(snapshot(12).utc, 10).unwrap()[0].id, b);
}
#[test]
fn retry_respects_global_ten_per_minute() {
    let f = Fixture::new();
    for n in 0..11 {
        f.accept(output(&format!("a{n}"), None, EventKind::ExecutionFailed));
    }
    let leases = f.queue.lease_due(base(), 20).unwrap();
    assert_eq!(leases.len(), 10);
    f.queue.retry_temporary(&leases[0], base(), 0).unwrap();
    f.time(30);
    assert!(f.queue.lease_due(snapshot(30).utc, 20).unwrap().is_empty());
    assert!(f.count("policy_digests") >= 1);
}
#[test]
fn digest_counts_only_once_and_expires_with_members() {
    let mut c = config();
    c.quiet.enabled = true;
    c.quiet.start = "11:00".into();
    c.quiet.end = "12:01".into();
    c.quiet.behavior = agentnotify_core::config::QuietBehavior::Digest;
    let f = Fixture::configured(c);
    let source = f.accept(output("a", Some("t"), EventKind::ResponseFinished));
    f.time(60);
    assert_eq!(
        f.policy.flush_digests(&f.queue, uuid::Uuid::nil()).unwrap(),
        Some(1)
    );
    assert_eq!(
        f.policy.flush_digests(&f.queue, uuid::Uuid::nil()).unwrap(),
        Some(0)
    );
    let lease = f.queue.lease_due(snapshot(60).utc, 10).unwrap().remove(0);
    let summary = f
        .policy
        .delivery_summary(&f.queue, lease.id)
        .unwrap()
        .unwrap();
    assert_eq!(summary.count, 1);
    assert_eq!(summary.labels.len(), 2);
    let (source_expiry,digest_expiry):(i64,i64)=f.db().query_row("SELECT (SELECT expires_at FROM notifications WHERE id=?1),(SELECT expires_at FROM notifications WHERE id=?2)",rusqlite::params![source.to_string(),lease.id.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(source_expiry, digest_expiry);
}
#[test]
fn aggregation_disabled_still_waits_three_seconds() {
    let mut c = config();
    c.policy.aggregate_finishes = false;
    let f = Fixture::configured(c);
    f.accept(output("a", None, EventKind::ResponseFinished));
    assert!(f.queue.lease_due(snapshot(2).utc, 10).unwrap().is_empty());
    assert_eq!(f.queue.lease_due(snapshot(3).utc, 10).unwrap().len(), 1);
}
#[test]
fn global_rate_overflow_materializes_one_minute_digest() {
    let f = Fixture::new();
    for n in 0..12 {
        f.accept(output(&format!("a{n}"), None, EventKind::ExecutionFailed));
    }
    assert_eq!(f.queue.lease_due(base(), 20).unwrap().len(), 10);
    f.time(60);
    assert_eq!(
        f.policy.flush_digests(&f.queue, uuid::Uuid::nil()).unwrap(),
        Some(1)
    );
    let lease = f.queue.lease_due(snapshot(60).utc, 20).unwrap().remove(0);
    assert_eq!(
        f.policy
            .delivery_summary(&f.queue, lease.id)
            .unwrap()
            .unwrap()
            .count,
        2
    );
}

#[test]
fn fix1_initial_quiet_digest_never_stores_body() {
    let mut c = config();
    c.quiet.enabled = true;
    c.quiet.start = "11:00".into();
    c.quiet.end = "13:00".into();
    c.quiet.behavior = agentnotify_core::config::QuietBehavior::Digest;
    let f = Fixture::with_bodies(c);
    let id = f.accept(body_output(
        "a",
        Some("t"),
        EventKind::ResponseFinished,
        "private fixture answer",
    ));
    assert_eq!(f.status(id), QueueStatus::DigestPending);
    let empty: bool = f
        .db()
        .query_row(
            "SELECT body IS NULL FROM notifications WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(empty, "initial digest must discard ciphertext too");
}
#[test]
fn fix1_transition_into_digest_clears_existing_body() {
    let f = Fixture::with_bodies(config());
    let id = f.accept(body_output(
        "a",
        Some("t"),
        EventKind::ResponseFinished,
        "private fixture answer",
    ));
    let had_body: bool = f
        .db()
        .query_row(
            "SELECT body IS NOT NULL FROM notifications WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(had_body);
    f.queue
        .transition(id, QueueStatus::Queued, QueueStatus::DigestPending)
        .unwrap();
    let empty: bool = f
        .db()
        .query_row(
            "SELECT body IS NULL FROM notifications WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        empty,
        "administrative transition must also clear ciphertext"
    );
}
#[test]
fn fix1_send_time_quiet_digest_clears_existing_body() {
    let f = Fixture::with_bodies(config());
    let id = f.accept(body_output(
        "a",
        Some("t"),
        EventKind::ResponseFinished,
        "private fixture answer",
    ));
    let mut c = config();
    c.quiet.enabled = true;
    c.quiet.start = "11:00".into();
    c.quiet.end = "13:00".into();
    c.quiet.behavior = agentnotify_core::config::QuietBehavior::Digest;
    f.policy.set_config(c).unwrap();
    f.time(10);
    assert!(f.queue.lease_due(snapshot(10).utc, 10).unwrap().is_empty());
    assert_eq!(f.status(id), QueueStatus::DigestPending);
    let empty: bool = f
        .db()
        .query_row(
            "SELECT body IS NULL FROM notifications WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(empty);
}
