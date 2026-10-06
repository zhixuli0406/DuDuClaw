//! Durable side-effect authority. Approval grants permission; receipts prove results.
use super::*;

/// Claim refusal when another executor's claim lease is still live: the
/// operation never began, so retrying later is safe (R-M2).
pub const OPERATION_LEASE_HELD: &str = "operation_lease_held_by_another_executor";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Prepared,
    Executing,
    Succeeded,
    Failed,
    Uncertain,
}
impl OperationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Executing => "executing",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Uncertain => "uncertain",
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperatorResolution {
    pub actor: String,
    pub reason: String,
    pub at: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationRecord {
    pub operation_id: String,
    pub run_id: String,
    pub step_key: String,
    pub approval_id: String,
    pub authority: OperationAuthority,
    pub binding: ExecutionBinding,
    pub state: OperationState,
    pub fence: i64,
    pub lease_owner: Option<String>,
    pub lease_until: Option<i64>,
    pub provider_key: Option<String>,
    pub receipt: Option<Value>,
    pub error_code: Option<String>,
    pub operator_resolution: Option<OperatorResolution>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationClaim {
    pub operation_id: String,
    pub token: String,
    pub fence: i64,
    pub owner: String,
}

impl ApprovalStore {
    pub(super) fn init_operations(conn: &Connection) -> Result<(), String> {
        conn.execute_batch("CREATE TABLE IF NOT EXISTS approval_operations (
            operation_id TEXT PRIMARY KEY, run_id TEXT NOT NULL, step_key TEXT NOT NULL,
            approval_id TEXT NOT NULL, binding_json TEXT NOT NULL, payload_json TEXT NOT NULL,
            state TEXT NOT NULL DEFAULT 'prepared', fence INTEGER NOT NULL DEFAULT 0,
            lease_owner TEXT, lease_token TEXT, lease_until INTEGER, started_at TEXT, finished_at TEXT,
            provider_key TEXT, receipt_json TEXT, error_code TEXT, operator_resolution_json TEXT,
            UNIQUE(run_id,step_key));")
            .map_err(|e| e.to_string())
    }
}

impl ApprovalBroker {
    pub async fn prepare_operation(
        &self,
        approval_id: &ApprovalId,
        step_key: &str,
        provider_key: Option<&str>,
    ) -> Result<String, String> {
        if step_key.is_empty() || step_key.len() > 256 {
            return Err("invalid step key".into());
        }
        let rec = self.get(approval_id).await?.ok_or("request not found")?;
        if rec.request_kind != RequestKind::Approval {
            return Err("question cannot prepare an action".into());
        }
        let binding = rec
            .binding
            .as_ref()
            .ok_or("legacy approval cannot prepare execution")?;
        binding.validate(&rec.payload)?;
        let mut conn = self.store.conn.lock().await;
        if binding.run_origin_kind == "workflow" && binding.resume_handler == "workflow_v1" {
            self.attach_workflow_reader(&conn)?;
        }
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        // A prepared row is only a reservation: it is created while the card
        // is still pending (computer-use registers the operation before the
        // human answers) and cannot start anything. `claim_operation` checks
        // approved + unexpired + exact binding inside the IMMEDIATE
        // transaction that moves it forward, and a denied or expired card's
        // prepared row is never claimable (F1b item 9).
        let run_authority = Self::workflow_run_authority(&tx, binding)?;
        let id = uuid::Uuid::new_v4().to_string();
        let binding_json = serde_json::to_string(binding).unwrap();
        tx.execute(
            "INSERT INTO approval_operations(operation_id,run_id,step_key,approval_id,binding_json,payload_json,
                provider_key,run_authority_json)
            VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(run_id,step_key) DO NOTHING",
            params![
                id,
                binding.run_id,
                step_key,
                approval_id.as_str(),
                binding_json,
                rec.payload.to_string(),
                provider_key,
                run_authority
            ]
        )
        .map_err(|e| e.to_string())?;
        let (existing, frozen, source, human, key, payload): (
            String,
            String,
            String,
            String,
            Option<String>,
            String
        ) = tx
            .query_row(
                "SELECT operation_id,binding_json,authority_source,approval_id,provider_key,payload_json
                    FROM approval_operations WHERE run_id=?1 AND step_key=?2",
                params![binding.run_id, step_key],
                |r|
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?
                    ))
            )
            .map_err(|e| e.to_string())?;
        if frozen != binding_json
            || source != "bound_human_approval"
            || human != approval_id.as_str()
            || key.as_deref() != provider_key
            || payload_hash(&serde_json::from_str(&payload).map_err(|_| "invalid stored payload")?)
                != payload_hash(&rec.payload)
        {
            return Err("step already exists with a different contract".into());
        }
        let frozen_run: Option<String> = tx
            .query_row(
                "SELECT run_authority_json FROM approval_operations WHERE operation_id=?1",
                params![existing],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if frozen_run != run_authority {
            return Err("step workflow authority changed".into());
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(existing)
    }

    /// Re-read host policy outside the short transaction. Task revision is checked
    /// under an attached DB read lock in the same transaction as the claim CAS.
    async fn operation_context(&self, id: &str, expected: &ExecutionBinding) -> Result<(), String> {
        let conn = self.store.conn.lock().await;
        let payload: String = conn
            .query_row(
                "SELECT payload_json FROM approval_operations WHERE operation_id=?1",
                params![id],
                |r| r.get(0),
            )
            .map_err(|_| "operation not found")?;
        expected
            .validate(&serde_json::from_str(&payload).map_err(|_| "invalid stored payload")?)?;
        drop(conn);
        expected.validate_host_files()?;
        if let Some(home) = self.home_dir() {
            if policy_revision(&home, &expected.actor_principal)? != expected.policy_revision {
                return Err("policy changed".into());
            }
        }
        Ok(())
    }
    pub(super) fn check_task(
        tx: &rusqlite::Transaction<'_>,
        binding: &ExecutionBinding,
    ) -> Result<(), String> {
        if let Some(id) = binding.task_id.as_ref() {
            let task = tx
                .query_row(
                    &format!(
                        "SELECT {} FROM approval_tasks.tasks WHERE id=?1",
                        crate::task_store::TASK_COLUMNS
                    ),
                    params![id],
                    crate::task_store::row_to_task,
                )
                .map_err(|_| "task unavailable")?;
            if task.deadline_at.as_deref().is_some_and(|s| {
                DateTime::parse_from_rfc3339(s)
                    .map_or(true, |t| Utc::now() >= t.with_timezone(&Utc))
            }) {
                return Err("task deadline expired or invalid".into());
            }
            // Use the canonical projection under the attached DB transaction.
            // Checking only revision permits delete/reinsert or corrupt projection
            // ABA; the epoch tombstone also fences byte-identical recreations.
            if Some(task.authority_revision) != binding.task_revision
                || Some(task.authority_snapshot_hash()) != binding.task_snapshot_hash
                || !task.approval_eligible()
            {
                return Err("task authority changed or task closed".into());
            }
        }
        Ok(())
    }
    pub(super) fn attach_tasks(
        &self,
        conn: &Connection,
        binding: &ExecutionBinding,
    ) -> Result<(), String> {
        if binding.task_id.is_none() {
            return Ok(());
        }
        let home = std::fs::canonicalize(self.home_dir().ok_or("task binding needs a durable home")?)
            .map_err(|_| "task binding home unavailable")?;
        let path = home.join("tasks.db");
        // Same rule as the workflow reader: the authority database and its
        // sidecars must be the real files under the canonical home.
        for suffix in ["", "-wal", "-shm"] {
            let p = PathBuf::from(format!("{}{suffix}", path.display()));
            if std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err("task authority symlink refused".into());
            }
        }
        if !path.is_file() {
            return Err("task store missing".into());
        }
        let attached: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_database_list WHERE name='approval_tasks')",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if !attached {
            conn.execute(
                "ATTACH DATABASE ?1 AS approval_tasks",
                params![path.to_string_lossy().as_ref()],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    pub async fn claim_operation(
        &self,
        id: &str,
        expected: &ExecutionBinding,
        owner: &str,
        lease_seconds: i64,
    ) -> Result<OperationClaim, String> {
        if owner.is_empty() || !(1..=300).contains(&lease_seconds) {
            return Err("invalid lease".into());
        }
        self.operation_context(id, expected).await?;
        let mut conn = self.store.conn.lock().await;
        self.attach_tasks(&conn, expected)?;
        if conn
            .query_row(
                "SELECT authority_source FROM approval_operations WHERE operation_id=?1",
                params![id],
                |r| r.get::<_, String>(0),
            )
            .map_err(|e| e.to_string())?
            == "active_workflow_revision_grant"
            || expected.run_origin_kind == "workflow" && expected.resume_handler == "workflow_v1"
        {
            self.attach_workflow_reader(&conn)?;
        }
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        Self::check_task(&tx, expected)?;
        Self::check_operation_authority(&tx, id, expected)?;
        let token = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().timestamp();
        let n = tx
            .execute(
                "UPDATE approval_operations SET lease_owner=?1,lease_token=?2,lease_until=?3,fence=fence+1
            WHERE operation_id=?4 AND state='prepared' AND binding_json=?5
            AND (lease_until IS NULL OR lease_until<=?6)",
                params![
                    owner,
                    token,
                    now + lease_seconds,
                    id,
                    serde_json::to_string(expected).unwrap(),
                    now
                ]
            )
            .map_err(|e| e.to_string())?;
        if n != 1 {
            // R-M2: a live claim by another executor (e.g. a CLI that died
            // after claiming) is not a refusal; say so, so the runner retries.
            let leased: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM approval_operations WHERE operation_id=?1
                        AND state='prepared' AND lease_until IS NOT NULL AND lease_until>?2)",
                    params![id, now],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            if leased {
                return Err(OPERATION_LEASE_HELD.into());
            }
            return Err("operation not approved, expired, consumed or already claimed".into());
        }
        let fence = tx
            .query_row(
                "SELECT fence FROM approval_operations WHERE operation_id=?1",
                params![id],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(OperationClaim {
            operation_id: id.into(),
            token,
            fence,
            owner: owner.into(),
        })
    }
    /// The last durable boundary before any side effect; callers must also run
    /// their native capability/scope/threat gates immediately before this call.
    pub async fn begin_execution(
        &self,
        claim: &OperationClaim,
        current: &ExecutionBinding,
    ) -> Result<(), String> {
        self.operation_context(&claim.operation_id, current).await?;
        let mut conn = self.store.conn.lock().await;
        self.attach_tasks(&conn, current)?;
        if conn
            .query_row(
                "SELECT authority_source FROM approval_operations WHERE operation_id=?1",
                params![claim.operation_id],
                |r| r.get::<_, String>(0),
            )
            .map_err(|e| e.to_string())?
            == "active_workflow_revision_grant"
            || current.run_origin_kind == "workflow" && current.resume_handler == "workflow_v1"
        {
            self.attach_workflow_reader(&conn)?;
        }
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        Self::check_task(&tx, current)?;
        Self::check_operation_authority(&tx, &claim.operation_id, current)?;
        let n = tx
            .execute(
                "UPDATE approval_operations SET state='executing',started_at=?1 WHERE operation_id=?2
                    AND state='prepared'
            AND lease_token=?3 AND fence=?4 AND lease_owner=?5 AND lease_until>?6 AND binding_json=?7",
                params![
                    Utc::now().to_rfc3339(),
                    claim.operation_id,
                    claim.token,
                    claim.fence,
                    claim.owner,
                    Utc::now().timestamp(),
                    serde_json::to_string(current).unwrap()
                ]
            )
            .map_err(|e| e.to_string())?;
        if n != 1 {
            return Err("execution authorization revoked or fenced".into());
        }
        tx.commit().map_err(|e| e.to_string())
    }
    pub async fn settle_operation(
        &self,
        claim: &OperationClaim,
        state: OperationState,
        receipt: Option<Value>,
        error: Option<&str>,
    ) -> Result<(), String> {
        if !matches!(
            state,
            OperationState::Succeeded | OperationState::Failed | OperationState::Uncertain
        ) || (state == OperationState::Succeeded && receipt.as_ref().is_none_or(Value::is_null))
        {
            return Err("terminal result requires an actual receipt".into());
        }
        let conn = self.store.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE approval_operations SET state=?1,receipt_json=?2,error_code=?3,finished_at=?4
            WHERE operation_id=?5 AND state='executing' AND lease_token=?6 AND fence=?7 AND lease_owner=?8
                    AND lease_until>?9",
                params![
                    state.as_str(),
                    receipt.map(|v| v.to_string()),
                    error,
                    Utc::now().to_rfc3339(),
                    claim.operation_id,
                    claim.token,
                    claim.fence,
                    claim.owner,
                    Utc::now().timestamp()
                ]
            )
            .map_err(|e| e.to_string())?;
        if n != 1 {
            return Err("stale execution receipt refused by fence".into());
        }
        Ok(())
    }
    pub async fn list_operations(&self) -> Result<Vec<OperationRecord>, String> {
        Ok(self.list_operations_page(None, 200).await?.0)
    }
    pub async fn list_operations_page(
        &self,
        before: Option<i64>,
        limit: usize,
    ) -> Result<(Vec<OperationRecord>, Option<i64>), String> {
        self.operations_page(before, limit, None).await
    }
    pub async fn inspect_operation(&self, id: &str) -> Result<Option<OperationRecord>, String> {
        if uuid::Uuid::parse_str(id).is_err() {
            return Err("invalid operation ID".into());
        }
        Ok(self
            .operations_page(None, 1, Some(id))
            .await?
            .0
            .into_iter()
            .next())
    }
    async fn operations_page(
        &self,
        before: Option<i64>,
        limit: usize,
        operation_id: Option<&str>,
    ) -> Result<(Vec<OperationRecord>, Option<i64>), String> {
        let limit = limit.clamp(1, 500);
        let conn = self.store.conn.lock().await;
        let mut q = conn
            .prepare("SELECT operation_id,run_id,step_key,approval_id,binding_json,state,fence,lease_owner,
                lease_until,provider_key,receipt_json,error_code,rowid,operator_resolution_json,authority_source,
                revision_grant_id,revision_grant_epoch,revision_grant_hash FROM approval_operations WHERE (?1 IS NULL
                OR rowid<?1) AND (?3 IS NULL OR operation_id=?3) ORDER BY rowid DESC LIMIT ?2")
            .map_err(|e| e.to_string())?;
        let mut records = q
            .query_map(params![before, limit as i64 + 1, operation_id], |r| {
                let binding: String = r.get(4)?;
                let state: String = r.get(5)?;
                let parsed = serde_json::from_str(&binding).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        4,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
                let record = OperationRecord {
                    operation_id: r.get(0)?,
                    run_id: r.get(1)?,
                    step_key: r.get(2)?,
                    approval_id: r.get(3)?,
                    authority: match r.get::<_, String>(14)?.as_str() {
                        "bound_human_approval" => OperationAuthority::BoundHumanApproval {
                            approval_id: r.get(3)?,
                        },
                        "active_workflow_revision_grant" => {
                            OperationAuthority::ActiveWorkflowRevisionGrant {
                                grant_id: r.get(15)?,
                                epoch: r.get(16)?,
                                spec_hash: r.get(17)?,
                            }
                        }
                        _ => return Err(rusqlite::Error::InvalidQuery),
                    },
                    binding: parsed,
                    state: match state.as_str() {
                        "prepared" => OperationState::Prepared,
                        "executing" => OperationState::Executing,
                        "succeeded" => OperationState::Succeeded,
                        "failed" => OperationState::Failed,
                        _ => OperationState::Uncertain,
                    },
                    fence: r.get(6)?,
                    lease_owner: r.get(7)?,
                    lease_until: r.get(8)?,
                    provider_key: r.get(9)?,
                    receipt: r
                        .get::<_, Option<String>>(10)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    error_code: r.get(11)?,
                    operator_resolution: r
                        .get::<_, Option<String>>(13)?
                        .map(|raw| {
                            serde_json::from_str(&raw).map_err(|e| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    13,
                                    rusqlite::types::Type::Text,
                                    Box::new(e),
                                )
                            })
                        })
                        .transpose()?,
                };
                Ok((record, r.get::<_, i64>(12)?))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let next = if records.len() > limit {
            records.truncate(limit);
            records.last().map(|(_, id)| *id)
        } else {
            None
        };
        Ok((
            records.into_iter().map(|(record, _)| record).collect(),
            next,
        ))
    }
    /// Boot/recovery never repeats a potentially submitted action.
    pub async fn recover_operations(&self) -> Result<usize, String> {
        let conn = self.store.conn.lock().await;
        conn.execute(
            "UPDATE approval_operations SET state='uncertain',error_code='execution_lease_lost'
                WHERE state='executing' AND lease_until<=?1",
            params![Utc::now().timestamp()]
        )
        .map_err(|e| e.to_string())
    }
    /// Called once by the gateway boot owner, never by a CLI opening the store.
    pub async fn invalidate_live_on_restart(&self) -> Result<(), String> {
        let conn = self.store.conn.lock().await;
        conn.execute_batch("UPDATE approvals SET status='invalidated',invalidated_reason='restart_requires_reobserve'
            WHERE status IN ('pending','approved') AND json_extract(binding_json,
            '$.resume_handler') IN ('computer_reobserve_v1','live_only_v1');
            UPDATE approval_operations SET state='uncertain',error_code='gateway_restarted_during_execution'
            WHERE state='executing' AND json_extract(binding_json,'$.resume_handler') IN ('computer_reobserve_v1',
            'live_only_v1');")
            .map_err(|e| e.to_string())
    }
    /// Admin reconciliation records evidence and actor, never resets to prepared.
    pub async fn resolve_uncertain(
        &self,
        id: &str,
        expected_fence: i64,
        succeeded: bool,
        receipt: Value,
        actor: &str,
        reason: &str,
    ) -> Result<(), String> {
        if actor.is_empty() || reason.trim().is_empty() || receipt.is_null() {
            return Err("reconciliation needs actor, reason and evidence".into());
        }
        let conn = self.store.conn.lock().await;
        let decision = serde_json::json!({
            "actor": actor,
            "reason": reason,
            "receipt": receipt,
            "at": Utc::now().to_rfc3339()
        });
        let n = conn
            .execute(
                "UPDATE approval_operations SET state=?1,receipt_json=?2,operator_resolution_json=?3,finished_at=?4,
                    fence=fence+1 WHERE operation_id=?5 AND state='uncertain' AND fence=?6",
                params![
                    if succeeded { "succeeded" } else { "failed" },
                    receipt.to_string(),
                    decision.to_string(),
                    Utc::now().to_rfc3339(),
                    id,
                    expected_fence
                ]
            )
            .map_err(|e| e.to_string())?;
        if n != 1 {
            return Err("reconciliation state changed".into());
        }
        Ok(())
    }
}
