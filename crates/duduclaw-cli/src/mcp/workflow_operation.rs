//! Service-only workflow extension at the existing MCP dispatcher choke point.
//! The session authenticates transport; approvals.db remains the effect authority.
use crate::mcp_auth::Principal;
use chrono::Utc;
use duduclaw_core::workflow_mcp::*;
use duduclaw_gateway::approval::{
    ApprovalBroker, ApprovalStatus, OperationClaim, OperationRecord,
    OperationState, RequestKind, payload_hash, policy_revision,
};
use duduclaw_gateway::workflow::schema::{
    ApprovedWorkflowRevision, InputRef, StepAction, StepDefinition, StepEvidence,
    WorkflowEnvironment, WorkflowRun,
};
use rusqlite::OptionalExtension;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub(crate) const ENV_SESSION: &str = "DUDUCLAW_WORKFLOW_SESSION_ID";
pub(crate) const ENV_SECRET: &str = "DUDUCLAW_WORKFLOW_SESSION_SECRET";
pub(crate) const EFFECT_TOOLS: &[&str] = &["tasks_update", "update_cron_task"];
pub(crate) const READ_TOOLS: &[&str] = &["web_fetch_cached"];

pub(crate) struct WorkflowSession {
    pub id: String,
    actor: String,
    secret: Vec<u8>,
    home: PathBuf,
    prepared: Mutex<HashMap<String, PrepareTicket>>,
}
impl WorkflowSession {
    /// Called only by the stdio server. HTTP/SSE constructors never attach it.
    pub fn from_env(
        home: &Path,
        actor: &str,
        principal: &Principal,
    ) -> Result<Option<Arc<Self>>, String> {
        let id = std::env::var(ENV_SESSION).ok();
        let secret = std::env::var(ENV_SECRET).ok();
        if id.is_none() && secret.is_none() {
            return Ok(None);
        }
        let id = id.ok_or("incomplete workflow session")?;
        let secret = secret.ok_or("incomplete workflow session")?;
        if id.is_empty()
            || id.len() > 128
            || !id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
            || principal.is_external
            || principal.client_id != duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID
            || crate::mcp_namespace::verified_employee_from_env(home).as_deref() != Some(actor)
        {
            return Err("workflow requires a verified employee service stdio session".into());
        }
        if secret.len() != 64 || !secret.is_ascii() {
            return Err("invalid workflow session secret".into());
        }
        let secret: Result<Vec<u8>, _> = (0..64)
            .step_by(2)
            .map(|i| u8::from_str_radix(&secret[i..i + 2], 16))
            .collect();
        Ok(Some(Arc::new(Self {
            id,
            actor: actor.into(),
            secret: secret.map_err(|_| "invalid workflow session secret")?,
            home: std::fs::canonicalize(home).map_err(|e| e.to_string())?,
            prepared: Mutex::new(HashMap::new()),
        })))
    }
    fn verify<T: serde::Serialize>(
        &self,
        domain: &str,
        value: &T,
        mac: &str,
    ) -> Result<(), String> {
        verify(&self.secret, domain, &unsigned(value)?, mac)
    }
    fn environment_hash(&self) -> Result<String, String> {
        let cwd = std::fs::canonicalize(self.home.join("agents").join(&self.actor))
            .map_err(|e| e.to_string())?;
        Ok(WorkflowEnvironment {
            schema_version: 1,
            binary_version: env!("CARGO_PKG_VERSION").into(),
            binary_hash: current_binary_hash()?,
            home: self.home.to_string_lossy().into_owned(),
            cwd: cwd.to_string_lossy().into_owned(),
            receipt_adapter_version: 1,
        }
        .hash())
    }
}
/// SHA-256 of this process's executable, computed once per process: the
/// running image does not change. A failed read is never cached.
pub(crate) fn current_binary_hash() -> Result<String, String> {
    use sha2::{Digest, Sha256};
    static CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    if let Some(hash) = CACHE.get() {
        return Ok(hash.clone());
    }
    let binary = std::env::current_exe().map_err(|e| e.to_string())?;
    let bytes = std::fs::read(binary).map_err(|e| e.to_string())?;
    let hash = format!("{:x}", Sha256::digest(bytes));
    Ok(CACHE.get_or_init(|| hash).clone())
}
mod rows;
use rows::{fresh, read_optional_row, read_row};
struct StoredCall {
    run: WorkflowRun,
    revision: ApprovedWorkflowRevision,
    evidence: StepEvidence,
    step: StepDefinition,
}
impl StoredCall {
    fn read(
        session: &WorkflowSession,
        run_id: &str,
        step_id: &str,
        tool: &str,
        args: &Value,
    ) -> Result<Self, String> {
        let run: WorkflowRun = read_row(
            &session.home,
            "SELECT record_json FROM workflow_runs WHERE run_id=?1",
            &[&run_id],
        )?;
        let revision: ApprovedWorkflowRevision = match read_optional_row(
            &session.home,
            "SELECT record_json FROM workflow_revisions WHERE workflow_id=?1 AND revision=?2",
            &[&run.workflow_id, &run.revision],
        )? {
            Some(revision) => revision,
            None if matches!(
                run.trigger,
                duduclaw_gateway::workflow::schema::Trigger::Fixture { .. }
            ) =>
            {
                read_row(
                    &session.home,
                    "SELECT record_json FROM workflow_candidate_revisions WHERE workflow_id=?1 AND revision=?2",
                    &[&run.workflow_id, &run.revision],
                )?
            }
            None => {
                return Err("nonfixture requires an approved immutable workflow revision".into());
            }
        };
        let evidence: StepEvidence = read_row(
            &session.home,
            "SELECT record_json FROM workflow_steps WHERE run_id=?1 AND step_id=?2",
            &[&run_id, &step_id],
        )?;
        let step = revision
            .definition
            .steps
            .iter()
            .find(|s| s.step_id == step_id)
            .ok_or("stored workflow step missing")?
            .clone();
        let creator_matches = if matches!(
            run.trigger,
            duduclaw_gateway::workflow::schema::Trigger::Fixture { .. }
        ) {
            run.creator_grant.actor == revision.creator_grant.actor
                && run.creator_grant.policy_revision == revision.creator_grant.policy_revision
                && run
                    .creator_grant
                    .allowed_tools
                    .is_subset(&revision.creator_grant.allowed_tools)
        } else {
            run.creator_grant == revision.creator_grant
        };
        if run.actor != session.actor
            || revision.definition.hash() != revision.revision_hash
            || run.workflow_hash != revision.revision_hash
            || run.skill_hash != revision.definition.skill_revision_hash
            || !run.creator_grant.allowed_tools.contains(tool)
            || !revision.definition.required_capabilities.contains(tool)
            || !creator_matches
            || run.audience != revision.audience
            || !matches!(
                run.status,
                duduclaw_gateway::workflow::schema::RunStatus::Running
            )
            || evidence.input_hash != payload_hash(args)
        {
            return Err("stored workflow authority/input mismatch".into());
        }
        fresh(&run.deadline_at)?;
        fresh(&revision.expires_at)?;
        match &step.action {
            StepAction::McpRead { tool: stored } | StepAction::McpEffect { tool: stored, .. }
                if stored == tool =>
            {
                ()
            }
            _ => return Err("workflow step tool mismatch".into()),
        }
        step.input_schema.validate(args)?;
        if let InputRef::Literal { value } = &step.input {
            if value != args {
                return Err("workflow original arguments differ from immutable literal".into());
            }
        }
        if tool == "web_fetch_cached" {
            // Three-page pilot URLs are fixed DATA in the accepted immutable step.
            let InputRef::Literal { value } = &step.input else {
                return Err("workflow page URL must be immutable literal".into());
            };
            if args.get("url") != value.get("url")
                || args.get("url").and_then(Value::as_str).is_none()
            {
                return Err("workflow URL outside exact page allowlist".into());
            }
        }
        Ok(Self {
            run,
            revision,
            evidence,
            step,
        })
    }
    async fn authorize_audience(&self, home: &Path) -> Result<(), String> {
        let draft = duduclaw_gateway::workflow_draft_context::draft_for_workflow_readonly(
            home,
            &self.run.workflow_id,
            self.run.revision,
        )?
        .ok_or("workflow immutable source draft unavailable")?;
        if draft.owner != self.run.actor
            || draft.definition != self.revision.definition
            || draft.revision_hash != self.run.workflow_hash
            || draft.audience != self.run.audience
        {
            return Err("workflow source draft authority mismatch".into());
        }
        let decision = self
            .run
            .decision_context
            .as_ref()
            .ok_or("workflow trusted decision principal unavailable")?;
        let principal = duduclaw_gateway::review_evidence::audience::trusted_dashboard_principal(
            home,
            &decision.principal_id,
        )?;
        duduclaw_gateway::review_evidence::audience::authorize_workflow_audience(
            home,
            &principal,
            &draft.source_task,
            &draft.audience,
        )
        .await
    }
    fn authority_digest(&self) -> String {
        payload_hash(
            &json!({
                "revision_hash": self.revision.revision_hash,
                "creator_grant": self.run.creator_grant,
                "actor": self.run.actor,
                "audience": self.run.audience,
                "task": self.run.task,
                "step_id": self.step.step_id,
                "input_hash": self.evidence.input_hash,
                "grant": self.run.grant
            }),
        )
    }
}
/// Only the verified extension verifier constructs this exact per-effect proof.
/// A service ticket or active revision grant never counts as a human decision.
#[derive(Clone)]
struct HumanBoundProof {
    request_id: String,
    operation_id: String,
    next_fence: i64,
    binding_digest: String,
    payload_hash: String,
    actor: String,
    task_id: Option<String>,
    policy_revision: String,
    expires_at: String,
}
impl HumanBoundProof {
    fn validate(
        &self,
        operation: &OperationRecord,
        claim: &OperationClaim,
        current: &EffectiveToolCall,
    ) -> Result<(), String> {
        let duduclaw_gateway::approval::OperationAuthority::BoundHumanApproval { approval_id } =
            &operation.authority
        else {
            return Err("human proof authority source mismatch".into());
        };
        fresh(&self.expires_at)?;
        if &self.request_id != approval_id
            || self.operation_id != claim.operation_id
            || self.next_fence != claim.fence
            || self.binding_digest != digest(&operation.binding)?
            || self.payload_hash != current.payload_hash
            || self.actor != current.actor
            || self.task_id != operation.binding.task_id
            || self.policy_revision != current.policy_revision
        {
            return Err("human proof request/operation/fence/current payload mismatch".into());
        }
        Ok(())
    }
}

pub(crate) struct WorkflowCall {
    session: Arc<WorkflowSession>,
    metadata: WorkflowMetadata,
    stored: StoredCall,
    policy_before: String,
    environment_hash: String,
    original_arguments: Value,
    human_proof: Mutex<Option<HumanBoundProof>>,
    requirements: Mutex<Vec<String>>,
    prepared: Option<PrepareTicket>,
    operation: Option<OperationRecord>,
}
impl WorkflowCall {
    pub async fn parse(
        session: Option<&Arc<WorkflowSession>>,
        principal: &Principal,
        params: &Value,
    ) -> Result<Option<Self>, String> {
        let Some(meta) = params.get("_meta") else {
            return Ok(None);
        };
        let Some(obj) = meta.as_object() else {
            return Err("invalid tool call metadata".into());
        };
        let Some(raw) = obj.get(META_KEY) else {
            return Ok(None);
        };
        if obj.len() != 1 {
            return Err("unknown workflow metadata".into());
        }
        canonical_bytes(raw)?;
        let metadata: WorkflowMetadata = serde_json::from_value(raw.clone())
            .map_err(|_| "invalid typed workflow extension metadata")?;
        metadata.validate()?;
        let session = session
            .ok_or("workflow extension unavailable on this transport/session")?
            .clone();
        if principal.is_external
            || principal.client_id != duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID
            || crate::mcp_namespace::verified_employee_from_env(&session.home).as_deref()
                != Some(session.actor.as_str())
        {
            return Err("workflow employee identity rejected".into());
        }
        let tool = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or("missing workflow tool")?;
        let args = params
            .get("arguments")
            .ok_or("missing workflow arguments")?;
        if tool == "web_fetch_cached" {
            let url = args
                .get("url")
                .and_then(Value::as_str)
                .ok_or("workflow public URL missing")?;
            duduclaw_gateway::web_fetch::validate_workflow_public_url(url)
                .map_err(|_| "workflow public URL rejected")?;
        }
        let (run_id, step_key, input_hash, prepared, operation) = match &metadata {
            WorkflowMetadata::Prepare { context, .. } | WorkflowMetadata::Read { context, .. } => {
                if context.session_id != session.id {
                    return Err("workflow session mismatch".into());
                }
                session.verify("context", context, &context.mac)?;
                fresh(&context.expires_at)?;
                (
                    context.run_id.clone(),
                    context.step_key.clone(),
                    context.input_hash.clone(),
                    None,
                    None,
                )
            }
            WorkflowMetadata::Execute { ticket, .. } => {
                if ticket.version != VERSION || ticket.session_id != session.id {
                    return Err("workflow session mismatch".into());
                }
                session.verify("execute", ticket, &ticket.mac)?;
                fresh(&ticket.expires_at)?;
                let prepared = session
                    .prepared
                    .lock()
                    .map_err(|_| "workflow session poisoned")?
                    .get(&ticket.prepare_digest)
                    .cloned()
                    .ok_or("prepare ticket missing from this session")?;
                let broker = ApprovalBroker::open(&session.home)?;
                let operation = broker
                    .inspect_operation(&ticket.operation_id)
                    .await?
                    .ok_or("workflow operation missing")?;
                if digest(&operation.binding)? != ticket.binding_digest
                    || operation.binding.expires_at != ticket.expires_at
                {
                    return Err("execute ticket binding mismatch".into());
                }
                (
                    prepared.effective.run_id.clone(),
                    prepared.effective.step_key.clone(),
                    String::new(),
                    Some(prepared),
                    Some(operation),
                )
            }
        };
        if !input_hash.is_empty() && input_hash != payload_hash(args) {
            return Err("workflow original input hash mismatch".into());
        }
        let stored = StoredCall::read(&session, &run_id, &step_key, tool, args)?;
        stored.authorize_audience(&session.home).await?;
        match &metadata {
            WorkflowMetadata::Read { .. }
                if READ_TOOLS.contains(&tool)
                    && matches!(stored.step.action, StepAction::McpRead { .. }) =>
            {
                ()
            }
            WorkflowMetadata::Prepare { .. } | WorkflowMetadata::Execute { .. }
                if EFFECT_TOOLS.contains(&tool)
                    && matches!(stored.step.action, StepAction::McpEffect { .. }) =>
            {
                ()
            }
            _ => return Err("unsupported workflow receipt/read adapter".into()),
        }
        let policy_before = policy_revision(&session.home, &session.actor)?;
        if policy_before != stored.run.policy_revision {
            return Err("workflow current policy drift".into());
        }
        let environment_hash = session.environment_hash()?;
        if environment_hash != stored.run.environment_hash {
            return Err("workflow environment drift".into());
        }
        Ok(Some(Self {
            session,
            metadata,
            stored,
            policy_before,
            environment_hash,
            original_arguments: args.clone(),
            human_proof: Mutex::new(None),
            requirements: Mutex::new(Vec::new()),
            prepared,
            operation,
        }))
    }
    pub fn is_prepare(&self) -> bool {
        matches!(self.metadata, WorkflowMetadata::Prepare { .. })
    }
    pub fn is_read(&self) -> bool {
        matches!(self.metadata, WorkflowMetadata::Read { .. })
    }
    pub async fn require_human(&self, reason: &str, payload: &Value) -> Result<(), String> {
        self.requirements
            .lock()
            .map_err(|_| "workflow gate poisoned")?
            .push(reason.into());
        if self.is_prepare() {
            return Ok(());
        }
        if self.is_read() {
            return Err("workflow_read_requires_bound_human_approval".into());
        }
        let op = self
            .operation
            .as_ref()
            .ok_or("workflow operation unavailable")?;
        let duduclaw_gateway::approval::OperationAuthority::BoundHumanApproval { approval_id } =
            &op.authority
        else {
            return Err("active revision grant cannot satisfy Ask/static human approval".into());
        };
        let broker = ApprovalBroker::open(&self.session.home)?;
        let rec = broker
            .get(&approval_id.clone().into())
            .await?
            .ok_or("bound human request missing")?;
        if rec.request_kind != RequestKind::Approval
            || rec.status != ApprovalStatus::Approved
            || rec.agent_id != self.session.actor
            || rec.binding.as_ref() != Some(&op.binding)
            || rec.payload != *payload
            || payload_hash(payload) != op.binding.payload_hash
        {
            return Err("human bound proof mismatch".into());
        }
        op.binding.validate(payload)?;
        let proof = HumanBoundProof {
            request_id: approval_id.clone(),
            operation_id: op.operation_id.clone(),
            next_fence: op.fence.checked_add(1).ok_or("invalid operation fence")?,
            binding_digest: digest(&op.binding)?,
            payload_hash: payload_hash(payload),
            actor: self.session.actor.clone(),
            task_id: op.binding.task_id.clone(),
            policy_revision: self.policy_before.clone(),
            expires_at: op.binding.expires_at.clone(),
        };
        *self
            .human_proof
            .lock()
            .map_err(|_| "workflow proof poisoned")? = Some(proof);
        Ok(())
    }
    fn effective(&self, payload: &Value) -> Result<EffectiveToolCall, String> {
        if policy_revision(&self.session.home, &self.session.actor)? != self.policy_before {
            return Err("policy changed during workflow gate evaluation".into());
        }
        let current_stored = StoredCall::read(
            &self.session,
            &self.stored.run.run_id,
            &self.stored.step.step_id,
            payload["name"].as_str().ok_or("missing tool")?,
            &self.original_arguments,
        )?;
        if current_stored.authority_digest() != self.stored.authority_digest()
            || current_stored.run.decision_context != self.stored.run.decision_context
            || current_stored.run.deadline_at != self.stored.run.deadline_at
        {
            return Err("workflow authority changed during gate evaluation".into());
        }
        if policy_revision(&self.session.home, &self.stored.run.creator_grant.actor)?
            != self.stored.run.creator_grant.policy_revision
        {
            return Err("workflow creator authority revoked/changed".into());
        }
        self.stored
            .step
            .input_schema
            .validate(&payload["arguments"])?;
        if matches!(
            self.stored.run.trigger,
            duduclaw_gateway::workflow::Trigger::Fixture { .. }
        ) && matches!(self.stored.step.action, StepAction::McpEffect { .. })
        {
            duduclaw_gateway::workflow::staging::check_effect_scope(
                &self.session.home,
                payload["name"].as_str().ok_or("missing tool")?,
                &payload["arguments"],
            )?;
        }
        let mut requirements = self
            .requirements
            .lock()
            .map_err(|_| "workflow gate poisoned")?
            .clone();
        requirements.sort();
        requirements.dedup();
        Ok(EffectiveToolCall {
            version: VERSION,
            actor: self.session.actor.clone(),
            session_id: self.session.id.clone(),
            run_id: self.stored.run.run_id.clone(),
            step_key: self.stored.step.step_id.clone(),
            tool: payload["name"].as_str().ok_or("missing tool")?.into(),
            effective_arguments: payload["arguments"].clone(),
            payload_hash: payload_hash(payload),
            policy_revision: self.policy_before.clone(),
            authority_digest: self.stored.authority_digest(),
            environment_hash: self.environment_hash.clone(),
            approval_requirements: requirements,
            expires_at: self.stored.run.deadline_at.clone(),
        })
    }
    pub fn finish_prepare(&self, payload: &Value) -> Result<Value, String> {
        let effective = self.effective(payload)?;
        let mut ticket = PrepareTicket {
            effective,
            nonce: uuid::Uuid::new_v4().to_string(),
            mac: String::new(),
        };
        ticket.mac = sign(&self.session.secret, "prepare", &unsigned(&ticket)?)?;
        let key = digest(&ticket)?;
        let mut cache = self
            .session
            .prepared
            .lock()
            .map_err(|_| "workflow session poisoned")?;
        cache.retain(|_, p| fresh(&p.effective.expires_at).is_ok());
        if cache.len() >= 256 {
            return Err("workflow prepare session capacity reached".into());
        }
        cache.insert(key, ticket.clone());
        serde_json::to_value(ticket).map_err(|e| e.to_string())
    }
    pub async fn before_effect(
        &self,
        payload: &Value,
    ) -> Result<(ApprovalBroker, OperationClaim), String> {
        let current = self.effective(payload)?;
        // Refresh human visibility authority at the final effect boundary. Worker
        // capabilities remain the native dispatcher/creator-grant authority.
        self.stored.authorize_audience(&self.session.home).await?;
        let prepared = self.prepared.as_ref().ok_or("missing prepare")?;
        if current != prepared.effective {
            return Err("workflow effective gate/payload drift".into());
        }
        let op = self.operation.as_ref().ok_or("missing operation")?;
        let b = &op.binding;
        if b.run_id != current.run_id
            || op.step_key != current.step_key
            || b.actor_principal != current.actor
            || b.payload_hash != current.payload_hash
            || b.policy_revision != current.policy_revision
            || b.environment_hash != current.environment_hash
            || b.run_origin_kind != "workflow"
            || b.resume_handler != "workflow_v1"
            || self.stored.run.task.as_ref().map(|t| &t.task_id) != b.task_id.as_ref()
            || self.stored.run.task.as_ref().map(|t| t.revision) != b.task_revision
            || self.stored.run.task.as_ref().map(|t| &t.snapshot_hash)
                != b.task_snapshot_hash.as_ref()
            || self.stored.run.decision_context.as_ref() != Some(&b.decision_context)
        {
            return Err("workflow binding/current service authority mismatch".into());
        }
        let cwd = std::fs::canonicalize(self.session.home.join("agents").join(&self.session.actor))
            .map_err(|e| e.to_string())?;
        if b.cwd.as_deref() != Some(cwd.to_string_lossy().as_ref())
            || chrono::DateTime::parse_from_rfc3339(&b.expires_at)
                .map_err(|_| "invalid binding expiry")?
                > chrono::DateTime::parse_from_rfc3339(&self.stored.run.deadline_at)
                    .map_err(|_| "invalid run deadline")?
        {
            return Err("workflow cwd/deadline binding mismatch".into());
        }
        if let duduclaw_gateway::approval::OperationAuthority::ActiveWorkflowRevisionGrant {
            grant_id,
            epoch,
            spec_hash,
        } = &op.authority
        {
            let grant = self
                .stored
                .run
                .grant
                .as_ref()
                .ok_or("workflow run missing active revision grant")?;
            if &grant.grant_id != grant_id || grant.epoch != *epoch || &grant.spec_hash != spec_hash
            {
                return Err("workflow operation/run grant mismatch".into());
            }
        }
        b.validate(payload)?;
        let broker = ApprovalBroker::open(&self.session.home)?;
        #[cfg(feature = "workflow-test-checkpoints")]
        host_test_checkpoint(&self.session.home, &op.operation_id, "before_claim").await?;
        let claim = broker
            .claim_operation(
                &op.operation_id,
                b,
                &format!("stdio:{}", self.session.id),
                300,
            )
            .await?;
        if !current.approval_requirements.is_empty() {
            let proof = self
                .human_proof
                .lock()
                .map_err(|_| "workflow proof poisoned")?
                .clone()
                .ok_or("workflow human proof missing")?;
            proof.validate(op, &claim, &current)?;
        }
        // This is the final authority CAS immediately before the original handler.
        #[cfg(feature = "workflow-test-checkpoints")]
        host_test_checkpoint(&self.session.home, &op.operation_id, "before_begin").await?;
        broker.begin_execution(&claim, b).await?;
        #[cfg(feature = "workflow-test-checkpoints")]
        host_test_checkpoint(&self.session.home, &op.operation_id, "after_begin").await?;
        Ok((broker, claim))
    }
    pub async fn settle(
        &self,
        broker: &ApprovalBroker,
        claim: &OperationClaim,
        observation: &HandlerObservation,
        result: &Value,
    ) -> Result<Value, String> {
        let op = self.operation.as_ref().ok_or("missing operation")?;
        let binding_digest = digest(&op.binding)?;
        let (state, receipt, error) = match observation {
            HandlerObservation::RejectedBeforeEffect => (
                OperationState::Failed,
                None,
                Some("handler_rejected_before_effect"),
            ),
            HandlerObservation::Unknown => (
                OperationState::Uncertain,
                None,
                Some("handler_effect_unconfirmed"),
            ),
            HandlerObservation::Committed(evidence) => {
                let receipt = json!({
                    "version": 1,
                    "operation_id": op.operation_id,
                    "run_id": op.run_id,
                    "step_key": op.step_key,
                    "tool": self.prepared.as_ref().ok_or("missing prepare")?.effective.tool,
                    "actor": self.session.actor,
                    "payload_hash": op.binding.payload_hash,
                    "policy_revision": op.binding.policy_revision,
                    "binding_digest": binding_digest,
                    "fence": claim.fence,
                    "authority_source": op.authority,
                    "outcome": "committed",
                    "evidence": evidence,
                    "result_hash": payload_hash(result),
                    "observed_at": Utc::now().to_rfc3339()
                });
                (OperationState::Succeeded, Some(receipt), None)
            }
        };
        broker
            .settle_operation(claim, state, receipt.clone(), error)
            .await?;
        let reply = OperationReply {
            version: VERSION,
            operation_id: op.operation_id.clone(),
            binding_digest,
            authority_source: serde_json::from_value(
                serde_json::to_value(&op.authority).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?,
            state: state.as_str().into(),
            receipt_digest: receipt.as_ref().map(payload_hash),
            result: Some(result.clone()),
            error_code: error.map(str::to_owned),
        };
        serde_json::to_value(reply).map_err(|e| e.to_string())
    }
    pub fn read_reply(
        &self,
        payload: &Value,
        result: &Value,
        observation: &ReadObservation,
    ) -> Result<Value, String> {
        let current = self.effective(payload)?;
        let ReadObservation::Observed { output, evidence } = observation else {
            return Err("workflow read lacks typed handler observation".into());
        };
        self.stored.step.output_schema.validate(output)?;
        if result.get("isError").and_then(Value::as_bool) == Some(true)
            || result.pointer("/result/isError").and_then(Value::as_bool) == Some(true)
        {
            return Err("workflow read handler rejected".into());
        }
        serde_json::to_value(ReadReply {
            version: VERSION,
            run_id: current.run_id,
            step_key: current.step_key,
            tool: current.tool,
            actor: current.actor,
            payload_hash: current.payload_hash,
            policy_revision: current.policy_revision,
            authority_digest: current.authority_digest,
            environment_hash: current.environment_hash,
            observed_at: Utc::now().to_rfc3339(),
            result: output.clone(),
            result_hash: payload_hash(output),
            evidence: evidence.clone(),
            error_code: None,
        })
        .map_err(|e| e.to_string())
    }
}
mod observation;
#[cfg(feature = "workflow-test-checkpoints")]
use observation::host_test_checkpoint;
pub(crate) use observation::{
    EFFECT_OBSERVATION, HandlerObservation, READ_OBSERVATION, ReadObservation, effect_committed,
    effect_start, is_workflow_read, read_observed,
};

#[cfg(test)]
#[path = "workflow_operation_tests.rs"]
mod tests;
