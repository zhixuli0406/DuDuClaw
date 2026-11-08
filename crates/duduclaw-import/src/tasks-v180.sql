-- Generated from reviewed task-store DDL (v1.80.x tasks.db; scripts/capture_tasks_schema.py).
PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;

             CREATE TABLE IF NOT EXISTS tasks (
                 id              TEXT PRIMARY KEY,
                 title           TEXT NOT NULL,
                 description     TEXT NOT NULL DEFAULT '',
                 status          TEXT NOT NULL DEFAULT 'todo',
                 priority        TEXT NOT NULL DEFAULT 'medium',
                 assigned_to     TEXT NOT NULL,
                 created_by      TEXT NOT NULL DEFAULT 'system',
                 created_at      TEXT NOT NULL,
                 updated_at      TEXT NOT NULL,
                 completed_at    TEXT,
                 blocked_reason  TEXT,
                 parent_task_id  TEXT,
                 tags            TEXT NOT NULL DEFAULT '',
                 message_id      TEXT
             );

             CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
             CREATE INDEX IF NOT EXISTS idx_tasks_assigned ON tasks(assigned_to);
             CREATE INDEX IF NOT EXISTS idx_tasks_a2a_creator_id ON tasks(created_by, id);
             CREATE INDEX IF NOT EXISTS idx_tasks_priority ON tasks(priority);

             CREATE TABLE IF NOT EXISTS discovery_decision_receipts (
                 approval_id TEXT PRIMARY KEY,
                 task_id TEXT NOT NULL,
                 decider TEXT NOT NULL,
                 approve INTEGER NOT NULL CHECK (approve IN (0,1)),
                 payload_sha256 TEXT NOT NULL,
                 frozen_sha256 TEXT NOT NULL,
                 created_at TEXT NOT NULL
             );

             CREATE TABLE IF NOT EXISTS activity (
                 id          TEXT PRIMARY KEY,
                 event_type  TEXT NOT NULL,
                 agent_id    TEXT NOT NULL,
                 task_id     TEXT,
                 summary     TEXT NOT NULL,
                 timestamp   TEXT NOT NULL,
                 metadata    TEXT
             );

             CREATE INDEX IF NOT EXISTS idx_activity_agent ON activity(agent_id);
             CREATE INDEX IF NOT EXISTS idx_activity_type  ON activity(event_type);
             CREATE INDEX IF NOT EXISTS idx_activity_ts    ON activity(timestamp DESC);

             CREATE TABLE IF NOT EXISTS task_comments (
                 id          TEXT PRIMARY KEY,
                 task_id     TEXT NOT NULL,
                 author_user TEXT NOT NULL,
                 body        TEXT NOT NULL,
                 created_at  TEXT NOT NULL
             );

             CREATE INDEX IF NOT EXISTS idx_comments_task ON task_comments(task_id, created_at);

             CREATE TABLE IF NOT EXISTS goals (
                 id              TEXT PRIMARY KEY,
                 title           TEXT NOT NULL,
                 description     TEXT NOT NULL DEFAULT '',
                 parent_goal_id  TEXT,
                 status          TEXT NOT NULL DEFAULT 'active',
                 created_at      TEXT NOT NULL
             );

             CREATE INDEX IF NOT EXISTS idx_goals_parent ON goals(parent_goal_id);
             CREATE INDEX IF NOT EXISTS idx_goals_status ON goals(status);
ALTER TABLE tasks ADD COLUMN claimed_by TEXT;
ALTER TABLE tasks ADD COLUMN claimed_at TEXT;
ALTER TABLE tasks ADD COLUMN lease_expires_at TEXT;
ALTER TABLE tasks ADD COLUMN depends_on TEXT NOT NULL DEFAULT '[]';
ALTER TABLE tasks ADD COLUMN retry_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tasks ADD COLUMN max_retries INTEGER NOT NULL DEFAULT 3;
ALTER TABLE tasks ADD COLUMN goal_mode INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tasks ADD COLUMN acceptance_criteria TEXT;
ALTER TABLE tasks ADD COLUMN result_summary TEXT;
ALTER TABLE tasks ADD COLUMN judge_feedback TEXT;
ALTER TABLE tasks ADD COLUMN goal_id TEXT;
ALTER TABLE tasks ADD COLUMN lease_renewed_at TEXT;
ALTER TABLE tasks ADD COLUMN source_channel TEXT;
ALTER TABLE tasks ADD COLUMN source_chat_id TEXT;
ALTER TABLE tasks ADD COLUMN revision_round INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tasks ADD COLUMN diminishing INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tasks ADD COLUMN agent_seconds INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tasks ADD COLUMN goal_state_json TEXT;
ALTER TABLE tasks ADD COLUMN source_discord_guild_id TEXT;
ALTER TABLE tasks ADD COLUMN deadline_at TEXT;
ALTER TABLE tasks ADD COLUMN cost_cap_cents INTEGER CHECK (cost_cap_cents > 0);
ALTER TABLE tasks ADD COLUMN timeout_secs INTEGER CHECK (timeout_secs BETWEEN 30 AND 3600);
ALTER TABLE tasks ADD COLUMN risk_boundary TEXT;
ALTER TABLE tasks ADD COLUMN acceptance_criteria_baseline TEXT;
ALTER TABLE tasks ADD COLUMN pause_reason TEXT;
ALTER TABLE tasks ADD COLUMN plan_pending TEXT;
ALTER TABLE tasks ADD COLUMN plan_review TEXT;
ALTER TABLE tasks ADD COLUMN archived INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tasks ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tasks ADD COLUMN team_spec_json TEXT;
ALTER TABLE tasks ADD COLUMN kind TEXT NOT NULL DEFAULT 'task';
ALTER TABLE tasks ADD COLUMN discovery_spec_json TEXT;
ALTER TABLE tasks ADD COLUMN discovery_run_id TEXT;
ALTER TABLE tasks ADD COLUMN discovery_approval_id TEXT;
ALTER TABLE tasks ADD COLUMN criteria_ledger TEXT;
ALTER TABLE tasks ADD COLUMN authority_revision INTEGER NOT NULL DEFAULT 1 CHECK(authority_revision >= 1);
ALTER TABLE tasks ADD COLUMN on_behalf_of TEXT;
ALTER TABLE tasks ADD COLUMN created_via TEXT;
ALTER TABLE tasks ADD COLUMN a2a_owner TEXT;
CREATE TRIGGER IF NOT EXISTS task_timeout_follows_assignee
             AFTER UPDATE OF assigned_to ON tasks
             WHEN OLD.assigned_to IS NOT NEW.assigned_to AND NEW.timeout_secs IS NOT NULL
             BEGIN
                 UPDATE tasks SET timeout_secs = NULL WHERE id = NEW.id;
             END;
CREATE UNIQUE INDEX IF NOT EXISTS idx_tasks_discovery_run ON tasks(discovery_run_id) WHERE discovery_run_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_tasks_employee_health ON tasks(assigned_to, goal_mode, status);
CREATE INDEX IF NOT EXISTS idx_tasks_lease ON tasks(status, lease_expires_at);
CREATE INDEX IF NOT EXISTS idx_tasks_archived ON tasks(archived);
CREATE INDEX IF NOT EXISTS idx_tasks_on_behalf_of ON tasks(on_behalf_of, updated_at) WHERE on_behalf_of IS NOT NULL;
CREATE TABLE IF NOT EXISTS task_access_principals (
                 task_id TEXT PRIMARY KEY,
                 principal TEXT NOT NULL
             );
             UPDATE tasks SET a2a_owner = substr(created_by, 5)
             WHERE a2a_owner IS NULL AND created_by GLOB 'a2a:user:?*';
             WITH RECURSIVE inherited(id, principal) AS (
                 SELECT id, a2a_owner FROM tasks WHERE a2a_owner IS NOT NULL
                 UNION SELECT t.id, i.principal FROM tasks t JOIN inherited i ON t.parent_task_id = i.id
             ) UPDATE tasks SET a2a_owner = (SELECT principal FROM inherited WHERE inherited.id = tasks.id)
               WHERE a2a_owner IS NULL AND id IN (SELECT id FROM inherited);
             INSERT OR IGNORE INTO task_access_principals
                 SELECT id, a2a_owner FROM tasks WHERE a2a_owner IS NOT NULL;
             CREATE TRIGGER IF NOT EXISTS task_a2a_owner_immutable
             BEFORE UPDATE OF a2a_owner ON tasks
             WHEN OLD.a2a_owner IS NOT NULL AND NEW.a2a_owner IS NOT OLD.a2a_owner
             BEGIN SELECT RAISE(ABORT, 'A2A task owner is immutable'); END;
             CREATE TRIGGER IF NOT EXISTS task_a2a_parent_update
             BEFORE UPDATE OF parent_task_id ON tasks
             WHEN (OLD.a2a_owner IS NOT NULL AND NEW.parent_task_id IS NOT OLD.parent_task_id)
                 OR EXISTS (SELECT 1 FROM task_access_principals
                 WHERE task_id = NEW.parent_task_id AND principal IS NOT NEW.a2a_owner)
             BEGIN SELECT RAISE(ABORT, 'A2A parent owner mismatch'); END;
             CREATE TRIGGER IF NOT EXISTS task_a2a_owner_insert
             AFTER INSERT ON tasks
             BEGIN
                 SELECT CASE WHEN EXISTS (SELECT 1 FROM task_access_principals
                     WHERE task_id = NEW.id)
                     THEN RAISE(ABORT, 'A2A task id cannot be reused') END;
                 SELECT CASE WHEN NEW.a2a_owner IS NOT NULL AND NEW.parent_task_id IS NOT NULL
                     AND NOT EXISTS (SELECT 1 FROM task_access_principals
                     WHERE task_id = NEW.parent_task_id AND principal = NEW.a2a_owner)
                     THEN RAISE(ABORT, 'A2A parent owner mismatch') END;
                 UPDATE tasks SET a2a_owner = COALESCE(NEW.a2a_owner,
                     (SELECT principal FROM task_access_principals WHERE task_id = NEW.parent_task_id))
                     WHERE id = NEW.id;
                 INSERT INTO task_access_principals SELECT id, a2a_owner FROM tasks
                     WHERE id = NEW.id AND a2a_owner IS NOT NULL;
             END;
CREATE TABLE IF NOT EXISTS task_authority_epochs (
            task_id TEXT PRIMARY KEY, revision_floor INTEGER NOT NULL CHECK(revision_floor>=1));
            INSERT INTO task_authority_epochs(task_id,revision_floor)
                SELECT id,authority_revision FROM tasks WHERE 1
                ON CONFLICT(task_id) DO UPDATE SET revision_floor=MAX(revision_floor,excluded.revision_floor);
            CREATE TRIGGER IF NOT EXISTS task_epoch_no_delete BEFORE DELETE ON task_authority_epochs
                BEGIN SELECT RAISE(ABORT,'task authority tombstones cannot be deleted'); END;
            CREATE TRIGGER IF NOT EXISTS task_epoch_no_rewind BEFORE UPDATE ON task_authority_epochs
                WHEN NEW.task_id IS NOT OLD.task_id OR NEW.revision_floor<OLD.revision_floor
                BEGIN SELECT RAISE(ABORT,'task authority epoch cannot rewind'); END;
            CREATE TRIGGER IF NOT EXISTS task_revision_no_rewind BEFORE UPDATE OF authority_revision ON tasks
                WHEN NEW.authority_revision<OLD.authority_revision
                BEGIN SELECT RAISE(ABORT,'task authority revision cannot rewind'); END;
            CREATE TRIGGER IF NOT EXISTS task_epoch_insert AFTER INSERT ON tasks BEGIN
                UPDATE tasks SET authority_revision=MAX(NEW.authority_revision,
                    COALESCE((SELECT revision_floor FROM task_authority_epochs WHERE task_id=NEW.id),1))
                    WHERE id=NEW.id;
                INSERT INTO task_authority_epochs(task_id,revision_floor)
                    SELECT id,authority_revision FROM tasks WHERE id=NEW.id
                    ON CONFLICT(task_id) DO UPDATE SET revision_floor=MAX(revision_floor,excluded.revision_floor);
            END;
            CREATE TRIGGER IF NOT EXISTS task_epoch_update AFTER UPDATE OF authority_revision ON tasks BEGIN
                INSERT INTO task_authority_epochs(task_id,revision_floor) VALUES(NEW.id,NEW.authority_revision)
                    ON CONFLICT(task_id) DO UPDATE SET revision_floor=MAX(revision_floor,excluded.revision_floor);
            END;
            CREATE TRIGGER IF NOT EXISTS task_epoch_delete AFTER DELETE ON tasks BEGIN
                INSERT INTO task_authority_epochs(task_id,revision_floor) VALUES(OLD.id,OLD.authority_revision+1)
                    ON CONFLICT(task_id) DO UPDATE SET revision_floor=MAX(revision_floor,excluded.revision_floor);
            END;
            CREATE TRIGGER IF NOT EXISTS task_epoch_rename AFTER UPDATE OF id ON tasks
                WHEN OLD.id IS NOT NEW.id BEGIN
                INSERT INTO task_authority_epochs(task_id,revision_floor) VALUES(OLD.id,OLD.authority_revision+1)
                    ON CONFLICT(task_id) DO UPDATE SET revision_floor=MAX(revision_floor,excluded.revision_floor);
                INSERT INTO task_authority_epochs(task_id,revision_floor) VALUES(NEW.id,OLD.authority_revision+1)
                    ON CONFLICT(task_id) DO UPDATE SET revision_floor=MAX(revision_floor,excluded.revision_floor);
            END;
DROP TRIGGER IF EXISTS task_authority_revision_v1;
             CREATE TRIGGER task_authority_revision_v1
             AFTER UPDATE OF id,title,description,assigned_to,created_by,created_at,parent_task_id,goal_id,tags,depends_on,max_retries,goal_mode,acceptance_criteria,acceptance_criteria_baseline,deadline_at,cost_cap_cents,timeout_secs,risk_boundary,plan_pending,team_spec_json,kind,source_channel,source_chat_id,source_discord_guild_id,discovery_spec_json,discovery_run_id,discovery_approval_id,archived,retry_count,revision_round,status,claimed_by ON tasks
             WHEN OLD.id IS NOT NEW.id OR OLD.title IS NOT NEW.title OR OLD.description IS NOT NEW.description OR OLD.assigned_to IS NOT NEW.assigned_to OR OLD.created_by IS NOT NEW.created_by OR OLD.created_at IS NOT NEW.created_at OR OLD.parent_task_id IS NOT NEW.parent_task_id OR OLD.goal_id IS NOT NEW.goal_id OR OLD.tags IS NOT NEW.tags OR OLD.depends_on IS NOT NEW.depends_on OR OLD.max_retries IS NOT NEW.max_retries OR OLD.goal_mode IS NOT NEW.goal_mode OR OLD.acceptance_criteria IS NOT NEW.acceptance_criteria OR OLD.acceptance_criteria_baseline IS NOT NEW.acceptance_criteria_baseline OR OLD.deadline_at IS NOT NEW.deadline_at OR OLD.cost_cap_cents IS NOT NEW.cost_cap_cents OR OLD.timeout_secs IS NOT NEW.timeout_secs OR OLD.risk_boundary IS NOT NEW.risk_boundary OR OLD.plan_pending IS NOT NEW.plan_pending OR OLD.team_spec_json IS NOT NEW.team_spec_json OR OLD.kind IS NOT NEW.kind OR OLD.source_channel IS NOT NEW.source_channel OR OLD.source_chat_id IS NOT NEW.source_chat_id OR OLD.source_discord_guild_id IS NOT NEW.source_discord_guild_id OR OLD.discovery_spec_json IS NOT NEW.discovery_spec_json OR OLD.discovery_run_id IS NOT NEW.discovery_run_id OR OLD.discovery_approval_id IS NOT NEW.discovery_approval_id OR OLD.archived IS NOT NEW.archived OR OLD.retry_count IS NOT NEW.retry_count OR OLD.revision_round IS NOT NEW.revision_round
                OR (OLD.claimed_by IS NOT NULL AND OLD.claimed_by IS NOT NEW.claimed_by)
                OR (OLD.status IS NOT NEW.status AND NOT (
                    (OLD.status IN ('todo','pending','revising','queued','pending_approval')
                        AND NEW.status = 'in_progress')
                    OR (OLD.status = 'in_progress' AND NEW.status = 'review')
                ))
             BEGIN
                 UPDATE tasks SET authority_revision = MAX(OLD.authority_revision + 1,
                     COALESCE((SELECT revision_floor FROM task_authority_epochs WHERE task_id=NEW.id),1))
                     WHERE id = NEW.id;
             END;
CREATE TABLE IF NOT EXISTS plans (
                 id          TEXT PRIMARY KEY,
                 title       TEXT NOT NULL,
                 description TEXT NOT NULL DEFAULT '',
                 agent_id    TEXT NOT NULL,
                 goal_id     TEXT,
                 status      TEXT NOT NULL DEFAULT 'active',
                 created_by  TEXT NOT NULL DEFAULT 'system',
                 created_at  TEXT NOT NULL,
                 updated_at  TEXT NOT NULL
             );

             CREATE INDEX IF NOT EXISTS idx_plans_agent  ON plans(agent_id);
             CREATE INDEX IF NOT EXISTS idx_plans_status ON plans(status);

             CREATE TABLE IF NOT EXISTS plan_steps (
                 id            TEXT PRIMARY KEY,
                 plan_id       TEXT NOT NULL,
                 text          TEXT NOT NULL,
                 assignee_kind TEXT NOT NULL DEFAULT 'agent',
                 assignee      TEXT NOT NULL DEFAULT '',
                 status        TEXT NOT NULL DEFAULT 'todo',
                 step_order    INTEGER NOT NULL,
                 created_at    TEXT NOT NULL,
                 updated_at    TEXT NOT NULL
             );

             CREATE INDEX IF NOT EXISTS idx_plan_steps_plan ON plan_steps(plan_id, step_order);
CREATE TABLE IF NOT EXISTS task_iterations (
                 id             INTEGER PRIMARY KEY AUTOINCREMENT,
                 task_id        TEXT NOT NULL,
                 round          INTEGER NOT NULL,
                 dispatched_at  TEXT NOT NULL,
                 submitted_at   TEXT,
                 judged_at      TEXT,
                 verdict        TEXT,
                 judge_feedback TEXT,
                 feedback_class TEXT
             );

             CREATE INDEX IF NOT EXISTS idx_task_iterations_task
                 ON task_iterations(task_id, round);
ALTER TABLE task_iterations ADD COLUMN verdict_json TEXT;
ALTER TABLE task_iterations ADD COLUMN dispatch_count INTEGER NOT NULL DEFAULT 1;
ALTER TABLE task_iterations ADD COLUMN state_hash TEXT;
ALTER TABLE task_iterations ADD COLUMN repeat_streak INTEGER;
ALTER TABLE task_iterations ADD COLUMN worker_excerpt TEXT;
ALTER TABLE task_iterations ADD COLUMN evaluator_verdict TEXT;
ALTER TABLE task_iterations ADD COLUMN iter_seq INTEGER;
ALTER TABLE task_iterations ADD COLUMN team_mode TEXT;
ALTER TABLE task_iterations ADD COLUMN gate_inputs_json TEXT;
ALTER TABLE task_iterations ADD COLUMN state_block_json TEXT;
ALTER TABLE task_iterations ADD COLUMN knobs_json TEXT;
ALTER TABLE task_iterations ADD COLUMN pause_reason TEXT;
ALTER TABLE task_iterations ADD COLUMN criteria_ledger_json TEXT;
CREATE TABLE IF NOT EXISTS task_survival_evidence (
                task_id TEXT PRIMARY KEY,
                difficulty TEXT CHECK(difficulty IS NULL OR difficulty IN ('simple','complex','mixed')),
                manual_retry INTEGER CHECK(manual_retry IS NULL OR manual_retry IN (0,1)),
                human_approved INTEGER CHECK(human_approved IS NULL OR human_approved IN (0,1)),
                evidence_version INTEGER NOT NULL DEFAULT 1 CHECK(evidence_version >= 1),
                difficulty_dispatches INTEGER NOT NULL DEFAULT 0 CHECK(difficulty_dispatches >= 0),
                human_approved_iteration_id INTEGER
             );
CREATE TRIGGER IF NOT EXISTS task_survival_evidence_new_goal
             AFTER INSERT ON tasks WHEN NEW.goal_mode = 1 AND NEW.kind IN ('task','goal')
             BEGIN
                INSERT INTO task_survival_evidence(task_id,manual_retry,human_approved)
                VALUES(NEW.id,0,0);
             END;
             CREATE TRIGGER IF NOT EXISTS task_survival_evidence_invalidate_approval
             AFTER UPDATE OF status,result_summary,acceptance_criteria,title,description ON tasks
             WHEN OLD.status IS NOT NEW.status OR OLD.result_summary IS NOT NEW.result_summary
                OR OLD.acceptance_criteria IS NOT NEW.acceptance_criteria
                OR OLD.title IS NOT NEW.title OR OLD.description IS NOT NEW.description
             BEGIN
                UPDATE task_survival_evidence SET human_approved=0,human_approved_iteration_id=NULL
                WHERE task_id=NEW.id;
             END;
             CREATE TRIGGER IF NOT EXISTS task_survival_evidence_remove_task
             AFTER DELETE ON tasks BEGIN DELETE FROM task_survival_evidence WHERE task_id=OLD.id; END;
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
CREATE INDEX IF NOT EXISTS idx_tasks_parent ON tasks(parent_task_id);
CREATE TABLE IF NOT EXISTS assign_schedules (
  id                   TEXT PRIMARY KEY,
  creator_user_id      TEXT NOT NULL,
  agent_id             TEXT NOT NULL,
  kind                 TEXT NOT NULL CHECK(kind IN ('task','goal')),
  title                TEXT NOT NULL,
  payload_json         TEXT NOT NULL,
  schedule_json        TEXT NOT NULL,
  timezone             TEXT,
  status               TEXT NOT NULL CHECK(status IN ('active','paused','done','removed')),
  pause_reason         TEXT,
  created_via          TEXT NOT NULL,
  source_channel       TEXT,
  source_chat_id       TEXT,
  next_fire_at         TEXT,
  last_fire_at         TEXT,
  last_task_id         TEXT,
  last_outcome         TEXT,
  fire_count           INTEGER NOT NULL DEFAULT 0,
  skip_count           INTEGER NOT NULL DEFAULT 0,
  consecutive_failures INTEGER NOT NULL DEFAULT 0,
  created_at           TEXT NOT NULL,
  updated_at           TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_assign_sched_creator ON assign_schedules(creator_user_id, status);
CREATE INDEX IF NOT EXISTS idx_assign_sched_due ON assign_schedules(status, next_fire_at);
CREATE TABLE IF NOT EXISTS a2a_requests (
                owner TEXT NOT NULL,
                message_id TEXT NOT NULL,
                payload_sha256 TEXT NOT NULL,
                task_id TEXT NOT NULL UNIQUE,
                PRIMARY KEY(owner, message_id)
             );
             CREATE TABLE IF NOT EXISTS a2a_task_sequence (
                owner TEXT NOT NULL,
                seq INTEGER NOT NULL,
                task_id TEXT NOT NULL UNIQUE,
                PRIMARY KEY(owner, seq)
             );
             CREATE TRIGGER IF NOT EXISTS a2a_task_sequence_insert
             AFTER INSERT ON tasks WHEN NEW.a2a_owner IS NOT NULL AND NEW.created_via = 'a2a'
             BEGIN
                INSERT INTO a2a_task_sequence(owner,seq,task_id)
                VALUES(NEW.a2a_owner, (SELECT COALESCE(MAX(seq),0)+1 FROM a2a_task_sequence WHERE owner=NEW.a2a_owner), NEW.id);
             END;
             INSERT INTO a2a_task_sequence(owner,seq,task_id)
                SELECT a2a_owner,
                    COALESCE((SELECT MAX(seq) FROM a2a_task_sequence WHERE owner=tasks.a2a_owner),0)
                        + ROW_NUMBER() OVER (PARTITION BY a2a_owner ORDER BY created_at,id), id
                FROM tasks WHERE a2a_owner IS NOT NULL AND created_via = 'a2a'
                    AND NOT EXISTS (SELECT 1 FROM a2a_task_sequence WHERE task_id=tasks.id);
