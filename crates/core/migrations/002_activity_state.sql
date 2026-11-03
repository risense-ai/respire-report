-- UTC毫秒均由受控ClockSnapshot提供；所有副作用与入站/到期/租约共享事务。
BEGIN IMMEDIATE;
CREATE TABLE actor_state (
 actor_key TEXT PRIMARY KEY, instance_id TEXT NOT NULL, session_id TEXT NOT NULL,
 generation INTEGER NOT NULL CHECK(generation BETWEEN 0 AND 9007199254740991),
 current_turn TEXT NOT NULL, start_token TEXT, running INTEGER NOT NULL
);
CREATE TABLE activity_state (
 actor_key TEXT NOT NULL, turn_key TEXT NOT NULL, instance_id TEXT NOT NULL, session_id TEXT NOT NULL,
 actor_scope TEXT NOT NULL, actor_id TEXT, upstream_turn TEXT,
 started_at INTEGER NOT NULL, last_progress_at INTEGER NOT NULL, last_observed_sequence INTEGER,
 observed_utc INTEGER NOT NULL, monotonic_ms INTEGER NOT NULL, boot_id TEXT NOT NULL,
 resume_epoch INTEGER NOT NULL, runtime_id TEXT NOT NULL, source_evidence TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN('active','closed','suspended_unknown')),
 alerted INTEGER NOT NULL DEFAULT 0, revision INTEGER NOT NULL DEFAULT 0,
 PRIMARY KEY(actor_key,turn_key)
);
CREATE INDEX activity_instance ON activity_state(instance_id,state);
CREATE TABLE policy_notifications (
 id TEXT PRIMARY KEY REFERENCES ingress(id), actor_key TEXT, turn_key TEXT, kind TEXT NOT NULL,
 activity_revision INTEGER, reason TEXT NOT NULL, aggregate_first INTEGER, aggregate_count INTEGER NOT NULL DEFAULT 1,
 digest_key TEXT
);
CREATE TABLE policy_dedupe (dedupe_key TEXT PRIMARY KEY, expires_at INTEGER NOT NULL);
-- 摘要只保存计数与必要标签；成员ID是审计关联，不复制正文。
CREATE TABLE policy_digests (digest_key TEXT PRIMARY KEY, instance_id TEXT NOT NULL, due_at INTEGER NOT NULL,
 count INTEGER NOT NULL, labels TEXT NOT NULL, notification_id TEXT UNIQUE);
CREATE TABLE policy_rate (id INTEGER PRIMARY KEY, attempted_at INTEGER NOT NULL);
CREATE INDEX policy_rate_time ON policy_rate(attempted_at);
CREATE TABLE watchdog_ticks(instance_id TEXT PRIMARY KEY, runtime_id TEXT NOT NULL, monotonic_ms INTEGER NOT NULL);
PRAGMA user_version=2;
COMMIT;
