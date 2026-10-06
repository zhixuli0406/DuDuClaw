//! Typed requests and immutable authorization snapshots. Transport is never authority.
use super::*;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    #[default]
    Approval,
    Question,
    Invalid,
}
impl RequestKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::Question => "question",
            Self::Invalid => "invalid",
        }
    }
    pub(crate) fn from_db(s: &str) -> Self {
        match s {
            "approval" => Self::Approval,
            "question" => Self::Question,
            _ => Self::Invalid,
        }
    }
}

/// `action_kind` of a workflow activation card. Accepting a workflow version
/// grants a standing authority, so only a current Admin decides it, in the
/// dashboard only (see `approval_notify::is_dashboard_only_kind`).
pub const WORKFLOW_ACTIVATION_KIND: &str = "workflow_activation";

fn is_activation_payload(payload: &Value) -> bool {
    payload.get("kind").and_then(Value::as_str) == Some(WORKFLOW_ACTIVATION_KIND)
}

/// Set exclusively by a verified inbound adapter. No credential is stored here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionContext {
    pub channel: String,
    pub account_id: String,
    pub conversation_id: String,
    pub principal_id: String,
}
impl DecisionContext {
    pub fn validate(&self) -> Result<(), String> {
        for v in [
            &self.channel,
            &self.account_id,
            &self.conversation_id,
            &self.principal_id,
        ] {
            if v.is_empty() || v.len() > 512 || v.chars().any(char::is_control) {
                return Err("incomplete decision identity".into());
            }
        }
        Ok(())
    }
}

tokio::task_local! { pub static CURRENT_DECISION_CONTEXT: Option<DecisionContext>; }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionBinding {
    pub schema_version: u32,
    pub run_id: String,
    #[serde(default)]
    pub run_origin_kind: String,
    pub actor_principal: String,
    pub decision_context: DecisionContext,
    pub task_id: Option<String>,
    pub task_revision: Option<i64>,
    pub task_snapshot_hash: Option<String>,
    pub payload_hash: String,
    pub policy_revision: String,
    pub cwd: Option<String>,
    pub environment_hash: String,
    pub file_hashes: std::collections::BTreeMap<String, String>,
    pub expires_at: String,
    pub resume_handler: String,
    pub resume_version: u32,
}
impl ExecutionBinding {
    pub fn validate(&self, payload: &Value) -> Result<(), String> {
        self.decision_context.validate()?;
        if self.schema_version != 1
            || self.resume_version != 1
            || self.run_id.is_empty()
            || !matches!(
                self.run_origin_kind.as_str(),
                "ingress" | "computer_session" | "workflow"
            )
            || self.actor_principal.is_empty()
            || self.policy_revision.is_empty()
            || self.environment_hash.is_empty()
            || self.payload_hash != payload_hash(payload)
            || !matches!(
                self.resume_handler.as_str(),
                "live_only_v1"
                    | "computer_reobserve_v1"
                    | "workflow_v1"
                    | "goal_kickoff_v1"
                    | "question_data_v1"
            )
            || self.task_id.is_some() != self.task_revision.is_some()
            || self.task_id.is_some() != self.task_snapshot_hash.is_some()
        {
            return Err("invalid or unsupported execution binding".into());
        }
        let expiry =
            DateTime::parse_from_rfc3339(&self.expires_at).map_err(|_| "invalid expiry")?;
        if expiry <= Utc::now() {
            return Err("request expired".into());
        }
        Ok(())
    }
    pub(super) fn validate_host_files(&self) -> Result<(), String> {
        if let Some(cwd) = self.cwd.as_deref() {
            if !Path::new(cwd).is_absolute() || !Path::new(cwd).is_dir() {
                return Err("execution cwd unavailable".into());
            }
        }
        for (path, expected) in &self.file_hashes {
            let path = Path::new(path);
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                Path::new(
                    self.cwd
                        .as_deref()
                        .ok_or("relative file binding requires cwd")?,
                )
                .join(path)
            };
            let bytes = std::fs::read(path).map_err(|_| "bound file unreadable")?;
            if hex::encode(Sha256::digest(bytes)) != *expected {
                return Err("bound file changed".into());
            }
        }
        Ok(())
    }
    pub fn digest(&self) -> String {
        payload_hash(&serde_json::to_value(self).expect("serializable binding"))
    }
}

/// Canonical key order makes identical typed JSON stable across writers.
pub fn payload_hash(value: &Value) -> String {
    fn canonical(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let sorted: std::collections::BTreeMap<_, _> =
                    m.iter().map(|(k, v)| (k.clone(), canonical(v))).collect();
                serde_json::to_value(sorted).unwrap()
            }
            Value::Array(a) => Value::Array(a.iter().map(canonical).collect()),
            _ => v.clone(),
        }
    }
    hex::encode(Sha256::digest(canonical(value).to_string().as_bytes()))
}

/// Authority snapshot re-read by the host immediately before claim and
/// execute: a hash of the normalized authority fields only (see
/// `policy_snapshot`). Missing mandatory agent policy or unreadable policy
/// fails closed; no secrets are exposed.
pub fn policy_revision(home: &Path, actor: &str) -> Result<String, String> {
    Ok(super::policy_snapshot::revision_of(
        &super::policy_snapshot::policy_digests(home, actor)?,
    ))
}

impl ApprovalBroker {
    /// Commit before notifying: an immediate reply can never race a missing row.
    pub async fn request_bound(
        &self,
        kind: RequestKind,
        agent_id: &str,
        summary: &str,
        payload: Value,
        binding: ExecutionBinding,
    ) -> Result<ApprovalId, String> {
        let rec = self
            .bound_record(ApprovalId::new(), kind, agent_id, summary, payload, binding)
            .await?;
        self.store.insert(&rec).await?;
        // Bound callers own delivery to the exact configured account; no fallback fan-out.
        Ok(rec.id)
    }

    /// Same as [`Self::request_bound`] but keyed: the id is derived from
    /// `key`, so a retry after a crash or a lost reply returns the one
    /// existing card instead of creating a second decidable card. A stored
    /// card with a different kind, actor, payload or binding is refused.
    pub async fn request_bound_keyed(
        &self,
        key: &str,
        kind: RequestKind,
        agent_id: &str,
        summary: &str,
        payload: Value,
        binding: ExecutionBinding,
    ) -> Result<ApprovalId, String> {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(format!("duduclaw-keyed-approval:{key}").as_bytes());
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        let id = ApprovalId::from(
            uuid::Builder::from_random_bytes(bytes)
                .into_uuid()
                .to_string(),
        );
        let rec = self
            .bound_record(id, kind, agent_id, summary, payload, binding)
            .await?;
        self.store.insert_if_absent(&rec).await?;
        let stored = self.get(&rec.id).await?.ok_or("keyed request missing")?;
        if stored.request_kind != rec.request_kind
            || stored.agent_id != rec.agent_id
            || stored.binding != rec.binding
            || payload_hash(&stored.payload) != payload_hash(&rec.payload)
        {
            return Err("keyed request already exists with a different contract".into());
        }
        Ok(stored.id)
    }

    pub(super) async fn bound_record(
        &self,
        id: ApprovalId,
        kind: RequestKind,
        agent_id: &str,
        summary: &str,
        payload: Value,
        binding: ExecutionBinding,
    ) -> Result<ApprovalRecord, String> {
        if kind == RequestKind::Invalid {
            return Err("invalid request kind".into());
        }
        binding.validate(&payload)?;
        if agent_id != binding.actor_principal {
            return Err("request actor does not own the employee identity".into());
        }
        self.validate_task_snapshot(&binding).await?;
        let now = Utc::now();
        let expiry = DateTime::parse_from_rfc3339(&binding.expires_at)
            .map_err(|_| "invalid expiry")?
            .with_timezone(&Utc);
        Ok(ApprovalRecord {
            id,
            agent_id: agent_id.into(),
            action_kind: if kind == RequestKind::Question {
                "bound_question"
            } else if is_activation_payload(&payload) {
                WORKFLOW_ACTIVATION_KIND
            } else {
                "bound_action"
            }
            .into(),
            summary: summary.into(),
            payload,
            status: ApprovalStatus::Pending,
            created_at: now.to_rfc3339(),
            decided_at: None,
            decided_by: None,
            ttl_seconds: (expiry - now).num_seconds().max(1),
            notify_channel: Some(binding.decision_context.channel.clone()),
            notify_chat_id: Some(binding.decision_context.conversation_id.clone()),
            reminded_at: None,
            simulation: None,
            request_kind: kind,
            binding: Some(binding),
            answer: None,
            invalidated_reason: None,
        })
    }

    async fn validate_task_snapshot(&self, binding: &ExecutionBinding) -> Result<(), String> {
        if let Some(id) = binding.task_id.as_ref() {
            let home = self.home_dir().ok_or("task requires durable store")?;
            let store = crate::task_store::TaskStore::open(&home)?;
            let snapshot = store.authority_snapshot(id).await?.ok_or("task missing")?;
            if !snapshot.eligible
                || Some(snapshot.revision) != binding.task_revision
                || Some(snapshot.hash) != binding.task_snapshot_hash
            {
                return Err("task contract changed or task is closed".into());
            }
        }
        Ok(())
    }

    pub async fn decide_bound(
        &self,
        id: &ApprovalId,
        context: &DecisionContext,
        approve: bool,
    ) -> Result<(), String> {
        self.resolve_bound(
            id,
            context,
            RequestKind::Approval,
            if approve {
                ApprovalStatus::Approved
            } else {
                ApprovalStatus::Denied
            },
            None,
            None,
        )
        .await
    }
    pub async fn answer_question(
        &self,
        id: &ApprovalId,
        context: &DecisionContext,
        answer: Value,
    ) -> Result<(), String> {
        let text = answer.as_str().ok_or("question answer must be text")?;
        if text.trim().is_empty() || text.len() > 8192 {
            return Err("question answer empty or too long".into());
        }
        let row = self.get(id).await?.ok_or("request not found")?;
        if let Some(options) = row.payload.get("options").and_then(Value::as_array) {
            if options.is_empty()
                || !options.iter().all(Value::is_string)
                || !options.contains(&answer)
            {
                return Err("answer is not an offered option".into());
            }
        }
        self.resolve_bound(
            id,
            context,
            RequestKind::Question,
            ApprovalStatus::Answered,
            Some(answer),
            None,
        )
        .await
    }
    async fn resolve_bound(
        &self,
        id: &ApprovalId,
        context: &DecisionContext,
        kind: RequestKind,
        status: ApprovalStatus,
        answer: Option<Value>,
        dashboard: Option<&duduclaw_auth::UserContext>,
    ) -> Result<(), String> {
        context.validate()?;
        let rec = self.get(id).await?.ok_or("request not found")?;
        let binding = rec
            .binding
            .as_ref()
            .ok_or("legacy request has no authorization binding")?;
        binding.validate(&rec.payload)?;
        if rec.request_kind != kind || (dashboard.is_none() && &binding.decision_context != context)
        {
            return Err("request kind or decision identity mismatch".into());
        }
        // An activation is decided by a current Admin in the dashboard, never
        // by a channel press, whatever the press's context says.
        if (rec.action_kind == WORKFLOW_ACTIVATION_KIND || is_activation_payload(&rec.payload))
            && dashboard.is_none()
        {
            return Err("workflow activation is decided by an Admin in the dashboard".into());
        }
        if let Some(home) = self.home_dir() {
            if policy_revision(&home, &binding.actor_principal)? != binding.policy_revision {
                return Err("policy changed; request a new approval".into());
            }
        }
        binding.validate_host_files()?;
        self.validate_task_snapshot(binding).await?;
        let mut conn = self.store.conn.lock().await;
        self.attach_tasks(&conn, binding)?;
        let resume_workflow = self.attach_workflow_for_resume(&conn, binding)?;
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        Self::check_task(&tx, binding)?;
        let n = tx
            .execute(
                "UPDATE approvals SET status=?1, decided_at=?2, decided_by=?3, answer_json=?4
            WHERE id=?5 AND status='pending' AND request_kind=?6 AND binding_json=?7
            AND julianday(?2) < julianday(json_extract(binding_json,'$.expires_at'))
            AND julianday(?2) < julianday(created_at) + ttl_seconds/86400.0",
                params![
                    status.as_str(),
                    Utc::now().to_rfc3339(),
                    dashboard
                        .map(|c| format!("dashboard:{}", c.user_id))
                        .unwrap_or_else(|| format!("{}:{}", context.channel, context.principal_id)),
                    answer.map(|v| v.to_string()),
                    id.as_str(),
                    kind.as_str(),
                    serde_json::to_string(binding).unwrap()
                ],
            )
            .map_err(|e| e.to_string())?;
        if n != 1 {
            return Err("request expired or decided concurrently".into());
        }
        // A decision on a workflow step wakes the same run in the same
        // transaction; the runner still re-checks every gate on resume.
        if resume_workflow {
            Self::write_workflow_resume(&tx, binding, id)?;
        }
        tx.commit().map_err(|e| e.to_string())
    }

    pub(crate) fn require_current_dashboard_role(
        &self,
        ctx: &duduclaw_auth::UserContext,
        min: duduclaw_auth::UserRole,
    ) -> Result<(), String> {
        let home = self
            .home_dir()
            .ok_or("dashboard decisions require durable identity")?;
        require_current_dashboard_role_in_home(&home, ctx, min)
    }

    /// Dashboard is a distinct, authenticated managerial authority. It does
    /// not fabricate the channel principal and cannot answer private questions.
    pub async fn decide_bound_dashboard(
        &self,
        id: &ApprovalId,
        ctx: &duduclaw_auth::UserContext,
        approve: bool,
    ) -> Result<(), String> {
        let rec = self.get(id).await?.ok_or("request missing")?;
        let activation =
            rec.action_kind == WORKFLOW_ACTIVATION_KIND || is_activation_payload(&rec.payload);
        self.require_current_dashboard_role(
            ctx,
            if activation {
                duduclaw_auth::UserRole::Admin
            } else {
                duduclaw_auth::UserRole::Manager
            },
        )?;
        let context = rec
            .binding
            .as_ref()
            .ok_or("binding missing")?
            .decision_context
            .clone();
        self.resolve_bound(
            id,
            &context,
            RequestKind::Approval,
            if approve {
                ApprovalStatus::Approved
            } else {
                ApprovalStatus::Denied
            },
            None,
            Some(ctx),
        )
        .await
    }

    pub async fn invalidate_request(&self, id: &ApprovalId, reason: &str) -> Result<(), String> {
        let conn = self.store.conn.lock().await;
        conn.execute(
            "UPDATE approvals SET status='invalidated',invalidated_reason=?1 WHERE id=?2 AND status IN ('pending',
                'approved')",
            params![reason, id.as_str()]
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// Read fresh identity without creating an unrelated approvals database.
pub(crate) fn require_current_dashboard_role_in_home(
    home: &Path,
    ctx: &duduclaw_auth::UserContext,
    min: duduclaw_auth::UserRole,
) -> Result<(), String> {
    if !ctx.has_role(min) {
        return Err("dashboard authority required".into());
    }
    let Some(db) = crate::decision_notify::open_user_db(home)? else {
        return if !home.join("users.db").exists() && ctx.user_id == "system" {
            Ok(())
        } else {
            Err("identity store unavailable".into())
        };
    };
    if ctx.user_id == "system"
        && db
            .list_users()
            .map_err(|_| "identity store unreadable")?
            .is_empty()
    {
        return Ok(());
    }
    let user = db
        .get_user(&ctx.user_id)
        .map_err(|_| "identity lookup failed")?
        .ok_or("decision principal unavailable")?;
    if user.status != duduclaw_auth::UserStatus::Active || !user.role.at_least(min) {
        return Err("decision authority revoked".into());
    }
    Ok(())
}
