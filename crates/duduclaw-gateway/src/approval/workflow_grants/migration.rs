use super::*;

impl ApprovalStore {
    pub(in crate::approval) fn migrate_workflow_authority(conn: &Connection) -> Result<(), String> {
        let tx =
            rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| e.to_string())?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS approval_authority_schema(version INTEGER NOT NULL);" ,)
            .map_err(|e| e.to_string())?;
        let versions: Vec<i64> = {
            let mut q = tx
                .prepare("SELECT version FROM approval_authority_schema")
                .map_err(|e| e.to_string())?;
            q.query_map([], |r| r.get(0))
                .map_err(|e| e.to_string())?
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?
        };
        if versions.len() > 1 || versions.first().is_some_and(|v| *v != 1) {
            return Err("unsupported approval authority schema".into());
        }
        let columns: HashSet<String> = {
            let mut q = tx
                .prepare("PRAGMA main.table_info(approval_operations)")
                .map_err(|e| e.to_string())?;
            q.query_map([], |r| r.get(1))
                .map_err(|e| e.to_string())?
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?
        };
        for (name, ddl) in [
            (
                "authority_source",
                "authority_source TEXT NOT NULL DEFAULT 'bound_human_approval'",
            ),
            ("revision_grant_id", "revision_grant_id TEXT"),
            ("revision_grant_epoch", "revision_grant_epoch INTEGER"),
            ("revision_grant_hash", "revision_grant_hash TEXT"),
            ("run_authority_json", "run_authority_json TEXT"),
        ] {
            if !columns.contains(name) {
                tx.execute(
                    &format!("ALTER TABLE main.approval_operations ADD COLUMN {ddl}"),
                    [],
                )
                .map_err(|e| e.to_string())?;
            }
        }
        tx.execute_batch("CREATE TABLE IF NOT EXISTS workflow_revision_grants (
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
            SELECT RAISE(ABORT,'approval retained'); END;")
            .map_err(|e| e.to_string())?;
        let invalid: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM approval_operations
                    WHERE authority_source NOT IN ('bound_human_approval','active_workflow_revision_grant')
                    OR (authority_source='bound_human_approval' AND (approval_id='' OR revision_grant_id IS NOT NULL
                    OR revision_grant_epoch IS NOT NULL OR revision_grant_hash IS NOT NULL))
                    OR (authority_source='active_workflow_revision_grant' AND (approval_id<>''
                    OR revision_grant_id IS NULL OR revision_grant_epoch IS NULL OR revision_grant_hash IS NULL)))",
                [],
                |r| r.get(0)
            )
            .map_err(|e| e.to_string())?;
        if invalid {
            return Err("invalid stored operation authority".into());
        }
        if versions.is_empty() {
            tx.execute("INSERT INTO approval_authority_schema VALUES(1)", [])
                .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }
}
