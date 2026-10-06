//! Workflow wake-ups that must commit together with a human decision, and
//! ledger lookups the workflow runner needs for crash recovery.
use super::*;

impl ApprovalBroker {
    /// Attach `workflow.db` when a decision on this binding can resume a
    /// workflow run. No workflow store means no run that could wait on it.
    pub(super) fn attach_workflow_for_resume(
        &self,
        conn: &Connection,
        binding: &ExecutionBinding,
    ) -> Result<bool, String> {
        if binding.run_origin_kind != "workflow" || binding.resume_handler != "workflow_v1" {
            return Ok(false);
        }
        let Some(home) = self.home_dir() else {
            return Ok(false);
        };
        let home = std::fs::canonicalize(home).map_err(|_| "workflow resume home unavailable")?;
        match std::fs::symlink_metadata(home.join("workflow.db")) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err("workflow resume store unreadable".into()),
            Ok(_) => {
                self.attach_workflow_reader(conn)?;
                Ok(true)
            }
        }
    }

    /// Queue a resume for the formal run this decision belongs to. Fixture
    /// runs and activation requests have no queued run and are skipped.
    pub(super) fn write_workflow_resume(
        tx: &rusqlite::Transaction<'_>,
        binding: &ExecutionBinding,
        approval_id: &ApprovalId,
    ) -> Result<(), String> {
        let (outbox_id, payload) = crate::workflow::resume_outbox(
            &binding.run_id,
            &binding.actor_principal,
            approval_id.as_str(),
        );
        tx.execute(
            "INSERT INTO approval_workflow.workflow_outbox(outbox_id,kind,entity_id,payload_json,delivered)
                SELECT ?1,'resume',?2,?3,0 WHERE EXISTS(SELECT 1 FROM approval_workflow.workflow_runs
                WHERE run_id=?2 AND json_extract(record_json,'$.activation_id') IS NOT NULL
                AND json_extract(record_json,'$.actor')=?4)
                ON CONFLICT(outbox_id) DO NOTHING",
            params![outbox_id, binding.run_id, payload, binding.actor_principal],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// The ledger row for one workflow step, if any operation was ever
    /// prepared for it (`UNIQUE(run_id, step_key)`).
    pub async fn operation_for_step(
        &self,
        run_id: &str,
        step_key: &str,
    ) -> Result<Option<OperationRecord>, String> {
        let id: Option<String> = {
            let conn = self.store.conn.lock().await;
            conn.query_row(
                "SELECT operation_id FROM approval_operations WHERE run_id=?1 AND step_key=?2",
                params![run_id, step_key],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?
        };
        match id {
            Some(id) => self.inspect_operation(&id).await,
            None => Ok(None),
        }
    }
}

impl ApprovalBroker {
    /// Read side of the revocation outbox: each undelivered revocation with
    /// the operations of that grant still executing, then marked delivered.
    /// An executing operation is not stopped by revocation; this is how the
    /// operator learns about it.
    pub async fn drain_grant_revocations(
        &self,
    ) -> Result<Vec<(String, i64, String, Vec<String>)>, String> {
        let mut conn = self.store.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let rows: Vec<(String, String, i64, String)> = {
            let mut q = tx
                .prepare(
                    "SELECT id,grant_id,epoch,reason FROM workflow_grant_outbox
                        WHERE kind='revoke' AND delivered=0 ORDER BY rowid",
                )
                .map_err(|e| e.to_string())?;
            q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .map_err(|e| e.to_string())?
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?
        };
        let mut out = Vec::with_capacity(rows.len());
        for (id, grant, epoch, reason) in rows {
            let executing: Vec<String> = {
                let mut q = tx
                    .prepare(
                        "SELECT operation_id FROM approval_operations WHERE revision_grant_id=?1
                            AND state='executing'",
                    )
                    .map_err(|e| e.to_string())?;
                q.query_map(params![grant], |r| r.get(0))
                    .map_err(|e| e.to_string())?
                    .collect::<Result<_, _>>()
                    .map_err(|e| e.to_string())?
            };
            tx.execute(
                "UPDATE workflow_grant_outbox SET delivered=1 WHERE id=?1",
                params![id],
            )
            .map_err(|e| e.to_string())?;
            out.push((grant, epoch, reason, executing));
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(out)
    }
}
