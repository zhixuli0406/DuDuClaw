-- Exact reviewed v1.82 autopilot initializer: the v1.79.3 catalog (autopilot-v1793.sql,
-- unchanged) plus the v1.82 approval-gate ledger `autopilot_approval_dispatch`
-- (`autopilot_store::DISPATCH_SCHEMA_SQL`, byte-identical). The ledger is never
-- authorized for reading: old frozen actions and approval ids never authorize anything.
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

CREATE TABLE IF NOT EXISTS autopilot_approval_dispatch (
    dispatch_id   TEXT PRIMARY KEY,
    approval_id   TEXT UNIQUE,
    rule_id       TEXT NOT NULL,
    rule_name     TEXT NOT NULL,
    rule_hash     TEXT NOT NULL,
    event_name    TEXT NOT NULL,
    event_ref     TEXT NOT NULL,
    dedupe_key    TEXT NOT NULL,
    frozen_json   TEXT NOT NULL,
    frozen_digest TEXT NOT NULL,
    state         TEXT NOT NULL,
    outcome       TEXT,
    detail        TEXT,
    notice_activity_pending INTEGER NOT NULL DEFAULT 0,
    notice_push_state TEXT,
    notice_push_attempts INTEGER NOT NULL DEFAULT 0,
    notice_push_next_at TEXT,
    notice_pushed_at TEXT,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ap_dispatch_state ON autopilot_approval_dispatch(state);
CREATE INDEX IF NOT EXISTS idx_ap_dispatch_dedupe ON autopilot_approval_dispatch(dedupe_key);
CREATE INDEX IF NOT EXISTS idx_ap_dispatch_notice ON autopilot_approval_dispatch(notice_activity_pending, notice_push_state);
