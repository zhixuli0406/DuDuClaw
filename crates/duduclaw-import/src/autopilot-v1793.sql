-- Exact reviewed v1.79.3 autopilot initializer.
PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;

             CREATE TABLE IF NOT EXISTS autopilot_rules (
                 id               TEXT PRIMARY KEY,
                 name             TEXT NOT NULL,
                 enabled          INTEGER NOT NULL DEFAULT 1,
                 trigger_event    TEXT NOT NULL,
                 conditions       TEXT NOT NULL DEFAULT '{}',
                 action           TEXT NOT NULL DEFAULT '{}',
                 created_at       TEXT NOT NULL,
                 last_triggered_at TEXT,
                 trigger_count    INTEGER NOT NULL DEFAULT 0
             );

             CREATE TABLE IF NOT EXISTS autopilot_history (
                 id           TEXT PRIMARY KEY,
                 rule_id      TEXT NOT NULL,
                 rule_name    TEXT NOT NULL,
                 triggered_at TEXT NOT NULL,
                 result       TEXT NOT NULL,
                 details      TEXT
             );

             CREATE INDEX IF NOT EXISTS idx_ap_history_rule ON autopilot_history(rule_id);
             CREATE INDEX IF NOT EXISTS idx_ap_history_ts   ON autopilot_history(triggered_at DESC);

ALTER TABLE autopilot_rules ADD COLUMN sequence TEXT;

ALTER TABLE autopilot_rules ADD COLUMN metadata TEXT;
