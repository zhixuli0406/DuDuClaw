//! Dedicated discovery lifecycle. Ordinary worker mutations cannot use these
//! compare-and-set transitions or change the frozen specification.
use super::*;
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiscoveryDecisionReceipt {
    pub approval_id: String, pub task_id: String, pub decider: String, pub approve: bool,
    pub payload_sha256: String, pub frozen_sha256: String,
}
impl TaskStore {
    pub(crate) fn discovery_home(&self) -> Result<&Path, String> {
        self.db_path.parent().ok_or_else(|| "missing discovery home".into())
    }
    pub(crate) async fn discovery_decision_receipt(&self, id: &str) -> Result<Option<DiscoveryDecisionReceipt>, String> {
        let conn=self.conn.lock().await;
        conn.query_row("SELECT approval_id,task_id,decider,approve,payload_sha256,frozen_sha256 FROM discovery_decision_receipts WHERE approval_id=?1",
            params![id], |row| Ok(DiscoveryDecisionReceipt {approval_id:row.get(0)?,task_id:row.get(1)?,decider:row.get(2)?,
                approve:row.get::<_,i64>(3)? == 1,payload_sha256:row.get(4)?,frozen_sha256:row.get(5)?}))
            .optional().map_err(|e|e.to_string())
    }
    pub(crate) async fn save_discovery_decision_receipt(&self, receipt: &DiscoveryDecisionReceipt) -> Result<(), String> {
        let conn=self.conn.lock().await;
        conn.execute("INSERT INTO discovery_decision_receipts(approval_id,task_id,decider,approve,payload_sha256,frozen_sha256,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(approval_id) DO NOTHING",
            params![receipt.approval_id,receipt.task_id,receipt.decider,receipt.approve as i64,
                receipt.payload_sha256,receipt.frozen_sha256,Utc::now().to_rfc3339()]).map_err(|e|e.to_string())?;
        drop(conn);
        if self.discovery_decision_receipt(&receipt.approval_id).await?.as_ref() != Some(receipt) {
            return Err("discovery approval already has a different human decision intent".into());
        }
        Ok(())
    }
    /// Lifecycle work is independent of the bounded public history query.
    pub(crate) async fn discovery_active_tasks(&self) -> Result<Vec<TaskRow>, String> {
        let conn=self.conn.lock().await;
        let mut statement=conn.prepare(&format!("SELECT {TASK_COLUMNS} FROM tasks WHERE kind='discovery' AND status IN ('pending_approval','queued','in_progress') ORDER BY created_at DESC,id DESC"))
            .map_err(|e|e.to_string())?;
        statement.query_map([],row_to_task).map_err(|e|e.to_string())?
            .collect::<Result<Vec<_>,_>>().map_err(|e|e.to_string())
    }
    pub async fn discovery_tasks(&self) -> Result<Vec<TaskRow>, String> {
        let conn = self.conn.lock().await;
        let mut statement = conn.prepare(&format!("SELECT {TASK_COLUMNS} FROM tasks WHERE kind='discovery' ORDER BY created_at DESC LIMIT 1000"))
            .map_err(|e| e.to_string())?;
        statement.query_map([], row_to_task).map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }
    pub(crate) async fn discovery_creator_assignees(&self, creator: &str) -> Result<Vec<String>,String> {
        let conn=self.conn.lock().await;
        let mut statement=conn.prepare("SELECT DISTINCT assigned_to FROM tasks WHERE kind='discovery' AND created_by=?1")
            .map_err(|e|e.to_string())?;
        statement.query_map(params![creator],|row|row.get(0)).map_err(|e|e.to_string())?
            .collect::<Result<Vec<_>,_>>().map_err(|e|e.to_string())
    }
    /// Apply trusted ownership/agent ACL filters before bounding history.
    pub(crate) async fn discovery_visible_tasks(&self, creator: &str, owned: &[String], managed: &[String],
        all_agents: bool, agent_filter: Option<&str>, limit: usize) -> Result<Vec<TaskRow>,String> {
        let mut values=vec![rusqlite::types::Value::Text(creator.into())];
        let mut placeholders=|names: &[String]| {
            let indexes=names.iter().map(|name| {
                values.push(rusqlite::types::Value::Text(name.clone())); format!("?{}",values.len())
            }).collect::<Vec<_>>();
            if indexes.is_empty() {"NULL".into()} else {indexes.join(",")}
        };
        let owned=placeholders(owned); let managed=placeholders(managed);
        let access=if all_agents {"1=1".into()} else {format!("(assigned_to IN ({managed}) OR (created_by=?1 AND assigned_to IN ({owned})))")};
        let filter=if let Some(agent)=agent_filter { values.push(rusqlite::types::Value::Text(agent.into()));
            format!(" AND assigned_to=?{}",values.len()) } else {String::new()};
        values.push(rusqlite::types::Value::Integer(i64::try_from(limit).map_err(|_| "invalid discovery limit")?));
        let conn=self.conn.lock().await;
        let mut statement=conn.prepare(&format!("SELECT {TASK_COLUMNS} FROM tasks WHERE kind='discovery' AND {access}{filter} ORDER BY created_at DESC,id DESC LIMIT ?{}",values.len()))
            .map_err(|e|e.to_string())?;
        statement.query_map(rusqlite::params_from_iter(values),row_to_task).map_err(|e|e.to_string())?
            .collect::<Result<Vec<_>,_>>().map_err(|e|e.to_string())
    }
    pub async fn discovery_by_run(&self, run: &str) -> Result<Option<TaskRow>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(&format!("SELECT {TASK_COLUMNS} FROM tasks WHERE kind='discovery' AND discovery_run_id=?1"),
            params![run], row_to_task).optional().map_err(|e| e.to_string())
    }
    pub(crate) async fn authorize_discovery(&self, id: &str, approval_id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        conn.execute("UPDATE tasks SET status='queued',updated_at=?3 WHERE id=?1 AND kind='discovery' AND status='pending_approval' AND discovery_approval_id=?2",
            params![id, approval_id, Utc::now().to_rfc3339()]).map(|n| n == 1).map_err(|e| e.to_string())
    }
    pub(crate) async fn claim_discovery(&self, id: &str, worker: &str, expiry: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        conn.execute("UPDATE tasks SET status='in_progress',claimed_by=?2,claimed_at=?3,lease_renewed_at=?3,lease_expires_at=?4,updated_at=?3
            WHERE id=?1 AND kind='discovery' AND status='queued' AND claimed_by IS NULL AND archived=0",
            params![id, worker, Utc::now().to_rfc3339(), expiry]).map(|n| n == 1).map_err(|e| e.to_string())
    }
    pub(crate) async fn renew_discovery(&self, id: &str, worker: &str, expiry: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        conn.execute("UPDATE tasks SET lease_renewed_at=?3,lease_expires_at=?4,updated_at=?3
            WHERE id=?1 AND kind='discovery' AND status='in_progress' AND claimed_by=?2",
            params![id, worker, Utc::now().to_rfc3339(), expiry]).map(|n| n == 1).map_err(|e| e.to_string())
    }
    pub(crate) async fn finish_discovery(&self, id: &str, worker: &str, success: bool, summary: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        conn.execute("UPDATE tasks SET status=?3,result_summary=?4,completed_at=?5,updated_at=?5,lease_expires_at=NULL
            WHERE id=?1 AND kind='discovery' AND status='in_progress' AND claimed_by=?2",
            params![id, worker, if success {"done"} else {"failed"}, summary, Utc::now().to_rfc3339()])
            .map(|n| n == 1).map_err(|e| e.to_string())
    }
    pub(crate) async fn interrupt_discovery(&self, id: &str, scanned_expiry: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        conn.execute("UPDATE tasks SET status='failed',blocked_reason='discovery interrupted; create a new run to retry',lease_expires_at=NULL,updated_at=?3
            WHERE id=?1 AND kind='discovery' AND status='in_progress' AND lease_expires_at=?2",
            params![id, scanned_expiry, Utc::now().to_rfc3339()]).map(|n| n == 1).map_err(|e| e.to_string())
    }
    pub(crate) async fn cancel_discovery(&self, id: &str, reason: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        conn.execute("UPDATE tasks SET status='cancelled',blocked_reason=?2,updated_at=?3,completed_at=?3
            WHERE id=?1 AND kind='discovery' AND status IN ('pending_approval','queued','in_progress')",
            params![id, reason, Utc::now().to_rfc3339()]).map(|n| n == 1).map_err(|e| e.to_string())
    }
    /// Cancel only while still awaiting approval, so a withdrawal is never
    /// recorded for a request a manager authorized concurrently.
    pub(crate) async fn cancel_pending_discovery(&self, id: &str, reason: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        conn.execute("UPDATE tasks SET status='cancelled',blocked_reason=?2,updated_at=?3,completed_at=?3
            WHERE id=?1 AND kind='discovery' AND status='pending_approval'",
            params![id, reason, Utc::now().to_rfc3339()]).map(|n| n == 1).map_err(|e| e.to_string())
    }
}
