-- Exact reviewed v1.79.3 workflow initializer.
PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA secure_delete=ON;

CREATE TABLE IF NOT EXISTS workflow_schema_meta (owner TEXT PRIMARY KEY,
            version INTEGER NOT NULL);
            INSERT OR IGNORE INTO workflow_schema_meta VALUES('foundation',1);
            CREATE TABLE IF NOT EXISTS workflow_revisions (workflow_id TEXT NOT NULL,revision INTEGER NOT NULL,
            revision_hash TEXT NOT NULL,record_json TEXT NOT NULL,PRIMARY KEY(workflow_id,revision));
            CREATE TRIGGER IF NOT EXISTS workflow_revision_immutable BEFORE UPDATE ON workflow_revisions BEGIN SELECT
            RAISE(ABORT,'workflow revision immutable'); END;
            CREATE TRIGGER IF NOT EXISTS workflow_revision_no_delete BEFORE DELETE ON workflow_revisions BEGIN SELECT
            RAISE(ABORT,'workflow revision retained'); END;
            CREATE TABLE IF NOT EXISTS workflow_candidate_revisions(workflow_id TEXT NOT NULL,
            revision INTEGER NOT NULL,revision_hash TEXT NOT NULL,record_json TEXT NOT NULL,PRIMARY KEY(workflow_id,
            revision));
            CREATE TRIGGER IF NOT EXISTS workflow_candidate_immutable BEFORE UPDATE ON workflow_candidate_revisions
            BEGIN SELECT RAISE(ABORT,'workflow candidate immutable'); END;
            CREATE TRIGGER IF NOT EXISTS workflow_candidate_retained BEFORE DELETE ON workflow_candidate_revisions
            BEGIN SELECT RAISE(ABORT,'workflow candidate retained'); END;
            CREATE TABLE IF NOT EXISTS workflow_runs (run_id TEXT PRIMARY KEY,trigger_key TEXT NOT NULL UNIQUE,
            workflow_id TEXT NOT NULL,revision INTEGER NOT NULL,workflow_hash TEXT NOT NULL,record_json TEXT NOT NULL,
            status TEXT NOT NULL,created_at TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS workflow_steps (run_id TEXT NOT NULL,step_id TEXT NOT NULL,
            position INTEGER NOT NULL,status TEXT NOT NULL,record_json TEXT NOT NULL,PRIMARY KEY(run_id,step_id));
            CREATE TRIGGER IF NOT EXISTS workflow_run_authority_immutable BEFORE UPDATE ON workflow_runs
            WHEN json_extract(NEW.record_json,'$.run_id') IS NOT json_extract(OLD.record_json,'$.run_id')
            OR json_extract(NEW.record_json,'$.trigger_key') IS NOT json_extract(OLD.record_json,'$.trigger_key')
            OR json_extract(NEW.record_json,'$.trigger') IS NOT json_extract(OLD.record_json,'$.trigger')
            OR json_extract(NEW.record_json,'$.workflow_id') IS NOT json_extract(OLD.record_json,'$.workflow_id')
            OR json_extract(NEW.record_json,'$.revision') IS NOT json_extract(OLD.record_json,'$.revision')
            OR json_extract(NEW.record_json,'$.workflow_hash') IS NOT json_extract(OLD.record_json,'$.workflow_hash')
            OR json_extract(NEW.record_json,'$.skill_hash') IS NOT json_extract(OLD.record_json,'$.skill_hash')
            OR json_extract(NEW.record_json,'$.actor') IS NOT json_extract(OLD.record_json,'$.actor')
            OR json_extract(NEW.record_json,'$.creator_grant') IS NOT json_extract(OLD.record_json,'$.creator_grant')
            OR json_extract(NEW.record_json,'$.audience') IS NOT json_extract(OLD.record_json,'$.audience')
            OR json_extract(NEW.record_json,'$.task') IS NOT json_extract(OLD.record_json,'$.task')
            OR json_extract(NEW.record_json,'$.input') IS NOT json_extract(OLD.record_json,'$.input')
            OR json_extract(NEW.record_json,'$.input_hash') IS NOT json_extract(OLD.record_json,'$.input_hash')
            OR json_extract(NEW.record_json,'$.input_observed_at') IS NOT json_extract(OLD.record_json,
            '$.input_observed_at') OR json_extract(NEW.record_json,
            '$.policy_revision') IS NOT json_extract(OLD.record_json,'$.policy_revision')
            OR json_extract(NEW.record_json,'$.environment_hash') IS NOT json_extract(OLD.record_json,
            '$.environment_hash') OR json_extract(NEW.record_json,'$.grant') IS NOT json_extract(OLD.record_json,
            '$.grant') OR json_extract(NEW.record_json,'$.activation_id') IS NOT json_extract(OLD.record_json,
            '$.activation_id') OR json_extract(NEW.record_json,'$.deadline_at') IS NOT json_extract(OLD.record_json,
            '$.deadline_at') OR json_extract(NEW.record_json,'$.budget') IS NOT json_extract(OLD.record_json,
            '$.budget') OR json_extract(NEW.record_json,'$.created_at') IS NOT json_extract(OLD.record_json,
            '$.created_at') OR json_extract(NEW.record_json,'$.decision_context') IS NOT json_extract(OLD.record_json,
            '$.decision_context') BEGIN SELECT RAISE(ABORT,'workflow run authority immutable'); END;
            CREATE TABLE IF NOT EXISTS workflow_run_leases(run_id TEXT PRIMARY KEY,token TEXT NOT NULL,
            lease_until INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS workflow_fixture_run_requests(run_id TEXT PRIMARY KEY,
            request_json TEXT NOT NULL,evidence_json TEXT);
            CREATE TRIGGER IF NOT EXISTS workflow_fixture_final_immutable BEFORE UPDATE
            ON workflow_fixture_run_requests WHEN OLD.evidence_json IS NOT NULL BEGIN SELECT RAISE(ABORT,
            'fixture execution evidence immutable'); END;
            CREATE TABLE IF NOT EXISTS workflow_artifact_commits(record_id TEXT PRIMARY KEY,run_id TEXT NOT NULL,
            step_id TEXT NOT NULL,content_hash TEXT NOT NULL,content_json TEXT NOT NULL,audience_json TEXT NOT NULL,
            receipt_json TEXT NOT NULL);
            CREATE TRIGGER IF NOT EXISTS workflow_artifact_immutable BEFORE UPDATE ON workflow_artifact_commits BEGIN
            SELECT RAISE(ABORT,'workflow artifact immutable'); END;
            CREATE TRIGGER IF NOT EXISTS workflow_artifact_retained BEFORE DELETE ON workflow_artifact_commits BEGIN
            SELECT RAISE(ABORT,'workflow artifact retained'); END;
            CREATE TRIGGER IF NOT EXISTS workflow_completed_step_immutable BEFORE UPDATE ON workflow_steps
            WHEN OLD.status='succeeded' BEGIN SELECT RAISE(ABORT,'workflow completed step immutable'); END;
            CREATE TRIGGER IF NOT EXISTS workflow_step_retained BEFORE DELETE ON workflow_steps BEGIN SELECT
            RAISE(ABORT,'workflow step retained'); END;

            CREATE TABLE IF NOT EXISTS workflow_activations (activation_id TEXT PRIMARY KEY,workflow_id TEXT NOT NULL,
            revision INTEGER NOT NULL,material_hash TEXT NOT NULL,acceptance_id TEXT NOT NULL,grant_id TEXT,
            grant_epoch INTEGER,state TEXT NOT NULL,record_json TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS workflow_outbox (outbox_id TEXT PRIMARY KEY,kind TEXT NOT NULL,
            entity_id TEXT NOT NULL,payload_json TEXT NOT NULL,delivered INTEGER NOT NULL DEFAULT 0);
            CREATE INDEX IF NOT EXISTS idx_workflow_runs_status ON workflow_runs(status);
            CREATE TABLE IF NOT EXISTS workflow_failure_resets(reset_id TEXT PRIMARY KEY,
            activation_id TEXT NOT NULL,reset_at TEXT NOT NULL,reset_by TEXT NOT NULL,reason TEXT NOT NULL);
            CREATE TRIGGER IF NOT EXISTS workflow_failure_reset_immutable BEFORE UPDATE ON workflow_failure_resets
            BEGIN SELECT RAISE(ABORT,'workflow failure reset immutable'); END;
            CREATE TRIGGER IF NOT EXISTS workflow_failure_reset_retained BEFORE DELETE ON workflow_failure_resets
            BEGIN SELECT RAISE(ABORT,'workflow failure reset retained'); END;
            CREATE TABLE IF NOT EXISTS workflow_cost_entries(entry_id TEXT PRIMARY KEY,run_id TEXT NOT NULL,
            workflow_id TEXT NOT NULL,formal INTEGER NOT NULL,month TEXT NOT NULL,step_id TEXT NOT NULL,
            attempt INTEGER NOT NULL,kind TEXT NOT NULL,amount_micros INTEGER NOT NULL CHECK(amount_micros>=0),
            basis TEXT NOT NULL,created_at TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS idx_workflow_cost_run ON workflow_cost_entries(run_id);
            CREATE INDEX IF NOT EXISTS idx_workflow_outbox_undelivered ON workflow_outbox(delivered)
            WHERE delivered=0;
            CREATE INDEX IF NOT EXISTS idx_workflow_cost_month ON workflow_cost_entries(workflow_id,month);
            CREATE TRIGGER IF NOT EXISTS workflow_cost_immutable BEFORE UPDATE ON workflow_cost_entries
            BEGIN SELECT RAISE(ABORT,'workflow cost entry immutable'); END;
            CREATE TRIGGER IF NOT EXISTS workflow_cost_retained BEFORE DELETE ON workflow_cost_entries
            BEGIN SELECT RAISE(ABORT,'workflow cost entry retained'); END;

CREATE TRIGGER IF NOT EXISTS workflow_run_principal_immutable
            BEFORE UPDATE OF record_json ON workflow_runs
            WHEN json_extract(NEW.record_json,'$.on_behalf_of') IS NOT
                 json_extract(OLD.record_json,'$.on_behalf_of')
            BEGIN SELECT RAISE(ABORT,'workflow run principal immutable'); END;

CREATE TABLE IF NOT EXISTS workflow_schema_meta(owner TEXT PRIMARY KEY,version INTEGER NOT NULL);
      INSERT OR IGNORE INTO workflow_schema_meta VALUES('evidence',1);
      CREATE TABLE IF NOT EXISTS review_snapshots(snapshot_id TEXT PRIMARY KEY,task_id TEXT NOT NULL,
        authority_revision INTEGER NOT NULL,snapshot_hash TEXT NOT NULL,record_json TEXT NOT NULL);
      CREATE TRIGGER IF NOT EXISTS review_snapshot_immutable BEFORE UPDATE ON review_snapshots BEGIN SELECT
        RAISE(ABORT,'review snapshot immutable'); END;
      CREATE TRIGGER IF NOT EXISTS review_snapshot_retained BEFORE DELETE ON review_snapshots BEGIN SELECT
        RAISE(ABORT,'review snapshot retained'); END;
      CREATE TABLE IF NOT EXISTS review_acceptances(snapshot_id TEXT PRIMARY KEY,snapshot_hash TEXT NOT NULL,
        record_json TEXT NOT NULL);
      CREATE TRIGGER IF NOT EXISTS review_acceptance_immutable BEFORE UPDATE ON review_acceptances BEGIN SELECT
        RAISE(ABORT,'review acceptance immutable'); END;
      CREATE TRIGGER IF NOT EXISTS review_acceptance_retained BEFORE DELETE ON review_acceptances BEGIN SELECT
        RAISE(ABORT,'review acceptance retained'); END;
      CREATE TABLE IF NOT EXISTS workflow_drafts(draft_id TEXT NOT NULL,revision INTEGER NOT NULL,owner TEXT NOT NULL,
        draft_hash TEXT NOT NULL,disabled INTEGER NOT NULL CHECK(disabled=1),record_json TEXT NOT NULL,
        PRIMARY KEY(draft_id,revision));
      CREATE TRIGGER IF NOT EXISTS workflow_draft_immutable BEFORE UPDATE ON workflow_drafts BEGIN SELECT RAISE(ABORT,
        'workflow draft immutable'); END;
      CREATE TRIGGER IF NOT EXISTS workflow_draft_retained BEFORE DELETE ON workflow_drafts BEGIN SELECT RAISE(ABORT,
        'workflow draft retained'); END;
      CREATE TABLE IF NOT EXISTS workflow_fixture_results(run_id TEXT PRIMARY KEY,draft_id TEXT NOT NULL,
        revision INTEGER NOT NULL,fixture_id TEXT NOT NULL,evidence_hash TEXT NOT NULL,record_json TEXT NOT NULL,
        assertions_json TEXT NOT NULL);
      CREATE TRIGGER IF NOT EXISTS workflow_fixture_immutable BEFORE UPDATE ON workflow_fixture_results BEGIN SELECT
        RAISE(ABORT,'fixture result immutable'); END;
      CREATE TRIGGER IF NOT EXISTS workflow_fixture_retained BEFORE DELETE ON workflow_fixture_results BEGIN SELECT
        RAISE(ABORT,'fixture result retained'); END;
      CREATE TABLE IF NOT EXISTS review_artifact_audiences(snapshot_id TEXT NOT NULL,task_id TEXT NOT NULL,
        agent_id TEXT NOT NULL,archived_name TEXT NOT NULL,audience_json TEXT NOT NULL,PRIMARY KEY(snapshot_id,
        agent_id,archived_name));
      CREATE TRIGGER IF NOT EXISTS review_artifact_audience_immutable BEFORE UPDATE ON review_artifact_audiences BEGIN
        SELECT RAISE(ABORT,'artifact audience immutable'); END;
      CREATE TRIGGER IF NOT EXISTS review_artifact_audience_retained BEFORE DELETE ON review_artifact_audiences BEGIN
        SELECT RAISE(ABORT,'artifact audience retained'); END;
