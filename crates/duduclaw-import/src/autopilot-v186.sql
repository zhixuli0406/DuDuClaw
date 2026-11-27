-- Exact reviewed v1.86 autopilot initializer: the v1.82 catalog plus rule
-- extensions, dispatch step links and durable runs (RUNS_SCHEMA_SQL, byte-identical).
-- Runs, steps and approval ledgers are never authorized for reading.
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

ALTER TABLE autopilot_rules ADD COLUMN active_window TEXT;
ALTER TABLE autopilot_rules ADD COLUMN run_policy TEXT;
ALTER TABLE autopilot_rules ADD COLUMN owner_department TEXT;
ALTER TABLE autopilot_rules ADD COLUMN created_by_user_id TEXT;
ALTER TABLE autopilot_rules ADD COLUMN updated_by_user_id TEXT;
ALTER TABLE autopilot_rules ADD COLUMN updated_at TEXT;
ALTER TABLE autopilot_approval_dispatch ADD COLUMN run_id TEXT;
ALTER TABLE autopilot_approval_dispatch ADD COLUMN step_index INTEGER;

CREATE TABLE IF NOT EXISTS autopilot_runs (
    run_id TEXT PRIMARY KEY,
    rule_id TEXT NOT NULL,
    rule_name TEXT NOT NULL,
    rule_hash TEXT NOT NULL,
    event_name TEXT NOT NULL,
    event_ref TEXT NOT NULL,
    dedupe_key TEXT NOT NULL UNIQUE,
    fields_json TEXT NOT NULL,
    state TEXT NOT NULL,
    step_index INTEGER NOT NULL DEFAULT 0,
    step_count INTEGER NOT NULL,
    next_at TEXT,
    lease_owner TEXT,
    lease_until TEXT,
    owner_department TEXT,
    outcome TEXT,
    detail TEXT,
    notice_pending INTEGER NOT NULL DEFAULT 0,
    notice_push_state TEXT,
    notice_push_attempts INTEGER NOT NULL DEFAULT 0,
    notice_push_next_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ap_runs_due ON autopilot_runs(state, next_at);
CREATE INDEX IF NOT EXISTS idx_ap_runs_rule ON autopilot_runs(rule_id, state);
CREATE TABLE IF NOT EXISTS autopilot_run_steps (
    run_id TEXT NOT NULL,
    step_index INTEGER NOT NULL,
    step_type TEXT NOT NULL,
    state TEXT NOT NULL,
    idempotency_key TEXT NOT NULL UNIQUE,
    dispatch_id TEXT,
    queue_message_id TEXT,
    started_at TEXT,
    finished_at TEXT,
    detail TEXT,
    PRIMARY KEY (run_id, step_index)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_ap_dispatch_run_step ON autopilot_approval_dispatch(run_id, step_index) WHERE run_id IS NOT NULL;
