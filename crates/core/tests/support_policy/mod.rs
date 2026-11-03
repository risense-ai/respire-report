#![allow(dead_code)]
use agentnotify_core::contracts::Ingress;
#[path = "../support_queue/mod.rs"]
mod queue_fixture;
use agentnotify_core::{
    clock::{Clock, ClockSnapshot},
    config::*,
    event::*,
    policy::Policy,
    queue::*,
};
use chrono::{DateTime, Duration as Delta, Utc};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;
pub fn at(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}
pub fn base() -> DateTime<Utc> {
    at("2026-10-30T12:00:00Z")
}
pub fn snapshot(t: i64) -> ClockSnapshot {
    ClockSnapshot {
        utc: base() + Delta::seconds(t),
        monotonic: Duration::from_secs(t as u64),
        boot_id: "fixture-boot".into(),
        resume_epoch: 0,
    }
}
pub fn config() -> ConfigV1 {
    let mut c = ConfigV1::default();
    c.quiet.enabled = false;
    c.watchdog.enabled = true;
    c.watchdog.threshold_seconds = 60;
    c
}
pub fn caps() -> AdapterCapabilities {
    AdapterCapabilities {
        reliable_started: true,
        reliable_progress: true,
        reliable_actor: true,
        reliable_turn_or_generation: true,
    }
}
pub fn evidence() -> SourceEvidence {
    SourceEvidence {
        source_class: SourceClass::Unknown,
        origin_evidence: OriginEvidence::Unknown,
        background_result: BackgroundResult::Unknown,
        classification_reason: "neutral_fixture_v1".into(),
    }
}
pub fn output(actor: &str, turn: Option<&str>, kind: EventKind) -> AdapterOutput {
    let mut event: NormalizedEventV1 = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/events/neutral-finish.json"
    ))
    .unwrap();
    event.actor_id = Some(actor.into());
    event.actor_scope = ActorScope::Identified;
    event.turn_id = turn.map(str::to_owned);
    event.kind = kind;
    event.completion_basis = if kind == EventKind::ResponseFinished {
        CompletionBasis::ResponseStop
    } else {
        CompletionBasis::None
    };
    AdapterOutput {
        notification: Some(event),
        lifecycle: None,
        source_evidence: evidence(),
    }
}
pub fn signal(actor: &str, turn: Option<&str>, kind: LifecycleKind) -> AdapterOutput {
    AdapterOutput {
        notification: None,
        lifecycle: Some(LifecycleSignal {
            instance_id: Uuid::nil(),
            session_id: "s".into(),
            actor_id: Some(actor.into()),
            actor_scope: ActorScope::Identified,
            turn_id: turn.map(str::to_owned),
            upstream_id: None,
            kind,
            received_at: base(),
        }),
        source_evidence: evidence(),
    }
}
pub struct TestClock(pub Mutex<DateTime<Utc>>);
impl Clock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
    fn monotonic(&self) -> Duration {
        Duration::ZERO
    }
}
pub struct Security(PathBuf);
impl DirectorySecurity for Security {
    fn verify(&self, p: &Path) -> Result<(), agentnotify_core::contracts::QueueError> {
        if p == self.0 {
            reject_links(p)
        } else {
            Err(error(
                agentnotify_core::contracts::QueueErrorCode::StorageUnavailable,
            ))
        }
    }
}
pub struct Fixture {
    pub root: tempfile::TempDir,
    pub clock: Arc<TestClock>,
    pub policy: Arc<Policy>,
    pub queue: Arc<SqliteQueue>,
}
impl Fixture {
    pub fn new() -> Self {
        Self::configured(config())
    }
    pub fn with_bodies(mut c: ConfigV1) -> Self {
        c.include_question = true;
        c.include_answer = true;
        Self::configured_with_protector(c, Some(Arc::new(queue_fixture::FixtureAead)))
    }
    pub fn configured(c: ConfigV1) -> Self {
        Self::configured_with_protector(c, None)
    }
    fn configured_with_protector(c: ConfigV1, protector: Option<Arc<dyn BodyProtector>>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let clock = Arc::new(TestClock(Mutex::new(base())));
        let policy = Arc::new(Policy::new(c.clone(), "UTC", snapshot(0)).unwrap());
        policy.register_instance(Uuid::nil(), caps());
        let queue = Arc::new(Self::open_with_protector(
            root.path(),
            c,
            clock.clone(),
            policy.clone(),
            protector,
        ));
        Self {
            root,
            clock,
            policy,
            queue,
        }
    }
    pub fn open(
        root: &Path,
        c: ConfigV1,
        clock: Arc<TestClock>,
        policy: Arc<Policy>,
    ) -> SqliteQueue {
        Self::open_with_protector(root, c, clock, policy, None)
    }
    fn open_with_protector(
        root: &Path,
        c: ConfigV1,
        clock: Arc<TestClock>,
        policy: Arc<Policy>,
        protector: Option<Arc<dyn BodyProtector>>,
    ) -> SqliteQueue {
        let path = root.canonicalize().unwrap();
        SqliteQueue::open(
            &path,
            c,
            QueueDependencies {
                clock,
                security: Arc::new(Security(path.clone())),
                protector,
                participant: policy,
                faults: Arc::new(NoFaults),
            },
        )
        .unwrap()
    }
    pub fn time(&self, t: i64) {
        *self.clock.0.lock().unwrap() = base() + Delta::seconds(t);
        self.policy.set_snapshot(snapshot(t));
    }
    pub fn accept(&self, mut o: AdapterOutput) -> Uuid {
        let now = self.clock.now();
        if let Some(e) = &mut o.notification {
            e.received_at = now;
        }
        if let Some(l) = &mut o.lifecycle {
            l.received_at = now;
        }
        match self.queue.accept(o).unwrap() {
            agentnotify_core::contracts::EnqueueOutcome::Queued { id }
            | agentnotify_core::contracts::EnqueueOutcome::Duplicate { id }
            | agentnotify_core::contracts::EnqueueOutcome::Spooled { id } => id,
        }
    }
    pub fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.queue.database_path()).unwrap()
    }
    pub fn count(&self, table: &str) -> i64 {
        self.db()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    pub fn status(&self, id: Uuid) -> agentnotify_core::contracts::QueueStatus {
        self.queue.notification_status(id).unwrap()
    }
    pub fn due(&self) -> usize {
        self.policy
            .evaluate_due(&self.queue, Uuid::nil())
            .unwrap()
            .unwrap_or(0)
    }
}

pub fn body_output(
    actor: &str,
    turn: Option<&str>,
    kind: EventKind,
    answer: &str,
) -> AdapterOutput {
    let mut o = output(actor, turn, kind);
    let n = o.notification.as_mut().unwrap();
    n.question = Some("isolated fixture question".into());
    n.answer = Some(answer.into());
    n.content_key_available = true;
    o
}
