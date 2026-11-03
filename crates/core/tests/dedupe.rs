mod support_policy;
use agentnotify_core::{contracts::QueueStatus, event::*};
use support_policy::*;
#[test]
fn generation_idempotent_start() {
    let f = Fixture::new();
    let s = signal("a", None, LifecycleKind::Started);
    f.accept(s.clone());
    f.accept(s);
    let g: i64 = f
        .db()
        .query_row("SELECT generation FROM actor_state", [], |r| r.get(0))
        .unwrap();
    assert_eq!(g, 1);
    assert_eq!(f.count("activity_state"), 1);
}
#[test]
fn known_actor_missing_turn_fallback_ten_seconds() {
    let f = Fixture::with_bodies(config());
    f.accept(body_output(
        "a",
        None,
        EventKind::ResponseFinished,
        "same fixture answer",
    ));
    f.time(9);
    let b = f.accept(body_output(
        "a",
        None,
        EventKind::ResponseFinished,
        "same fixture answer",
    ));
    assert_eq!(f.status(b), QueueStatus::Suppressed);
    f.time(11);
    let c = f.accept(body_output(
        "a",
        None,
        EventKind::ResponseFinished,
        "same fixture answer",
    ));
    assert_eq!(f.status(c), QueueStatus::Queued);
}
#[test]
fn unknown_actor_only_stable_event_id_deduped() {
    let f = Fixture::new();
    let mut o = output("a", Some("t"), EventKind::ResponseFinished);
    o.notification.as_mut().unwrap().actor_scope = ActorScope::Unknown;
    o.notification.as_mut().unwrap().actor_id = None;
    let a = f.accept(o.clone());
    let b = f.accept(o.clone());
    assert_ne!(a, b);
    assert_eq!(f.status(a), QueueStatus::Queued);
    assert_eq!(f.status(b), QueueStatus::Queued);
    o.notification.as_mut().unwrap().event_id = Some("stable".into());
    assert_eq!(f.accept(o.clone()), f.accept(o));
}
#[test]
fn parent_progress_cannot_cancel_child() {
    let f = Fixture::new();
    let c = f.accept(output("child", Some("t"), EventKind::ResponseFinished));
    f.accept(signal("parent", Some("t"), LifecycleKind::Progress));
    assert_eq!(f.status(c), QueueStatus::Queued);
}
#[test]
fn siblings_do_not_merge() {
    let f = Fixture::new();
    let a = f.accept(output("a", Some("t"), EventKind::ResponseFinished));
    let b = f.accept(output("b", Some("t"), EventKind::ResponseFinished));
    assert_eq!(f.status(a), QueueStatus::Queued);
    assert_eq!(f.status(b), QueueStatus::Queued);
}

#[test]
fn fix1_default_privacy_different_content_is_not_deduped() {
    for kind in [EventKind::ResponseFinished, EventKind::ExecutionFailed] {
        let mut c = config();
        c.policy.aggregate_finishes = false;
        let f = Fixture::configured(c);
        let first = f.accept(body_output(
            "a",
            None,
            kind,
            "first different fixture answer",
        ));
        f.time(1);
        let second = f.accept(body_output(
            "a",
            None,
            kind,
            "second different fixture answer",
        ));
        assert_eq!(f.status(first), QueueStatus::Queued);
        assert_eq!(
            f.status(second),
            QueueStatus::Queued,
            "missing bodies are not equality evidence: {kind:?}"
        );
        let retained: i64 = f
            .db()
            .query_row(
                "SELECT count(*) FROM notifications WHERE body IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(retained, 0);
        let raw: Vec<u8> = f
            .db()
            .query_row(
                "SELECT metadata FROM ingress WHERE id=?1",
                [second.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        let e: IngressEnvelopeV1 = serde_json::from_slice(&raw).unwrap();
        let n = e.adapter_output.notification.unwrap();
        assert_eq!(n.question, None);
        assert_eq!(n.answer, None);
    }
}
