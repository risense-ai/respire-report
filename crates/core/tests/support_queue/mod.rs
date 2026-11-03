#![allow(dead_code)]
use agentnotify_core::{clock::Clock, config::ConfigV1, contracts::*, event::*, queue::*};
use chrono::{DateTime, Utc};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;
pub fn now() -> DateTime<Utc> {
    "2026-10-30T00:00:00Z".parse().unwrap()
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
/// Only a capability for the temporary directory owned by this fixture, not a Windows ACL verifier.
pub struct IsolatedDirectory(pub PathBuf);
impl DirectorySecurity for IsolatedDirectory {
    fn verify(&self, path: &Path) -> Result<(), QueueError> {
        if path != self.0 || path.canonicalize().ok() != Some(self.0.clone()) {
            return Err(error(QueueErrorCode::StorageUnavailable));
        }
        reject_links(path)?;
        Ok(())
    }
}
pub struct FixtureParticipant;
impl TransactionParticipant for FixtureParticipant {
    fn apply(
        &self,
        tx: &rusqlite::Transaction<'_>,
        e: &IngressEnvelopeV1,
        order: OrderingEvidence,
    ) -> Result<NotificationPlan, QueueError> {
        tx.execute_batch("CREATE TABLE IF NOT EXISTS fixture_observed (id TEXT PRIMARY KEY, kind TEXT, ordered INTEGER);").map_err(|_|error(QueueErrorCode::StorageFailed))?;
        let kind = e
            .adapter_output
            .lifecycle
            .as_ref()
            .map(|s| format!("{:?}", s.kind))
            .unwrap_or("event".into());
        tx.execute(
            "INSERT INTO fixture_observed VALUES (?1,?2,?3)",
            rusqlite::params![
                e.ingress_id.to_string(),
                kind,
                order == OrderingEvidence::Ordered
            ],
        )
        .map_err(|_| error(QueueErrorCode::StorageFailed))?;
        Ok(NotificationPlan {
            status: QueueStatus::Queued,
            not_before: e.received_at,
        })
    }
}
pub struct FixtureAead;
impl BodyProtector for FixtureAead {
    fn seal(&self, plain: &[u8], aad: &[u8]) -> Result<SealedBody, QueueError> {
        use aes_gcm::{
            Aes256Gcm, KeyInit,
            aead::{Aead, Payload},
        };
        let cipher = Aes256Gcm::new_from_slice(&[0xA7; 32]).unwrap();
        let random = Uuid::new_v4();
        let nonce: [u8; 12] = random.as_bytes()[..12].try_into().unwrap();
        let mut ciphertext = cipher
            .encrypt((&nonce).into(), Payload { msg: plain, aad })
            .map_err(|_| error(QueueErrorCode::StorageFailed))?;
        let tag = ciphertext
            .split_off(ciphertext.len() - 16)
            .try_into()
            .unwrap();
        Ok(SealedBody {
            nonce,
            tag,
            ciphertext,
        })
    }
    fn open(&self, sealed: &SealedBody, aad: &[u8]) -> Result<Vec<u8>, QueueError> {
        use aes_gcm::{
            Aes256Gcm, KeyInit,
            aead::{Aead, Payload},
        };
        let cipher = Aes256Gcm::new_from_slice(&[0xA7; 32]).unwrap();
        let mut bytes = sealed.ciphertext.clone();
        bytes.extend(sealed.tag);
        cipher
            .decrypt((&sealed.nonce).into(), Payload { msg: &bytes, aad })
            .map_err(|_| error(QueueErrorCode::StorageFailed))
    }
}
pub struct Fixture {
    pub root: tempfile::TempDir,
    pub clock: Arc<TestClock>,
    pub queue: Arc<SqliteQueue>,
}
impl Fixture {
    pub fn new() -> Self {
        Self::configured(ConfigV1::default(), Arc::new(NoFaults))
    }
    pub fn configured(config: ConfigV1, faults: Arc<dyn FaultInjector>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().canonicalize().unwrap();
        let clock = Arc::new(TestClock(Mutex::new(now())));
        let queue = SqliteQueue::open(
            &path,
            config,
            QueueDependencies {
                clock: clock.clone(),
                security: Arc::new(IsolatedDirectory(path.clone())),
                protector: Some(Arc::new(FixtureAead)),
                participant: Arc::new(FixtureParticipant),
                faults,
            },
        )
        .unwrap();
        Self {
            root,
            clock,
            queue: Arc::new(queue),
        }
    }
    pub fn drain(&self) -> ImportReport {
        let mut total = ImportReport::default();
        for _ in 0..100 {
            let r = self.queue.import_spool().unwrap();
            total.imported += r.imported;
            total.duplicates += r.duplicates;
            total.quarantined += r.quarantined;
            if r.remaining == 0 {
                return total;
            }
        }
        panic!("bounded import failed to drain");
    }
    pub fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.queue.database_path()).unwrap()
    }
    pub fn count(&self, table: &str) -> i64 {
        self.db()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    pub fn output(&self, id: &str) -> AdapterOutput {
        output(Uuid::nil(), id)
    }
    pub fn accept(&self, id: &str) -> EnqueueOutcome {
        self.queue.accept(self.output(id)).unwrap()
    }
}
pub fn output(instance: Uuid, id: &str) -> AdapterOutput {
    parse_generic(&serde_json::to_vec(&serde_json::json!({"schema_version":1,"adapter_id":"generic","instance_id":instance,"event_id":id,"session_id":"s","kind":"response_finished","completion_basis":"response_stop"})).unwrap(),&IngressContext{instance_id:instance,received_at:now(),content_key_available:false}).unwrap()
}
pub struct FailOnce(pub FaultPoint, pub std::sync::atomic::AtomicBool);
impl FailOnce {
    pub fn at(p: FaultPoint) -> Arc<Self> {
        Arc::new(Self(p, std::sync::atomic::AtomicBool::new(true)))
    }
}
impl FaultInjector for FailOnce {
    fn check(&self, p: FaultPoint) -> Result<(), QueueError> {
        if self.0 == p && self.1.swap(false, std::sync::atomic::Ordering::SeqCst) {
            Err(error(QueueErrorCode::StorageFailed))
        } else {
            Ok(())
        }
    }
}
pub fn id(out: EnqueueOutcome) -> Uuid {
    match out {
        EnqueueOutcome::Queued { id }
        | EnqueueOutcome::Spooled { id }
        | EnqueueOutcome::Duplicate { id } => id,
    }
}
