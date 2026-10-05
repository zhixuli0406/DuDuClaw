//! Typed proposals are DATA; all owner/grant/skill/snapshot fields are host derived.
use super::*;
use crate::review_evidence::IntegrityStatus;
use crate::review_evidence::audience::{
    TaskAudience, dashboard_may_read, dashboard_may_read_task, task_audience,
};
use crate::workflow::{
    AssertionOutcome, CostBudget, FixtureExecutionRequest, RunStatus, WorkflowDefinition,
};
use crate::workflow_drafts::{DraftFixture, SourceData, WorkflowDraft};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DraftProposal {
    skill_id: String,
    definition: WorkflowDefinition,
    fixtures: Vec<ProposalFixture>,
    source_data: Vec<SourceData>,
    effect_templates: std::collections::BTreeMap<String, crate::approval::EffectTemplate>,
    budget: CostBudget,
    input_max_age_seconds: u32,
    timezone: String,
    /// Accepted for older clients and ignored (V-M-6: no runner reads it).
    #[serde(default)]
    #[allow(dead_code)]
    stop_conditions: Vec<String>,
    routine: Option<ProposalRoutine>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposalRoutine {
    expression: String,
    timezone: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposalFixture {
    fixture_id: String,
    kind: crate::workflow::FixtureKind,
    input: Value,
    assertions: Vec<crate::workflow::FixtureAssertion>,
    #[serde(default)]
    decisions: std::collections::BTreeMap<String, crate::workflow_drafts::FixtureDecision>,
}
impl MethodHandler {
    /// Read access (Viewer) to a draft; see [`Self::accessible_draft_at`].
    pub(super) async fn accessible_draft(
        &self,
        params: &Value,
        ctx: &UserContext,
    ) -> Result<WorkflowDraft, WsFrame> {
        self.accessible_draft_at(params, ctx, AccessLevel::Viewer)
            .await
    }

    /// A draft the caller may act on at `level`: that binding on the source
    /// task's employee and on the draft's owner, plus the saved and current
    /// audience. Reading is Viewer; capturing, accepting, test runs, review
    /// requests, applying, revoking and run controls are Operator (V-M-3).
    /// A missing draft answers exactly like a refused one (L-1).
    pub(super) async fn accessible_draft_at(
        &self,
        params: &Value,
        ctx: &UserContext,
        level: AccessLevel,
    ) -> Result<WorkflowDraft, WsFrame> {
        let denied = || WsFrame::error_response("", PERMISSION_DENIED);
        let id = params.get("draft_id").and_then(Value::as_str).unwrap_or("");
        let revision = params.get("revision").and_then(Value::as_i64).unwrap_or(0);
        let store = self.workflow_store().await.map_err(|_| denied())?;
        let draft = store
            .draft(id, revision)
            .await
            .map_err(|_| denied())?
            .ok_or_else(denied)?;
        let tasks = self.task_store().await?;
        match tasks.get_task(&draft.source_task).await {
            Ok(Some(_)) => {
                self.authorize_task_access(&tasks, ctx, &draft.source_task, level)
                    .await
                    .map_err(|_| denied())?;
            }
            // F5-D (P-M7): an activated workflow outlives its source task;
            // once that task is removed, admins keep oversight (view, cancel,
            // decide step cards). Everyone else is refused.
            Ok(None) if ctx.is_admin() => {}
            _ => return Err(denied()),
        }
        if !ctx.has_agent_access(&draft.owner, level) {
            return Err(denied());
        }
        crate::review_evidence::audience::authorize_workflow_audience(
            &self.home_dir,
            ctx,
            &draft.source_task,
            &draft.audience,
        )
        .await
        .map_err(|_| WsFrame::error_response("", "permission denied"))?;
        Ok(draft)
    }
    async fn draft_issues(&self, draft: &WorkflowDraft) -> Result<Vec<String>, String> {
        let mut issues = Vec::new();
        let store = self.workflow_store().await?;
        let snapshot = store
            .review_snapshot(&draft.source_snapshot_id)
            .await?
            .ok_or("source review snapshot unavailable")?;
        let tasks = self
            .task_store()
            .await
            .map_err(|_| "task store unavailable")?;
        let current = tasks
            .authority_snapshot(&draft.source_task)
            .await?
            .ok_or("source task unavailable")?;
        if snapshot.snapshot_hash != draft.source_evidence_hash
            || current.revision != snapshot.authority_revision
            || current.hash != snapshot.authority_snapshot_hash
            || snapshot.artifacts.is_empty()
            || snapshot
                .gaps
                .iter()
                .any(|g| g == super::workflow_review_rpc::TRUNCATED_GAP)
            || snapshot
                .current_artifacts(&self.home_dir, &current)
                .iter()
                .any(|a| a.integrity != IntegrityStatus::Current)
        {
            issues.push("source_snapshot_stale_or_unverified".into());
        }
        let (_, skill_hash) = crate::workflow_draft_context::installed_skill_revision(
            &self.home_dir,
            &draft.owner,
            &draft.skill_id,
        )?;
        if skill_hash != draft.definition.skill_revision_hash {
            issues.push("skill_revision_changed".into());
        }
        let current_grant =
            crate::workflow_draft_context::creator_grant(&self.home_dir, &draft.owner)?;
        if current_grant.policy_revision != draft.creator_grant.policy_revision
            || !draft
                .definition
                .required_capabilities
                .is_subset(&current_grant.allowed_tools)
        {
            issues.push("policy_or_capability_changed".into());
        }
        Ok(issues)
    }
    async fn draft_view(&self, draft: WorkflowDraft) -> Result<Value, String> {
        let store = self.workflow_store().await?;
        let rows = store
            .fixture_results(&draft.draft_id, draft.revision)
            .await?;
        let mut issues = self.draft_issues(&draft).await?;
        let now = Utc::now();
        let complete = draft.fixtures.iter().all(|f| {
            rows.iter()
                .find(|(e, _)| e.fixture_id == f.fixture_id)
                .is_some_and(|(e, a)| {
                    a.len() == f.assertions.len()
                        && !a.is_empty()
                        && a.iter().all(|a| a.outcome == AssertionOutcome::Matched)
                        && DateTime::parse_from_rfc3339(&e.expires_at)
                            .is_ok_and(|t| t.with_timezone(&Utc) > now)
                        && !matches!(
                            e.status,
                            RunStatus::Uncertain
                                | RunStatus::Running
                                | RunStatus::Pending
                                | RunStatus::WaitingApproval
                        )
                })
        });
        if !complete {
            issues.push("fixture_evidence_missing_stale_or_mismatched".into());
        }
        let fixtures: Vec<Value> = rows
            .into_iter()
            .map(|(e, a)| json!({"evidence":e,"assertions":a}))
            .collect();
        let service = self.workflow_service().await?;
        let activation = service
            .activation_for_draft(&draft.draft_id, draft.revision)
            .await?;
        let projection=activation.as_ref().map(|a|json!({
            "activation_id": a.request.activation_id,
            "acceptance_id": a.acceptance_id,
            "state": a.state,
            "material_hash": a.material_hash,
            "error_code": a.error_code,
            "suspension": a.suspension,
            // U8: the activation stops on its own at this instant.
            "expires_at": a.request.spec.expires_at,
            // What the Admin accepted: each effect and the one record it changes.
            "effect_targets": crate::workflow::effect_targets::effect_targets(a.request.spec.templates.values())
        }));
        Ok(
            json!({
                "draft": draft,
                "fixtures": fixtures,
                "activation": projection,
                "activation_eligible": issues.is_empty()&&complete&&activation.is_none(),
                // R-M4: whether the money budgets can bind at all.
                "pricing": crate::workflow::cost_ledger::load_pricing(&self.home_dir).view(),
                "issues": issues
            }),
        )
    }
    pub(crate) async fn handle_workflow_drafts_create(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let fresh =
            match crate::review_evidence::audience::fresh_dashboard_context(&self.home_dir, ctx) {
                Ok(c) => c,
                Err(_) => return WsFrame::error_response("", "permission denied"),
            };
        let ctx = &fresh;
        if !ctx.has_role(UserRole::Manager) {
            return WsFrame::error_response("", "manager role required");
        }
        let task_id = params.get("task_id").and_then(Value::as_str).unwrap_or("");
        let snapshot_id = params
            .get("snapshot_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let expected_hash = params
            .get("snapshot_hash")
            .and_then(Value::as_str)
            .unwrap_or("");
        let tasks = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let task = match self
            .authorize_task_access(&tasks, ctx, task_id, AccessLevel::Operator)
            .await
        {
            Ok(t) => t,
            Err(f) => return f,
        };
        if task.status != "done" {
            return WsFrame::error_response("", "a completed source task is required");
        }
        let proposal = match params
            .get("proposal")
            .cloned()
            .ok_or("typed proposal required")
            .and_then(|v| {
                serde_json::from_value::<DraftProposal>(v).map_err(|_| "invalid typed proposal")
            }) {
            Ok(p) => p,
            Err(e) => return WsFrame::error_response("", e),
        };
        let store = match self.workflow_store().await {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let snapshot = match store.review_snapshot(snapshot_id).await {
            Ok(Some(s)) => s,
            _ => return WsFrame::error_response("", "source review snapshot unavailable"),
        };
        let task_aud = task_audience(&self.home_dir, task_id);
        if snapshot.task_id != task_id
            || task_aud == TaskAudience::Unreadable
            || !dashboard_may_read(ctx, &snapshot.audience)
            || !dashboard_may_read_task(ctx, &task_aud)
        {
            return WsFrame::error_response("", "permission denied");
        }
        if snapshot.snapshot_hash != expected_hash
            || snapshot.authority_revision != task.authority_revision
            || snapshot.authority_snapshot_hash != task.authority_snapshot_hash()
        {
            return WsFrame::error_response("", "source snapshot changed");
        }
        let owner = task
            .claimed_by
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&task.assigned_to);
        let creator = match crate::workflow_draft_context::creator_grant(&self.home_dir, owner) {
            Ok(g) => g,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let (_, skill_hash) = match crate::workflow_draft_context::installed_skill_revision(
            &self.home_dir,
            owner,
            &proposal.skill_id,
        ) {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let mut definition = proposal.definition;
        definition.skill_revision_hash = skill_hash;
        definition.revision = 1;
        definition.workflow_id = uuid::Uuid::new_v4().to_string();
        if let Err(e) = definition.validate(&creator.allowed_tools, &creator.allowed_tools) {
            return WsFrame::error_response("", &e);
        }
        let mut draft = WorkflowDraft {
            schema_version: 1,
            draft_id: uuid::Uuid::new_v4().to_string(),
            revision: 1,
            owner: owner.into(),
            source_task: task_id.into(),
            skill_id: proposal.skill_id,
            source_snapshot_id: snapshot.snapshot_id.clone(),
            source_evidence_hash: snapshot.snapshot_hash.clone(),
            revision_hash: definition.hash(),
            definition,
            source_data: proposal.source_data,
            fixtures: proposal
                .fixtures
                .into_iter()
                .map(|f| DraftFixture {
                    fixture_id: f.fixture_id,
                    kind: f.kind,
                    input_hash: crate::approval::payload_hash(&f.input),
                    assertion_hash: crate::approval::payload_hash(
                        &serde_json::to_value(&f.assertions).expect("assertions serializable"),
                    ),
                    input: f.input,
                    assertions: f.assertions,
                    decisions: f.decisions,
                })
                .collect(),
            creator_grant: creator,
            effect_templates: proposal.effect_templates,
            audience: snapshot.audience.clone(),
            budget: proposal.budget,
            input_max_age_seconds: proposal.input_max_age_seconds,
            timezone: proposal.timezone,
            routine: proposal.routine.map(|r| crate::workflow::RoutineSchedule {
                cron_id: uuid::Uuid::new_v4().to_string(),
                expression: r.expression,
                timezone: r.timezone,
            }),
            stop_conditions: Vec::new(),
            created_at: Utc::now().to_rfc3339(),
            disabled: true,
            review_status: "draft".into(),
            draft_hash: String::new(),
        };
        draft.draft_hash = draft.compute_hash();
        if let Err(e) = draft.validate(&snapshot) {
            return WsFrame::error_response("", &e);
        }
        match self.draft_issues(&draft).await {
            Ok(issues) if issues.is_empty() => (),
            Ok(_) => return WsFrame::error_response("", "source material stale or unverified"),
            Err(e) => return WsFrame::error_response("", &e),
        }
        if let Err(e) = store.save_draft(&draft).await {
            return WsFrame::error_response("", &e);
        }
        match self.draft_view(draft).await {
            Ok(v) => WsFrame::ok_response("", v),
            Err(e) => WsFrame::error_response("", &e),
        }
    }
    pub(crate) async fn handle_workflow_drafts_get(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let fresh =
            match crate::review_evidence::audience::fresh_dashboard_context(&self.home_dir, ctx) {
                Ok(c) => c,
                Err(_) => return WsFrame::error_response("", "permission denied"),
            };
        let ctx = &fresh;
        let draft = match self.accessible_draft(&params, ctx).await {
            Ok(d) => d,
            Err(f) => return f,
        };
        match self.draft_view(draft).await {
            Ok(v) => WsFrame::ok_response("", v),
            Err(e) => WsFrame::error_response("", &e),
        }
    }
    pub(crate) async fn handle_workflow_drafts_list(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let fresh =
            match crate::review_evidence::audience::fresh_dashboard_context(&self.home_dir, ctx) {
                Ok(c) => c,
                Err(_) => return WsFrame::error_response("", "permission denied"),
            };
        let ctx = &fresh;
        let task_id = params.get("task_id").and_then(Value::as_str).unwrap_or("");
        let tasks = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let task = match self
            .authorize_task_access(&tasks, ctx, task_id, AccessLevel::Viewer)
            .await
        {
            Ok(t) => t,
            Err(f) => return f,
        };
        if !dashboard_may_read_task(ctx, &task_audience(&self.home_dir, task_id)) {
            return WsFrame::error_response("", "permission denied");
        }
        let store = match self.workflow_store().await {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let owner = task
            .claimed_by
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&task.assigned_to);
        let (drafts, next_cursor) = match store
            .list_drafts_page(
                owner,
                Some(task_id),
                params.get("cursor").and_then(Value::as_i64),
            )
            .await
        {
            Ok(d) => d,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let mut views = Vec::new();
        for d in drafts {
            if !dashboard_may_read(ctx, &d.audience) {
                continue;
            }
            match self.draft_view(d).await {
                Ok(v) => views.push(v),
                Err(e) => return WsFrame::error_response("", &e),
            }
        }
        WsFrame::ok_response("", json!({"drafts":views,"next_cursor":next_cursor}))
    }
    pub(crate) async fn handle_workflow_drafts_run_fixture(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let fresh =
            match crate::review_evidence::audience::fresh_dashboard_context(&self.home_dir, ctx) {
                Ok(c) => c,
                Err(_) => return WsFrame::error_response("", "permission denied"),
            };
        let ctx = &fresh;
        if !ctx.has_role(UserRole::Manager) {
            return WsFrame::error_response("", "manager role required");
        }
        let draft = match self.accessible_draft_at(&params, ctx, AccessLevel::Operator).await {
            Ok(d) => d,
            Err(f) => return f,
        };
        if params.get("draft_hash").and_then(Value::as_str) != Some(draft.draft_hash.as_str()) {
            return WsFrame::error_response("", "fixed draft hash mismatch");
        }
        match self.draft_issues(&draft).await {
            Ok(i) if i.is_empty() => (),
            _ => return WsFrame::error_response("", "draft material stale or unverified"),
        }
        let id = params
            .get("fixture_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let fixture = match draft.fixtures.iter().find(|f| f.fixture_id == id) {
            Some(f) => f,
            None => return WsFrame::error_response("", "fixture not found"),
        };
        let request = FixtureExecutionRequest {
            fixture_id: fixture.fixture_id.clone(),
            draft_id: draft.draft_id.clone(),
            revision: draft.revision,
            kind: fixture.kind,
            definition: draft.definition.clone(),
            input: fixture.input.clone(),
            input_hash: fixture.input_hash.clone(),
            assertion_hash: fixture.assertion_hash.clone(),
            actor: draft.owner.clone(),
            creator_grant: draft.creator_grant.clone(),
            audience: draft.audience.clone(),
            task: None,
            deadline_at: (Utc::now() + ChronoDuration::minutes(15)).to_rfc3339(),
            budget: draft.budget.clone(),
            isolated_home: String::new(),
        };
        let service = match self.workflow_service().await {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let context = crate::approval::DecisionContext {
            channel: "dashboard".into(),
            account_id: "dashboard".into(),
            conversation_id: format!("dashboard:{}", ctx.user_id),
            principal_id: ctx.user_id.clone(),
        };
        let e = match crate::approval::CURRENT_DECISION_CONTEXT
            .scope(Some(context), service.run_fixture(request))
            .await
        {
            Ok(e) => e,
            Err(e) => return WsFrame::error_response("", &e),
        };
        // Re-read immutable service evidence rather than trusting an executor reply.
        let e = match service.fixture_evidence(&e.run_id).await {
            Ok(e) => e,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let store = match self.workflow_store().await {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", &e),
        };
        match store.save_fixture_evidence(&e).await {
            Ok(a) => {
                WsFrame::ok_response("", json!({"run_id":e.run_id,"evidence":e,"assertions":a}))
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    pub(crate) async fn handle_workflow_drafts_request_activation(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let fresh =
            match crate::review_evidence::audience::fresh_dashboard_context(&self.home_dir, ctx) {
                Ok(c) => c,
                Err(_) => return WsFrame::error_response("", "permission denied"),
            };
        let ctx = &fresh;
        if !ctx.has_role(UserRole::Manager) {
            return WsFrame::error_response("", "manager role required");
        }
        let draft = match self.accessible_draft_at(&params, ctx, AccessLevel::Operator).await {
            Ok(d) => d,
            Err(f) => return f,
        };
        if params.get("draft_hash").and_then(Value::as_str) != Some(draft.draft_hash.as_str()) {
            return WsFrame::error_response("", "fixed draft hash mismatch");
        }
        let result: Result<Value, String> = async {
            if !self.draft_issues(&draft).await?.is_empty() {
                return Err("draft material stale or unverified".into());
            }
            let service = self.workflow_service().await?;
            if service
                .activation_for_draft(&draft.draft_id, draft.revision)
                .await?
                .is_some()
            {
                return Err("activation already exists".into());
            }
            let store = self.workflow_store().await?;
            let rows = store
                .fixture_results(&draft.draft_id, draft.revision)
                .await?;
            let mut results = Vec::new();
            let mut assertions = Vec::new();
            let mut expiry = Utc::now() + ChronoDuration::days(1);
            for fixture in &draft.fixtures {
                let (e, a) = rows
                    .iter()
                    .find(|(e, _)| e.fixture_id == fixture.fixture_id)
                    .ok_or("fixture evidence missing")?;
                let fixed = service.fixture_evidence(&e.run_id).await?;
                if fixed != *e
                    || a.is_empty()
                    || a.len() != fixture.assertions.len()
                    || a.iter().any(|v| v.outcome != AssertionOutcome::Matched)
                {
                    return Err("fixture evidence mismatched".into());
                }
                let until = DateTime::parse_from_rfc3339(&e.expires_at)
                    .map_err(|_| "fixture expiry invalid")?
                    .with_timezone(&Utc);
                if until <= Utc::now() {
                    return Err("fixture evidence expired".into());
                }
                expiry = expiry.min(until);
                results.push(e.clone());
                assertions.extend(a.clone());
            }
            let digest =
                crate::approval::payload_hash(&json!({"results":results,"assertions":assertions}));
            let activation_id = uuid::Uuid::new_v4().to_string();
            // U8: the card must be decided while the fixture evidence is
            // fresh (its binding expires with it); the activation itself
            // lives for `[workflow] activation_days`.
            let expires_at = expiry.to_rfc3339();
            let fixtures_expires_at = expiry.to_rfc3339();
            let activation_expires_at = (Utc::now()
                + ChronoDuration::days(crate::workflow::activation::activation_days(&self.home_dir)?))
            .to_rfc3339();
            let context = crate::approval::DecisionContext {
                channel: "dashboard".into(),
                account_id: "dashboard".into(),
                conversation_id: format!("dashboard:{}", ctx.user_id),
                principal_id: ctx.user_id.clone(),
            };
            let spec = crate::approval::GrantSpec {
                schema_version: 1,
                workflow_id: draft.definition.workflow_id.clone(),
                workflow_revision: draft.revision,
                revision_hash: draft.revision_hash.clone(),
                skill_hash: draft.definition.skill_revision_hash.clone(),
                fixtures_digest: digest.clone(),
                activation_id: activation_id.clone(),
                actor: draft.owner.clone(),
                operator_context: context.clone(),
                creator_grant: draft.creator_grant.clone(),
                audience: draft.audience.clone(),
                templates: draft.effect_templates.clone(),
                input_max_age_seconds: draft.input_max_age_seconds,
                fixtures_expires_at,
                budget: draft.budget.clone(),
                expires_at: activation_expires_at.clone(),
                policy_revision: draft.creator_grant.policy_revision.clone(),
            };
            let revision = crate::workflow::ApprovedWorkflowRevision {
                definition: draft.definition.clone(),
                revision_hash: draft.revision_hash.clone(),
                owner: draft.owner.clone(),
                creator_grant: draft.creator_grant.clone(),
                audience: draft.audience.clone(),
                fixtures_digest: digest,
                acceptance_id: String::new(),
                accepted_at: String::new(),
                expires_at: activation_expires_at,
            };
            let request = crate::workflow::ActivationRequest {
                activation_id: activation_id.clone(),
                draft_id: draft.draft_id.clone(),
                draft_hash: draft.draft_hash.clone(),
                revision,
                spec,
                fixture_results: results,
                fixture_assertions: assertions,
                cron: draft.routine.clone(),
            };
            let (skill_path, skill_hash) = crate::workflow_draft_context::installed_skill_revision(
                &self.home_dir,
                &draft.owner,
                &draft.skill_id,
            )?;
            let environment = service.runner.executor.environment(&draft.owner)?;
            let binding = crate::approval::ExecutionBinding {
                schema_version: 1,
                run_id: activation_id,
                run_origin_kind: "workflow".into(),
                actor_principal: draft.owner.clone(),
                decision_context: context,
                task_id: None,
                task_revision: None,
                task_snapshot_hash: None,
                payload_hash: crate::approval::payload_hash(
                    &crate::workflow::activation::activation_payload(&request),
                ),
                policy_revision: draft.creator_grant.policy_revision.clone(),
                cwd: Some(
                    self.home_dir
                        .join("agents")
                        .join(&draft.owner)
                        .to_string_lossy()
                        .into_owned(),
                ),
                environment_hash: environment.hash(),
                file_hashes: std::collections::BTreeMap::from([(
                    skill_path.to_string_lossy().into_owned(),
                    skill_hash,
                )]),
                expires_at,
                resume_handler: "workflow_v1".into(),
                resume_version: 1,
            };
            let id = crate::skill_approval::request_workflow_revision_activation(
                &service, request, binding,
            )
            .await?;
            Ok(json!({"approval_id":id}))
        }
        .await;
        match result {
            Ok(v) => WsFrame::ok_response("", v),
            Err(e) => WsFrame::error_response("", &e),
        }
    }
    pub(crate) async fn handle_workflow_drafts_commit_activation(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        self.workflow_activation_action(params, ctx, false).await
    }
    pub(crate) async fn handle_workflow_drafts_revoke_activation(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        self.workflow_activation_action(params, ctx, true).await
    }
    async fn workflow_activation_action(
        &self,
        params: Value,
        ctx: &UserContext,
        revoke: bool,
    ) -> WsFrame {
        let fresh =
            match crate::review_evidence::audience::fresh_dashboard_context(&self.home_dir, ctx) {
                Ok(c) => c,
                Err(_) => return WsFrame::error_response("", "permission denied"),
            };
        let ctx = &fresh;
        if !ctx.has_role(UserRole::Manager) {
            return WsFrame::error_response("", "manager role required");
        }
        let draft = match self.accessible_draft_at(&params, ctx, AccessLevel::Operator).await {
            Ok(d) => d,
            Err(f) => return f,
        };
        if params.get("draft_hash").and_then(Value::as_str) != Some(draft.draft_hash.as_str()) {
            return WsFrame::error_response("", "fixed draft hash mismatch");
        }
        let result: Result<Value, String> = async {
            let service = self.workflow_service().await?;
            let record = service
                .activation_for_draft(&draft.draft_id, draft.revision)
                .await?
                .ok_or("activation not found")?;
            if !revoke && !self.draft_issues(&draft).await?.is_empty() {
                return Err("draft material stale or unverified".into());
            }
            let record = if revoke {
                service
                    .revoke_activation(
                        &record.request.activation_id,
                        "operator revoked fixed workflow revision",
                    )
                    .await?
            } else {
                service
                    .commit_activation(&record.request.activation_id)
                    .await?
            };
            serde_json::to_value(record).map_err(|e| e.to_string())
        }
        .await;
        match result {
            Ok(v) => WsFrame::ok_response("", v),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// Protect approval inbox metadata and decisions with the same immutable
    /// draft, source-task ACL and audience restrictions as the review route.
    pub(crate) async fn authorize_workflow_activation_record(
        &self,
        payload: &Value,
        ctx: &UserContext,
    ) -> Result<(), String> {
        if payload.get("kind").and_then(Value::as_str) != Some("workflow_activation") {
            return Ok(());
        }
        let fresh = crate::review_evidence::audience::fresh_dashboard_context(&self.home_dir, ctx)?;
        let request: crate::workflow::ActivationRequest =
            serde_json::from_value(payload.get("request").cloned().ok_or("permission denied")?)
                .map_err(|_| "permission denied")?;
        if crate::workflow::activation::activation_payload(&request) != *payload {
            return Err("permission denied".into());
        }
        let draft=self.accessible_draft(&json!({
            "draft_id": request.draft_id,
            "revision": request.revision.definition.revision
        }),&fresh).await.map_err(|_|"permission denied")?;
        if draft.draft_hash != request.draft_hash {
            return Err("permission denied".into());
        }
        Ok(())
    }
}
