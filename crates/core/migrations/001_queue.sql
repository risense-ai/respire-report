-- 入站ID为DB与spool共同去重凭据；event_key为实例/会话/actor范围的稳定来源事件键。
CREATE TABLE IF NOT EXISTS ingress (
 id TEXT PRIMARY KEY, instance_id TEXT NOT NULL, sequence INTEGER,
 event_key TEXT NOT NULL UNIQUE, received_at INTEGER NOT NULL,
 metadata BLOB NOT NULL, order_known INTEGER NOT NULL CHECK(order_known IN(0,1))
);
-- 不可变生命周期事实与通知通过同一入站外键提交，任务04活动表在同一事务维护。
CREATE TABLE IF NOT EXISTS lifecycle (ingress_id TEXT PRIMARY KEY REFERENCES ingress(id), payload BLOB NOT NULL);
-- 时间统一为UTC毫秒；body为有固定头的AEAD二进制封装，终态置NULL。
CREATE TABLE IF NOT EXISTS notifications (
 id TEXT PRIMARY KEY REFERENCES ingress(id), status TEXT NOT NULL CHECK(status IN
 ('queued','leased','sent','suppressed','digest_pending','retry_wait','auth_blocked','delivery_unknown','expired','permanent_failed')),
 due_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, lease_until INTEGER, lease_token TEXT,
 send_stage TEXT NOT NULL DEFAULT 'prepared' CHECK(send_stage IN('prepared','connecting','envelope','data_started','accepted')),
 attempts INTEGER NOT NULL DEFAULT 0, body BLOB
);
CREATE INDEX IF NOT EXISTS notification_due ON notifications(status,due_at);
-- checkpoint只前进；ordered=0是持久顺序降级，不能由后补序号自动清除。
CREATE TABLE IF NOT EXISTS checkpoints(instance_id TEXT PRIMARY KEY, sequence INTEGER NOT NULL, ordered INTEGER NOT NULL CHECK(ordered IN(0,1)));
CREATE TABLE IF NOT EXISTS diagnostics(id INTEGER PRIMARY KEY, ingress_id TEXT NOT NULL, code TEXT NOT NULL);
PRAGMA user_version=1;