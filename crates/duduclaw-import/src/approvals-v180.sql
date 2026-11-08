-- Fresh current (v1.80.x line) ApprovalStore schema; generated from reviewed initializers by
-- scripts/capture_approval_schema.py. approvals-v1793.sql is the pinned older catalog and is never
-- regenerated. Difference: RM-027 decision lease columns approvals.lease_token / lease_until.
PRAGMA secure_delete=ON; PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;

CREATE TABLE IF NOT EXISTS approvals (
                 id           TEXT PRIMARY KEY,
                 agent_id     TEXT NOT NULL,
                 action_kind  TEXT NOT NULL,
                 summary      TEXT NOT NULL,
                 payload      TEXT NOT NULL DEFAULT '{}',
                 status       TEXT NOT NULL DEFAULT 'pending',
                 created_at   TEXT NOT NULL,
                 decided_at   TEXT,
                 decided_by   TEXT,
                 ttl_seconds  INTEGER NOT NULL DEFAULT 3600
             );

             CREATE INDEX IF NOT EXISTS idx_approvals_status ON approvals(status);
             CREATE INDEX IF NOT EXISTS idx_approvals_agent  ON approvals(agent_id);


ALTER TABLE approvals ADD COLUMN notify_channel TEXT;

ALTER TABLE approvals ADD COLUMN notify_chat_id TEXT;

ALTER TABLE approvals ADD COLUMN reminded_at TEXT;

ALTER TABLE approvals ADD COLUMN simulation TEXT;

ALTER TABLE approvals ADD COLUMN request_kind TEXT NOT NULL DEFAULT 'approval';

ALTER TABLE approvals ADD COLUMN binding_json TEXT;

ALTER TABLE approvals ADD COLUMN answer_json TEXT;

ALTER TABLE approvals ADD COLUMN invalidated_reason TEXT;

ALTER TABLE approvals ADD COLUMN on_behalf_of TEXT;

ALTER TABLE approvals ADD COLUMN origin_session_id TEXT;

ALTER TABLE approvals ADD COLUMN origin_turn_id TEXT;

ALTER TABLE approvals ADD COLUMN lease_token TEXT;

ALTER TABLE approvals ADD COLUMN lease_until TEXT;

CREATE INDEX IF NOT EXISTS idx_approvals_turn_origin
                 ON approvals(origin_session_id, origin_turn_id, status);
             CREATE TABLE IF NOT EXISTS cancelled_approval_turns (
                 session_id TEXT NOT NULL,
                 turn_id TEXT NOT NULL,
                 cancelled_at TEXT NOT NULL,
                 PRIMARY KEY(session_id, turn_id)
             );
             CREATE INDEX IF NOT EXISTS idx_cancelled_approval_turns_time
                 ON cancelled_approval_turns(cancelled_at);
             CREATE TABLE IF NOT EXISTS approval_turn_origins (
                 approval_id TEXT NOT NULL,
                 session_id TEXT NOT NULL,
                 turn_id TEXT NOT NULL,
                 PRIMARY KEY(approval_id, session_id, turn_id)
             );
             CREATE INDEX IF NOT EXISTS idx_approval_turn_origins_turn
                 ON approval_turn_origins(session_id, turn_id);
             INSERT OR IGNORE INTO approval_turn_origins(approval_id, session_id, turn_id)
                 SELECT id, origin_session_id, origin_turn_id FROM approvals
                 WHERE origin_session_id IS NOT NULL AND origin_turn_id IS NOT NULL;

ALTER TABLE cancelled_approval_turns ADD COLUMN principal TEXT;

CREATE INDEX IF NOT EXISTS idx_cancelled_turns_principal_time
             ON cancelled_approval_turns(principal, julianday(cancelled_at));

CREATE TABLE IF NOT EXISTS approval_operations (
            operation_id TEXT PRIMARY KEY, run_id TEXT NOT NULL, step_key TEXT NOT NULL,
            approval_id TEXT NOT NULL, binding_json TEXT NOT NULL, payload_json TEXT NOT NULL,
            state TEXT NOT NULL DEFAULT 'prepared', fence INTEGER NOT NULL DEFAULT 0,
            lease_owner TEXT, lease_token TEXT, lease_until INTEGER, started_at TEXT, finished_at TEXT,
            provider_key TEXT, receipt_json TEXT, error_code TEXT, operator_resolution_json TEXT,
            UNIQUE(run_id,step_key));

CREATE TABLE IF NOT EXISTS approval_authority_schema(version INTEGER NOT NULL);

ALTER TABLE main.approval_operations ADD COLUMN authority_source TEXT NOT NULL DEFAULT 'bound_human_approval';

ALTER TABLE main.approval_operations ADD COLUMN revision_grant_id TEXT;

ALTER TABLE main.approval_operations ADD COLUMN revision_grant_epoch INTEGER;

ALTER TABLE main.approval_operations ADD COLUMN revision_grant_hash TEXT;

ALTER TABLE main.approval_operations ADD COLUMN run_authority_json TEXT;

CREATE TABLE IF NOT EXISTS workflow_revision_grants (
            grant_id TEXT PRIMARY KEY,activation_id TEXT NOT NULL UNIQUE,
            workflow_id TEXT NOT NULL,workflow_revision INTEGER NOT NULL,
            revision_hash TEXT NOT NULL,skill_hash TEXT NOT NULL,fixtures_digest TEXT NOT NULL,
            activation_acceptance_id TEXT NOT NULL,grant_spec_json TEXT NOT NULL,grant_spec_hash TEXT NOT NULL,
            actor_principal TEXT NOT NULL,audience_hash TEXT NOT NULL,
            authority_epoch INTEGER NOT NULL CHECK(authority_epoch>=1),
            state TEXT NOT NULL CHECK(state IN ('prepared','active','revoked')),
            expires_at TEXT NOT NULL,created_at TEXT NOT NULL,revoked_at TEXT,revoke_reason TEXT);
            CREATE TABLE IF NOT EXISTS workflow_activation_revocations(activation_id TEXT PRIMARY KEY,
            spec_hash TEXT NOT NULL,revoked_at TEXT NOT NULL,reason TEXT NOT NULL);
            CREATE TRIGGER IF NOT EXISTS activation_revocation_immutable BEFORE UPDATE
            ON workflow_activation_revocations BEGIN SELECT RAISE(ABORT,'activation revocation immutable'); END;
            CREATE TRIGGER IF NOT EXISTS activation_revocation_retained BEFORE DELETE
            ON workflow_activation_revocations BEGIN SELECT RAISE(ABORT,'activation revocation retained'); END;
            CREATE TABLE IF NOT EXISTS workflow_grant_outbox(id TEXT PRIMARY KEY,grant_id TEXT NOT NULL,
            epoch INTEGER NOT NULL,kind TEXT NOT NULL,reason TEXT NOT NULL,delivered INTEGER NOT NULL DEFAULT 0);
            CREATE TRIGGER IF NOT EXISTS operation_run_authority_immutable BEFORE UPDATE OF run_authority_json
            ON approval_operations WHEN NEW.run_authority_json IS NOT OLD.run_authority_json BEGIN SELECT RAISE(ABORT,
            'operation run authority immutable'); END;
            CREATE INDEX IF NOT EXISTS idx_operation_revision_grant ON approval_operations(revision_grant_id,state);
            CREATE TRIGGER IF NOT EXISTS operation_authority_insert BEFORE INSERT ON approval_operations
            WHEN NOT ((NEW.authority_source='bound_human_approval' AND NEW.approval_id<>''
            AND NEW.revision_grant_id IS NULL AND NEW.revision_grant_epoch IS NULL
            AND NEW.revision_grant_hash IS NULL) OR
            (NEW.authority_source='active_workflow_revision_grant' AND NEW.approval_id=''
            AND NEW.revision_grant_id IS NOT NULL AND NEW.revision_grant_epoch>=1
            AND NEW.revision_grant_hash IS NOT NULL))
            BEGIN SELECT RAISE(ABORT,'invalid operation authority source'); END;
            CREATE TRIGGER IF NOT EXISTS operation_authority_update BEFORE UPDATE OF authority_source,approval_id,
            revision_grant_id,revision_grant_epoch,revision_grant_hash ON approval_operations
            WHEN NEW.authority_source IS NOT OLD.authority_source OR NEW.approval_id IS NOT OLD.approval_id
            OR NEW.revision_grant_id IS NOT OLD.revision_grant_id
            OR NEW.revision_grant_epoch IS NOT OLD.revision_grant_epoch
            OR NEW.revision_grant_hash IS NOT OLD.revision_grant_hash
            BEGIN SELECT RAISE(ABORT,'operation authority immutable'); END;
            CREATE TRIGGER IF NOT EXISTS revision_grant_immutable BEFORE UPDATE ON workflow_revision_grants
            WHEN NEW.grant_id IS NOT OLD.grant_id OR NEW.activation_id IS NOT OLD.activation_id
            OR NEW.workflow_id IS NOT OLD.workflow_id OR NEW.workflow_revision IS NOT OLD.workflow_revision
            OR NEW.revision_hash IS NOT OLD.revision_hash OR NEW.skill_hash IS NOT OLD.skill_hash
            OR NEW.fixtures_digest IS NOT OLD.fixtures_digest
            OR NEW.activation_acceptance_id IS NOT OLD.activation_acceptance_id
            OR NEW.grant_spec_json IS NOT OLD.grant_spec_json OR NEW.grant_spec_hash IS NOT OLD.grant_spec_hash
            OR NEW.actor_principal IS NOT OLD.actor_principal OR NEW.audience_hash IS NOT OLD.audience_hash
            OR NEW.expires_at IS NOT OLD.expires_at OR NEW.created_at IS NOT OLD.created_at
            OR NEW.authority_epoch<>OLD.authority_epoch+1 OR NOT ((OLD.state='prepared' AND NEW.state IN ('active',
            'revoked')) OR (OLD.state='active' AND NEW.state='revoked'))
            BEGIN SELECT RAISE(ABORT,'invalid revision grant transition'); END;
            CREATE TRIGGER IF NOT EXISTS revision_grant_retained BEFORE DELETE ON workflow_revision_grants BEGIN
            SELECT RAISE(ABORT,'revision grant retained'); END;
            CREATE TRIGGER IF NOT EXISTS operation_state_transition BEFORE UPDATE OF state ON approval_operations
            WHEN NEW.state IS NOT OLD.state AND NOT ((OLD.state='prepared' AND NEW.state='executing')
            OR (OLD.state='executing' AND NEW.state IN ('succeeded','failed','uncertain'))
            OR (OLD.state='uncertain' AND NEW.state IN ('succeeded','failed')))
            BEGIN SELECT RAISE(ABORT,'invalid operation state transition'); END;
            CREATE TRIGGER IF NOT EXISTS operation_contract_immutable BEFORE UPDATE OF operation_id,run_id,step_key,
            binding_json,payload_json ON approval_operations
            WHEN NEW.operation_id IS NOT OLD.operation_id OR NEW.run_id IS NOT OLD.run_id
            OR NEW.step_key IS NOT OLD.step_key OR NEW.binding_json IS NOT OLD.binding_json
            OR NEW.payload_json IS NOT OLD.payload_json
            BEGIN SELECT RAISE(ABORT,'operation contract immutable'); END;
            CREATE TRIGGER IF NOT EXISTS operation_retained BEFORE DELETE ON approval_operations BEGIN
            SELECT RAISE(ABORT,'operation retained'); END;
            CREATE TRIGGER IF NOT EXISTS approval_decision_final BEFORE UPDATE OF status ON approvals
            WHEN NEW.status IS NOT OLD.status AND OLD.status<>'pending' AND NOT (NEW.status='invalidated'
            AND OLD.status IN ('approved','denied','expired','answered'))
            BEGIN SELECT RAISE(ABORT,'approval decision is final'); END;
            CREATE TRIGGER IF NOT EXISTS approval_retained BEFORE DELETE ON approvals BEGIN
            SELECT RAISE(ABORT,'approval retained'); END;

INSERT INTO approval_authority_schema VALUES(1);
