mod support_policy;
use agentnotify_core::{contracts::QueueStatus, event::*};
use support_policy::*;
#[test]
fn unknown_evidence_retained() {
    let f = Fixture::new();
    assert_eq!(
        f.policy
            .classify_output(&output("a", None, EventKind::ResponseFinished)),
        QueueStatus::Queued
    );
}
#[test]
fn internal_success_requires_verified_evidence() {
    let f = Fixture::new();
    let mut o = output("a", None, EventKind::ResponseFinished);
    o.source_evidence.source_class = SourceClass::InternalBackground;
    o.source_evidence.background_result = BackgroundResult::Success;
    assert_eq!(f.policy.classify_output(&o), QueueStatus::Queued);
    o.source_evidence.origin_evidence = OriginEvidence::VerifiedAdapter;
    assert_eq!(f.policy.classify_output(&o), QueueStatus::Suppressed);
    o.notification.as_mut().unwrap().kind = EventKind::ExecutionFailed;
    assert_eq!(f.policy.classify_output(&o), QueueStatus::Queued);
}
#[test]
fn subagent_not_blanket_filtered() {
    let f = Fixture::new();
    let mut o = output("child", None, EventKind::ResponseFinished);
    o.source_evidence.source_class = SourceClass::Subagent;
    o.notification.as_mut().unwrap().answer = Some("{\"facts\":[],\"result\":\"success\"}".into());
    assert_eq!(f.policy.classify_output(&o), QueueStatus::Queued);
}
#[test]
fn caller_reported_cannot_suppress() {
    let f = Fixture::new();
    let mut o = output("a", None, EventKind::ResponseFinished);
    o.source_evidence.source_class = SourceClass::InternalBackground;
    o.source_evidence.background_result = BackgroundResult::Success;
    o.source_evidence.origin_evidence = OriginEvidence::CallerReported;
    assert_eq!(f.policy.classify_output(&o), QueueStatus::Queued);
}
#[test]
fn pure_internal_started_has_evidence_and_no_watchdog() {
    let f = Fixture::new();
    let mut o = signal("a", Some("t"), LifecycleKind::Started);
    o.source_evidence.source_class = SourceClass::InternalBackground;
    o.source_evidence.origin_evidence = OriginEvidence::VerifiedAdapter;
    f.accept(o);
    assert_eq!(f.count("activity_state"), 0);
}
#[test]
fn shared_policy_contract_is_the_callable_decision_api() {
    let f = Fixture::new();
    let o = output("a", Some("t"), EventKind::ResponseFinished);
    let p: &dyn agentnotify_core::contracts::Policy = &*f.policy;
    assert!(
        matches!(p.decide(o.notification.as_ref().unwrap(),&o.source_evidence,&config(),base()),agentnotify_core::contracts::PolicyDecision::Notify{not_before} if not_before==snapshot(10).utc)
    );
}
