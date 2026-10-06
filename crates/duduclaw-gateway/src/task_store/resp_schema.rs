//! P2-A schema: continuous responsibilities, wake-up subscriptions and facts,
//! occurrences, steering, durable dispatch intents and stop requests.
//!
//! Every table is new (`CREATE TABLE IF NOT EXISTS`), `tasks` is never
//! altered, so an old `tasks.db` gains the tables in place and nothing about
//! existing rows changes. A schema version this build does not know makes
//! [`TaskStore::open`] fail (fail closed) instead of silently running without
//! the guarantees these tables carry.

use super::*;

/// Owner key of the version row in `responsibility_schema_meta`.
pub const RESP_SCHEMA_OWNER: &str = "p2a";
/// The only schema version this build reads and writes.
pub const RESP_SCHEMA_VERSION: i64 = 1;

const RESP_SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS responsibility_schema_meta (owner TEXT PRIMARY KEY, version INTEGER NOT NULL);
INSERT OR IGNORE INTO responsibility_schema_meta VALUES ('p2a', 1);

CREATE TABLE IF NOT EXISTS responsibilities (
  responsibility_id            TEXT PRIMARY KEY,
  owner_agent_id               TEXT NOT NULL,
  created_by                   TEXT NOT NULL,
  objective                    TEXT NOT NULL CHECK(length(objective) BETWEEN 1 AND 4000),
  acceptance_template          TEXT NOT NULL CHECK(length(acceptance_template) BETWEEN 1 AND 4000),
  scope_json                   TEXT NOT NULL,
  source_refs_json             TEXT NOT NULL DEFAULT '[]',
  notification_policy_json     TEXT NOT NULL,
  schedule_json                TEXT,
  occurrence_hours             INTEGER NOT NULL CHECK(occurrence_hours BETWEEN 1 AND 72),
  occurrence_cost_cap_cents       INTEGER NOT NULL CHECK(occurrence_cost_cap_cents > 0),
  budget_period                TEXT NOT NULL CHECK(budget_period IN ('day','week','month')),
  budget_timezone              TEXT NOT NULL,
  period_cost_limit_cents         INTEGER NOT NULL CHECK(period_cost_limit_cents >= occurrence_cost_cap_cents),
  period_occurrence_limit      INTEGER NOT NULL CHECK(period_occurrence_limit BETWEEN 1 AND 96),
  min_wake_interval_secs       INTEGER NOT NULL CHECK(min_wake_interval_secs >= 300),
  max_consecutive_failures     INTEGER NOT NULL DEFAULT 3 CHECK(max_consecutive_failures BETWEEN 1 AND 10),
  stop_at                      TEXT NOT NULL,
  state                        TEXT NOT NULL CHECK(state IN
                                 ('active','paused','disabled','budget_paused','failure_paused','expired')),
  state_reason                 TEXT,
  state_changed_by             TEXT,
  state_changed_at             TEXT,
  contract_revision            INTEGER NOT NULL DEFAULT 1 CHECK(contract_revision >= 1),
  contract_hash                TEXT NOT NULL,
  control_epoch                INTEGER NOT NULL DEFAULT 1 CHECK(control_epoch >= 1),
  consecutive_failures         INTEGER NOT NULL DEFAULT 0,
  last_occurrence_at           TEXT,
  created_at                   TEXT NOT NULL,
  updated_at                   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_resp_owner_state ON responsibilities(owner_agent_id, state);
CREATE TRIGGER IF NOT EXISTS resp_no_rewind BEFORE UPDATE OF contract_revision, control_epoch ON responsibilities
  WHEN NEW.contract_revision < OLD.contract_revision OR NEW.control_epoch < OLD.control_epoch
  BEGIN SELECT RAISE(ABORT,'responsibility revision cannot rewind'); END;
CREATE TRIGGER IF NOT EXISTS resp_expired_terminal BEFORE UPDATE OF state ON responsibilities
  WHEN OLD.state = 'expired' AND NEW.state <> 'expired'
  BEGIN SELECT RAISE(ABORT,'expired responsibility is terminal'); END;

CREATE TABLE IF NOT EXISTS task_wakeups (
  wakeup_id          TEXT PRIMARY KEY,
  responsibility_id  TEXT NOT NULL,
  control_epoch      INTEGER NOT NULL,
  kind               TEXT NOT NULL CHECK(kind IN ('time','event','decision')),
  recurring          INTEGER NOT NULL CHECK(recurring IN (0,1)),
  due_at             TEXT,
  event_name         TEXT,
  event_filter_json  TEXT,
  approval_id        TEXT,
  armed_by           TEXT NOT NULL,
  state              TEXT NOT NULL CHECK(state IN ('armed','consumed','cancelled','expired')),
  created_at         TEXT NOT NULL,
  updated_at         TEXT NOT NULL,
  armed_after_event_id INTEGER
);
CREATE INDEX IF NOT EXISTS idx_wakeup_due   ON task_wakeups(state, kind, due_at);
CREATE INDEX IF NOT EXISTS idx_wakeup_event ON task_wakeups(state, event_name) WHERE kind = 'event';
CREATE INDEX IF NOT EXISTS idx_wakeup_resp  ON task_wakeups(responsibility_id, state);

CREATE TABLE IF NOT EXISTS wakeup_fires (
  fire_id            TEXT PRIMARY KEY,
  wakeup_id          TEXT NOT NULL,
  responsibility_id  TEXT NOT NULL,
  control_epoch      INTEGER NOT NULL,
  fire_key           TEXT NOT NULL,
  reason             TEXT NOT NULL CHECK(reason IN ('time','event','decision','timeout')),
  data_json          TEXT,
  guard_flags_json   TEXT,
  state              TEXT NOT NULL CHECK(state IN ('pending','consumed','coalesced','dropped')),
  drop_reason        TEXT,
  occurrence_task_id TEXT,
  observed_at        TEXT NOT NULL,
  settled_at         TEXT,
  UNIQUE(responsibility_id, fire_key)
);
CREATE INDEX IF NOT EXISTS idx_fire_pending ON wakeup_fires(responsibility_id, state);

CREATE TABLE IF NOT EXISTS responsibility_occurrences (
  responsibility_id   TEXT NOT NULL,
  occurrence_key      TEXT NOT NULL,
  task_id             TEXT NOT NULL UNIQUE,
  contract_revision   INTEGER NOT NULL,
  control_epoch       INTEGER NOT NULL,
  period_key          TEXT NOT NULL,
  reserved_cents         INTEGER NOT NULL,
  charged_cents          INTEGER,
  cost_basis          TEXT CHECK(cost_basis IN ('measured','reserved_unknown')),
  outcome             TEXT CHECK(outcome IN ('done','failed','cancelled','stopped','stopped_counted','blocked')),
  predecessor_task_id TEXT,
  created_at          TEXT NOT NULL,
  settled_at          TEXT,
  PRIMARY KEY(responsibility_id, occurrence_key)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_occ_one_open ON responsibility_occurrences(responsibility_id) WHERE outcome IS NULL;
CREATE INDEX IF NOT EXISTS idx_occ_period ON responsibility_occurrences(responsibility_id, period_key);

CREATE TABLE IF NOT EXISTS responsibility_event_cursor (
  singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
  last_event_id INTEGER NOT NULL,
  updated_at TEXT NOT NULL,
  paused INTEGER NOT NULL DEFAULT 0 CHECK(paused IN (0,1))
);

CREATE TABLE IF NOT EXISTS task_steering (
  steering_id                   TEXT PRIMARY KEY,
  task_id                       TEXT NOT NULL,
  seq                           INTEGER NOT NULL,
  body                          TEXT NOT NULL CHECK(length(body) BETWEEN 1 AND 4000),
  body_hash                     TEXT NOT NULL,
  guard_flags_json              TEXT NOT NULL,
  submitted_by                  TEXT NOT NULL,
  submitted_via                 TEXT NOT NULL CHECK(submitted_via IN ('dashboard')),
  submitted_authority_revision  INTEGER NOT NULL,
  client_request_id             TEXT NOT NULL,
  state                         TEXT NOT NULL CHECK(state IN ('pending','delivering','applied','discarded')),
  intent_id                     TEXT,
  applied_round                 INTEGER,
  applied_message_id            TEXT,
  applied_authority_revision    INTEGER,
  discard_reason                TEXT,
  created_at                    TEXT NOT NULL,
  updated_at                    TEXT NOT NULL,
  UNIQUE(task_id, seq),
  UNIQUE(task_id, client_request_id)
);
CREATE INDEX IF NOT EXISTS idx_steer_task_state ON task_steering(task_id, state);

CREATE TABLE IF NOT EXISTS responsibility_notice_log (
  responsibility_id  TEXT NOT NULL,
  notice_key         TEXT NOT NULL,
  period_key         TEXT NOT NULL,
  outcome            TEXT NOT NULL,
  created_at         TEXT NOT NULL,
  PRIMARY KEY (responsibility_id, notice_key)
);
CREATE INDEX IF NOT EXISTS idx_notice_window ON responsibility_notice_log(responsibility_id, period_key);

CREATE TABLE IF NOT EXISTS task_dispatch_intents (
  intent_id           TEXT PRIMARY KEY,
  task_id             TEXT NOT NULL,
  iter                INTEGER NOT NULL,
  authority_revision  INTEGER NOT NULL,
  state               TEXT NOT NULL CHECK(state IN ('intended','enqueued','abandoned')),
  created_at          TEXT NOT NULL,
  updated_at          TEXT NOT NULL,
  started_at          TEXT,
  UNIQUE(task_id, iter)
);

CREATE TABLE IF NOT EXISTS task_stop_requests (
  root_task_id        TEXT PRIMARY KEY,
  requested_by        TEXT NOT NULL,
  requested_at        TEXT NOT NULL,
  expected_authority_revision INTEGER NOT NULL,
  affected_task_ids_json TEXT NOT NULL,
  state               TEXT NOT NULL CHECK(state IN ('cancel_pending','stopped','stopped_uncertain')),
  detail_json         TEXT,
  updated_at          TEXT NOT NULL,
  counts_as_failure   INTEGER NOT NULL DEFAULT 0 CHECK(counts_as_failure IN (0,1))
);
";

impl TaskStore {
    /// Create the P2-A tables and refuse a schema version this build does not
    /// understand. Idempotent; runs on every open.
    pub(super) fn init_responsibility_schema(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(RESP_SCHEMA_SQL)
            .map_err(|e| format!("init responsibility schema: {e}"))?;
        add_intent_started_at(conn)?;
        check_responsibility_schema_version(conn)
    }
}

/// M3-1: `task_dispatch_intents.started_at` (the dispatcher's durable "this
/// round was handed to a runtime" mark) for a table created before the
/// column existed, and the `tasks(parent_task_id)` index. Idempotent.
fn add_intent_started_at(conn: &Connection) -> Result<(), String> {
    let has: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('task_dispatch_intents') WHERE name = 'started_at')",
            [],
            |r| r.get(0),
        )
        .map_err(|e| format!("intent schema: {e}"))?;
    if !has {
        conn.execute_batch("ALTER TABLE task_dispatch_intents ADD COLUMN started_at TEXT")
            .map_err(|e| format!("intent schema: add started_at: {e}"))?;
    }
    // Stop-tree walks, the sub-task cap and the ancestry checks all follow
    // `parent_task_id`; without an index each level is a table scan.
    conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_tasks_parent ON tasks(parent_task_id)")
        .map_err(|e| format!("task parent index: {e}"))?;
    Ok(())
}

/// Fail closed on an unknown (newer or corrupted) responsibility schema.
pub(super) fn check_responsibility_schema_version(conn: &Connection) -> Result<(), String> {
    let version: Option<i64> = conn
        .query_row(
            "SELECT version FROM responsibility_schema_meta WHERE owner = ?1",
            params![RESP_SCHEMA_OWNER],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("read responsibility schema version: {e}"))?;
    match version {
        Some(RESP_SCHEMA_VERSION) => Ok(()),
        Some(other) => Err(format!(
            "responsibility schema version {other} is not supported by this build \
             (expected {RESP_SCHEMA_VERSION}); refusing to open the task store"
        )),
        None => Err("responsibility schema version row missing".into()),
    }
}
