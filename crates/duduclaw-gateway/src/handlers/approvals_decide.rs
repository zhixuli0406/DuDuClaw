//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    pub(crate) async fn handle_approvals_decide(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let expected = match params.get("expected_digest") {
            None => None,
            Some(Value::String(digest)) if digest.len() == 64
                && digest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) => {
                let Some(id) = params.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()) else {
                    return WsFrame::error_response("", "id is required");
                };
                Some((id.to_owned(), digest.clone()))
            }
            Some(_) => return WsFrame::error_response("", "expected_digest must be a lowercase SHA-256 hex digest"),
        };
        crate::approval::EXPECTED_DECISION_DIGEST.scope(
            expected,
            crate::approval::EXPECTED_DECISION_HOME.scope(
                self.home_dir.clone(),
                // Keep the large decision future out of both task-local wrappers.
                Box::pin(self.handle_approvals_decide_inner(params, ctx)),
            ),
        ).await
    }

    async fn handle_approvals_decide_inner(
        &self,
        mut params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let id = match params.get("id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "id is required"),
        };
        let approve = params
            .get("approve")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let reason = params
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let broker = match crate::approval::ApprovalBroker::open(&self.home_dir) {
            Ok(b) => b,
            Err(e) => return WsFrame::error_response("", &format!("open approvals: {e}")),
        };
        let approval_id = crate::approval::ApprovalId::from(id.clone());
        // Snapshot the record BEFORE deciding — we need its action_kind +
        // payload to run any post-decision side-effect (e.g. skill_create).
        let record = match Self::required_approval_for_decision(broker.get(&approval_id).await) {
            Ok(rec) => Some(rec),
            Err(response) => return response,
        };
        // SR2: a Personal → Shared delegation card is decided only by the
        // Personal employee's owner (`me.isolation.decide_personal_delegation`),
        // never here — not even by an Admin.
        if record.as_ref().is_some_and(|r| r.action_kind == crate::isolation_background::PERSONAL_DELEGATION_KIND) {
            return WsFrame::error_response("", crate::isolation_background::PERSONAL_DELEGATION_DECIDE_ELSEWHERE);
        }
        // v1.81 P1c: a Personal employee's card — its owner, or an audited Admin step-up (personal_decide_gate.rs).
        if let Some(r) = &record && let Err(frame) = Box::pin(self.personal_decide_gate(ctx, r, super::personal_data_gate::take_step_up(&mut params), "approvals.decide")).await { return frame; }
        if let Some(r) = &record {
            // Both approval and denial can wake work. Gate every kind before
            // specialized paths, expiry polling, or any broker decision.
            let live = match live_reader_context(&self.home_dir, ctx) {
                Ok(live) => live,
                Err(_) => return WsFrame::error_response("", PERMISSION_DENIED),
            };
            // Preserve the existing per-kind refusal before the general main
            // gate. Identity revocation above still takes precedence.
            let kind = crate::governance::ApprovalKind::parse(&r.action_kind);
            // List, get and live push use the same per-kind split:
            // `approval_visibility::dashboard_only_reader`.
            // U5: a request filed from an operator terminal (LINE inbox,
            // continuous responsibilities, forget by source, computer
            // workspaces) is decided only by a current Admin (role re-read
            // from users.db, not the cached context). The kinds and their
            // refusal texts come from one registry.
            if let Some(spec) = crate::approval::operator_cli_gate::spec_for(&r.action_kind) {
                if crate::approval::operator_cli_gate::require_admin_decider(&self.home_dir, ctx)
                    .is_err()
                {
                    return WsFrame::error_response("", spec.admin_refusal);
                }
            }
            // Decision Lab is admin-only. A manager who cannot inspect the
            // exact synthetic run must not mint its human-inspection receipt.
            // Role is re-read from users.db, same as list/get/push, not the
            // handshake snapshot.
            if r.action_kind == "support_pilot_review"
                && crate::approval::operator_cli_gate::require_admin_decider(&self.home_dir, ctx)
                    .is_err()
            {
                return WsFrame::error_response(
                    "",
                    "synthetic pilot review requires an admin who can inspect Decision Lab",
                );
            }
            // v1.82: an autopilot rule action is decided only by a current
            // Admin (role re-read from users.db). Approving it executes the
            // frozen action once; denying records the denial.
            if r.action_kind == crate::autopilot_approval::ACTION_KIND
                && crate::approval::operator_cli_gate::require_admin_decider(&self.home_dir, ctx)
                    .is_err()
            {
                return WsFrame::error_response("", crate::autopilot_approval::ADMIN_REFUSAL);
            }
            // v1.82 (H1): approving an autopilot action REQUIRES the digest
            // of the card the decider was shown (any RPC client). A wrong
            // digest is refused by `check_expected_digest` below and again
            // inside the broker's decision transaction. Denying needs none.
            if r.action_kind == crate::autopilot_approval::ACTION_KIND
                && approve
                && params.get("expected_digest").is_none()
            {
                return WsFrame::error_response("", crate::autopilot_approval::DIGEST_REQUIRED);
            }
            if kind.requires_board()
                && crate::approval::operator_cli_gate::require_admin_decider(&self.home_dir, ctx)
                    .is_err()
            {
                return WsFrame::error_response(
                    "",
                    "this approval kind requires board (admin) rights to decide",
                );
            }
            if !live.is_admin() {
                // agent_id is NOT NULL, but legacy/unbound requests may store
                // an empty string. Only a durable task owner can recover that
                // attribution; arbitrary payload agent labels are not authority.
                let agent_id = if r.agent_id.trim().is_empty() {
                    r.binding.as_ref().and_then(|b| b.task_id.clone())
                        .or_else(|| approval_task_ref(&r.payload))
                        .and_then(|task| task_owner_readonly(&self.home_dir, &task))
                } else {
                    Some(r.agent_id.clone())
                };
                let Some(agent_id) = agent_id else {
                    return WsFrame::error_response("", PERMISSION_DENIED);
                };
                if let Err(frame) = self.deny_main_for_non_admin(&live, &agent_id).await {
                    return frame;
                }
            }
            if let Err(error) = self.authorize_egress_decider(r, ctx) {
                return WsFrame::error_response("", &error);
            }
            // Reject hidden-payload guesses before any digest-dependent outcome,
            // including expired cards and specialized decision paths.
            if params.get("expected_digest").is_some() {
                let fresh = crate::review_evidence::audience::fresh_dashboard_context(&self.home_dir, ctx);
                if fresh.as_ref().map_or(true, |fresh| {
                    super::approvals_topology_rpc::digest_hidden_by_redaction(r, fresh.is_admin())
                }) {
                    return WsFrame::error_response("", "expected_digest is not accepted for this approval");
                }
            }
            if self
                .authorize_workflow_activation_record(&r.payload, ctx)
                .await
                .is_err()
            {
                return WsFrame::error_response("", "permission denied");
            }
            // A card built from a task is decided only by someone who may
            // read that task (live identity + audience), as in the list.
            // Bound cards are judged by `authorize_bound_decider` below.
            if let Some(task_id) = approval_task_ref(&r.payload).filter(|_| r.binding.is_none()) {
                let visible = live_reader_context(&self.home_dir, ctx).is_ok_and(|live| {
                    task_owner_readonly(&self.home_dir, &task_id).is_some_and(|owner| {
                        task_content_visible(&self.home_dir, &live, &task_id, &owner)
                    })
                });
                if !visible {
                    return WsFrame::error_response("", PERMISSION_DENIED);
                }
            }
        }
        // `[approval] forbid_self_approval`: a request raised while an
        // employee worked for this account is not approved by the same
        // account (bound requests are decided by their bound person only).
        if let Some(rec) = record.as_ref().filter(|r| r.binding.is_none()) {
            let me = duduclaw_core::HumanPrincipal::user(&ctx.user_id).map(|p| p.to_string());
            if crate::on_behalf_of::self_approval::refuses(
                &self.home_dir,
                rec.on_behalf_of.as_deref(),
                me.as_deref(),
                approve,
            ) {
                return WsFrame::error_response("", crate::on_behalf_of::self_approval::REFUSAL);
            }
        }
        if let Some(rec) = record.as_ref().filter(|r| r.binding.is_some()) {
            // A-M-3 / U3: who may decide is re-derived from a fresh identity
            // read (employee access, run audience, Admin for activations).
            let decider = match Box::pin(self.authorize_bound_decider(rec, ctx)).await {
                Ok(c) => c,
                Err(e) => return WsFrame::error_response("", &e),
            };
            if rec.action_kind == crate::admin_ai::card::ACTION_KIND {
                let env = crate::approval::decision_service::DecisionEnv {
                    home: self.home_dir.clone(), registry: Some(self.registry.clone()),
                    broker: broker.clone(), users: self.user_db.read().await.clone(),
                };
                return match Box::pin(crate::approval::decision_service::decide(
                    &env, &approval_id, approve, &reason,
                    &crate::approval::decision_service::Decider::Dashboard(decider),
                )).await {
                    Ok(outcome) => WsFrame::ok_response("", outcome.value),
                    Err(error) => error.dashboard_frame(),
                };
            }
            let lock = decision_lock(approval_id.as_str());
            let _guard = lock.lock().await;
            // Chat-admin decisions share this lock; discard the pre-lock snapshot.
            let fresh = match Self::required_approval_for_decision(broker.get(&approval_id).await) {
                Ok(rec) => rec,
                Err(response) => return response,
            };
            let rec = &fresh;
            if rec.is_stale(chrono::Utc::now()) {
                let _ = broker.poll(&approval_id).await;
                let error = if crate::approval_notify::is_dashboard_only_kind(&rec.action_kind) {
                    crate::approval_notify::dashboard_only_expired_text(&rec.action_kind)
                } else { "approval expired" };
                return WsFrame::error_response("", error);
            }
            if rec.status != crate::approval::ApprovalStatus::Pending {
                return WsFrame::error_response("", &format!("approval {} already {}", rec.id, rec.status.as_str()));
            }
            if let Err(error) = crate::approval::check_expected_digest(rec) {
                return WsFrame::error_response("", &error);
            }
            let session = "dashboard:approval";
            if let Err(text) = crate::entry_gate::check_approval_start(&self.home_dir, rec, approve, session).await {
                return WsFrame::error_response("", &text);
            }
            if let Err(e) = Box::pin(broker.decide_bound_dashboard(&approval_id, &decider, approve))
                .await
            {
                return WsFrame::error_response("", &e);
            }
            self.audit_activation_decision(rec, &decider, approve);
            let decider_name = self.user_display_name(&ctx.user_id).await;
            crate::approval_notify::spawn_dashboard_collapse(
                self.home_dir.clone(), rec.clone(), approve, decider_name,
            );
            return WsFrame::ok_response(
                "",
                json!({
                    "id": id,
                    "decided": if approve {"approved"}else{"denied"},
                    "execution_status": "awaiting_execution",
                    "submitter_is_decider": super::approval_decider::submitter_is_decider(rec, &decider),
                }),
            );
        }
        if record.as_ref().is_some_and(crate::approval::decision_service::handles) {
            let env = crate::approval::decision_service::DecisionEnv {
                home: self.home_dir.clone(), registry: Some(self.registry.clone()),
                broker: broker.clone(), users: self.user_db.read().await.clone(),
            };
            return match Box::pin(crate::approval::decision_service::decide_with_reach(
                &env, &approval_id, approve, &reason,
                &crate::approval::decision_service::Decider::Dashboard(ctx.clone()),
                params["reach_digest"].as_str(),
            )).await {
                Ok(outcome) => WsFrame::ok_response("", outcome.value),
                Err(error) => error.dashboard_frame(),
            };
        }
        // L1: a knowledge review past its deadline is a denial (TTL = DENY),
        // even before the expiry sweep has flipped its status — approving it
        // would write knowledge on a card the reviewer could no longer rely on.
        if let Some(rec) = record.as_ref().filter(|r| r.action_kind != "discovery") {
            if rec.is_stale(chrono::Utc::now()) {
                let _ = broker.poll(&approval_id).await;
                let error = if crate::approval_notify::is_dashboard_only_kind(&rec.action_kind) {
                    crate::approval_notify::dashboard_only_expired_text(&rec.action_kind)
                } else { "approval expired" };
                return WsFrame::error_response("", error);
            }
        }
        if let Some(rec) = record.as_ref().filter(|r| r.action_kind != "discovery") {
            if rec.status != crate::approval::ApprovalStatus::Pending {
                return WsFrame::error_response("", &format!("approval {} already {}", rec.id, rec.status.as_str()));
            }
        }
        // Specialized paths check after their own authorization, before writes.
        // The broker also rechecks inside its decision transaction.
        if let Some(rec) = record.as_ref().filter(|r| r.action_kind != "discovery") {
            if let Err(error) = crate::approval::check_expected_digest(rec) {
                return WsFrame::error_response("", &error);
            }
        }
        if record
            .as_ref()
            .is_some_and(|record| record.action_kind == "discovery")
        {
            let caller = match crate::discovery::service::TrustedCaller::from_user(ctx) {
                Ok(caller) => caller,
                Err(error) => return WsFrame::error_response("", &error),
            };
            let store = match self.task_store().await {
                Ok(store) => store,
                Err(frame) => return frame,
            };
            if let Err(error) = crate::discovery::service::prepare_approval_decision(
                &store,
                &broker,
                &caller,
                &approval_id,
                approve,
            )
            .await
            {
                return WsFrame::error_response("", &error);
            }
        }
        if let Some(rec) = &record {
            if let Err(text) = crate::entry_gate::check_approval_start(&self.home_dir, rec, approve, "dashboard:approval").await {
                return WsFrame::error_response("", &text);
            }
        }
        let decided_by = format!("dashboard:{}", ctx.user_id);
        let decision = if record.as_ref().is_some_and(|r| r.payload.get("egress_guard").is_some()) {
            broker.decide_egress_dashboard(&approval_id, ctx, approve).await
        } else {
            broker.decide(&approval_id, approve, &decided_by).await
        };
        if let Err(e) = decision {
            return WsFrame::error_response("", &format!("decide: {e}"));
        }

        if record
            .as_ref()
            .is_some_and(|record| record.action_kind == "discovery")
        {
            let caller = match crate::discovery::service::TrustedCaller::from_user(ctx) {
                Ok(caller) => caller,
                Err(error) => return WsFrame::error_response("", &error),
            };
            let store = match self.task_store().await {
                Ok(store) => store,
                Err(frame) => return frame,
            };
            if let Err(error) = crate::discovery::service::authorize_approved_request(
                &store,
                &broker,
                &caller,
                &approval_id,
            )
            .await
            {
                return WsFrame::error_response("", &error);
            }
        }

        // H1 (unified decision hand-off, 07-unified-decision-design.md §6):
        // a dashboard decision must retire the channel card(s) this approval
        // fanned out to, the same way a channel button press does — otherwise
        // "decided on the dashboard" leaves a stale, still-clickable card
        // behind. Fire-and-forget: an edit is cosmetic and must never delay
        // or fail a decision already durable in `approvals.db`.
        if let Some(rec) = &record {
            let decider_name = self.user_display_name(&ctx.user_id).await;
            crate::approval_notify::spawn_dashboard_collapse(
                self.home_dir.clone(),
                rec.clone(),
                approve,
                decider_name,
            );
        }

        if approve && let Some(rec) = record.as_ref().filter(|r|
            r.action_kind == crate::memory_forget_approval::ACTION_KIND_MEMORY_FORGET_SOURCE
                && r.payload.get("origin").and_then(Value::as_str) == Some(crate::memory_forget_approval::ORIGIN_DISCONNECT))
        {
            let Some(batch_id) = rec.payload.get("batch_id").and_then(Value::as_str) else {
                return WsFrame::error_response("", "disconnect batch missing");
            };
            if let Err(error) = crate::disconnect_forget::enqueue_approved(&self.home_dir, batch_id).await {
                return WsFrame::ok_response("", json!({"id":id,"decided":"approved",
                    "execution_status":"retry_pending","message":error}));
            }
            return WsFrame::ok_response("", json!({"id":id,"decided":"approved","execution_status":"awaiting_execution"}));
        }

        // ── S1 (PORTICO): mint an epoch-bound capability on approve ──
        // Approving is no longer a permanent grant. If the approval payload
        // carries a `scope_epoch` (a task_id / session_id subgoal key), mint
        // a revocable capability handle bound to it; closing that subgoal
        // (task done / session end) auto-revokes the handle. Absent a
        // scope_epoch we skip minting — the legacy one-shot behaviour — so
        // this is additive and never blocks existing approve flows.
        let mut capability_handle: Value = Value::Null;
        if approve {
            if let Some(rec) = &record {
                if let Some(epoch) = rec.payload.get("scope_epoch").and_then(|v| v.as_str()) {
                    if !epoch.is_empty() {
                        match crate::capability::CapabilityBroker::open(&self.home_dir) {
                            Ok(caps) => {
                                let ttl = rec
                                    .payload
                                    .get("capability_ttl_seconds")
                                    .and_then(|v| v.as_i64())
                                    .unwrap_or(rec.ttl_seconds);
                                match caps
                                    .grant_from_approval(&broker, &approval_id, epoch, ttl, None)
                                    .await
                                {
                                    Ok(g) => capability_handle = json!(g.handle_id),
                                    Err(e) => {
                                        // Fail-closed: minting failed ⇒ report,
                                        // do NOT silently proceed as if granted.
                                        return WsFrame::error_response(
                                            "",
                                            &format!("已核准但核發授權憑證失敗（保守拒絕）：{e}"),
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                return WsFrame::error_response(
                                    "",
                                    &format!("開啟授權憑證儲存失敗：{e}"),
                                );
                            }
                        }
                    }
                }
            }
        }

        // ── Side-effect: custom skill creation (V13-T13.0) ──
        // Approver identity is already gated by require_manager!; routing to a
        // human's manager is a fallback here (no manager_id column yet ⇒ any
        // admin/manager may approve; single-admin self-approval is audited).
        let mut side_effect: Value = Value::Null;
        if let Some(rec) = &record {
            if rec.action_kind == crate::custom_skills::ACTION_KIND_SKILL_CREATE {
                if approve {
                    match self
                        .install_approved_custom_skill(&id, rec, &ctx.user_id)
                        .await
                    {
                        Ok(name) => {
                            let created_by = rec
                                .payload
                                .get("created_by_user")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            side_effect = json!({
                                "installed_skill": name,
                                "self_approved": crate::custom_skills::is_self_approval(created_by, &ctx.user_id),
                            });
                        }
                        Err(e) => {
                            return WsFrame::error_response(
                                "",
                                &format!("approved, but install side-effect failed: {e}"),
                            );
                        }
                    }
                } else {
                    // Deny → mark the registry row rejected with the reason.
                    if let Some(cs_id) = rec.payload.get("custom_skill_id").and_then(|v| v.as_str())
                    {
                        if let Ok(store) = self.custom_skill_store() {
                            let _ = store
                                .transition(
                                    cs_id,
                                    crate::custom_skills::CustomSkillStatus::Rejected,
                                    None,
                                    Some(if reason.is_empty() {
                                        "rejected by approver"
                                    } else {
                                        &reason
                                    }),
                                    false,
                                )
                                .await;
                        }
                        side_effect = json!({ "custom_skill_rejected": cs_id });
                    }
                }
            }
        }

        WsFrame::ok_response(
            "",
            json!({
                "id": id,
                "decided": if approve { "approved" } else { "denied" },
                "side_effect": side_effect,
                "capability_handle": capability_handle,
            }),
        )
    }

    pub(crate) async fn handle_approval_operations(
        &self,
        params: Value,
        ctx: &UserContext,
        resolve: bool,
    ) -> WsFrame {
        if !ctx.is_admin() {
            return WsFrame::error_response("", "admin required");
        }
        let broker = match crate::approval::ApprovalBroker::open(&self.home_dir) {
            Ok(b) => b,
            Err(e) => return WsFrame::error_response("", &e),
        };
        if let Err(e) = broker.require_current_dashboard_role(ctx, duduclaw_auth::UserRole::Admin) {
            return WsFrame::error_response("", &e);
        }
        if !resolve {
            if let Err(e) = broker.recover_operations().await {
                return WsFrame::error_response("", &e);
            }
            if let Some(id) = params["operation_id"].as_str() {
                return match broker.inspect_operation(id).await {
                    Ok(Some(row)) => WsFrame::ok_response("", json!({"operation":row})),
                    Ok(None) => WsFrame::error_response("", "operation not found"),
                    Err(e) => WsFrame::error_response("", &e),
                };
            }
            return match broker
                .list_operations_page(
                    params["before_rowid"].as_i64(),
                    params["limit"].as_u64().unwrap_or(200) as usize,
                )
                .await
            {
                Ok((rows, next)) => {
                    WsFrame::ok_response("", json!({"operations":rows,"next_cursor":next}))
                }
                Err(e) => WsFrame::error_response("", &e),
            };
        }
        let Some(id) = params["operation_id"].as_str() else {
            return WsFrame::error_response("", "operation_id required");
        };
        let Some(fence) = params["expected_fence"].as_i64() else {
            return WsFrame::error_response("", "expected_fence required");
        };
        let Some(succeeded) = params["succeeded"].as_bool() else {
            return WsFrame::error_response("", "succeeded required");
        };
        match broker
            .resolve_uncertain(
                id,
                fence,
                succeeded,
                params["receipt"].clone(),
                &ctx.user_id,
                params["reason"].as_str().unwrap_or(""),
            )
            .await
        {
            Ok(()) => WsFrame::ok_response(
                "",
                json!({"operation_id":id,"resolved":true,"retry_allowed":false}),
            ),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// WP14-T14.6: recent budget-breaker incidents from `budget_events.jsonl`,
    /// newest first, plus a per-agent open-count. `limit` default 50.
    pub(crate) async fn handle_budget_incidents(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
        let path = self.home_dir.join("budget_events.jsonl");
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(_) => return WsFrame::error_response("", "Budget incidents unavailable"),
        };
        // Same direct-binding and Personal visibility rule as agents.list.
        // Filter before both pagination and aggregation; hidden rows contribute
        // neither entries nor counts, including unattributed rows.
        let hidden = if ctx.is_admin() {
            Default::default()
        } else {
            match self.personal_hidden_agents(ctx).await {
                Ok(hidden) => hidden,
                Err(reason) => return WsFrame::error_response("", reason),
            }
        };
        let visible = ctx.visible_agents();
        let mut events: Vec<Value> = raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|event| match &visible {
                None => true,
                Some(ids) => event.get("agent_id").and_then(Value::as_str)
                    .is_some_and(|id| !hidden.contains(id) && ids.iter().any(|visible_id| visible_id == id)),
            })
            .collect();
        events.reverse(); // newest first
        let mut per_agent: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        for e in &events {
            if let Some(a) = e.get("agent_id").and_then(|v| v.as_str()) {
                *per_agent.entry(a.to_string()).or_insert(0) += 1;
            }
        }
        let recent: Vec<Value> = events.into_iter().take(limit).collect();
        let agents: Vec<Value> = per_agent
            .into_iter()
            .map(|(agent_id, count)| json!({ "agent_id": agent_id, "open_events": count }))
            .collect();
        WsFrame::ok_response("", json!({ "incidents": recent, "by_agent": agents }))
    }

    /// WP10-T10.1: skill leaderboard ranked by estimated minutes saved. Scans
    /// global + per-agent skills, reads `estimated_minutes_saved` from metadata.
    /// Only skills carrying an estimate (i.e. approved via WP8's flow) appear —
    /// drafts have no estimate and are naturally excluded (no fabricated data).
    pub(crate) async fn handle_skills_leaderboard(&self, params: Value, ctx: &UserContext) -> WsFrame {
        // B8: an employee's own skills are ranked only for callers who may
        // view that employee (read live); company-wide skills stay visible.
        let fresh = match crate::review_evidence::audience::fresh_dashboard_context(&self.home_dir, ctx) {
            Ok(f) => f,
            Err(_) => return WsFrame::error_response("", "permission denied"),
        };
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
        let loc = duduclaw_agent::skill_loader::DEFAULT_SKILL_LOCALE;

        // Build the (dir, scope, owner) scan list, then await each.
        let mut targets: Vec<(std::path::PathBuf, &'static str, String)> =
            vec![(self.home_dir.join("skills"), "global", String::new())];
        if let Ok(rd) = std::fs::read_dir(self.home_dir.join("agents")) {
            let mut agent_dirs: Vec<(String, std::path::PathBuf)> = rd
                .flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| (e.file_name().to_string_lossy().to_string(), e.path()))
                .filter(|(agent, _)| fresh.has_agent_access(agent, AccessLevel::Viewer))
                .collect();
            // B8: stable order regardless of `read_dir`.
            agent_dirs.sort();
            for (agent, path) in agent_dirs {
                targets.push((path.join("SKILLS"), "agent", agent));
            }
        }

        let mut rows: Vec<Value> = Vec::new();
        for (dir, scope, owner) in targets {
            for sk in duduclaw_agent::registry::AgentRegistry::load_skills(&dir).await {
                let meta = duduclaw_agent::skill_loader::parse_skill_meta_from_content(
                    &sk.content,
                    &sk.name,
                );
                if let Some(mins) = meta.estimated_minutes_saved {
                    rows.push(json!({
                        "skill": sk.name,
                        "display_name": meta.display_name(loc),
                        "estimated_minutes_saved": mins,
                        "scope": scope,
                        "owner": owner,
                    }));
                }
            }
        }

        // B8: ties broken by (scope, owner, skill) so equal estimates keep
        // one order across calls and restarts.
        rows.sort_by(|a, b| {
            let mins = |v: &Value| v["estimated_minutes_saved"].as_u64().unwrap_or(0);
            let key = |v: &Value| {
                (
                    v["scope"].as_str().unwrap_or("").to_string(),
                    v["owner"].as_str().unwrap_or("").to_string(),
                    v["skill"].as_str().unwrap_or("").to_string(),
                )
            };
            mins(b).cmp(&mins(a)).then_with(|| key(a).cmp(&key(b)))
        });
        rows.truncate(limit);
        WsFrame::ok_response(
            "",
            json!({
                "leaderboard": rows,
                "metric": "estimated_minutes_saved",
                "note": "Ranked by per-use minutes saved; usage-count multiplication pending a persisted counter.",
            }),
        )
    }
}

/// Audit event: a knowledge-review side effect was applied but its decision
/// could not be recorded.
pub(crate) use crate::approval::decision_service::AUDIT_KNOWLEDGE_REVIEW_DECISION_UNRECORDED;

pub(super) use crate::approval::decision_service::decision_lock;
#[cfg(test)]
pub(crate) use crate::approval::decision_service::force_decide_failure;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approvals_decide_keeps_scoped_inner_future_on_heap() {
        // Check Send and size without constructing a handler or polling a future.
        fn future_size<F: std::future::Future + Send>(
            _: impl FnOnce(&'static MethodHandler, &'static UserContext) -> F,
        ) -> usize {
            std::mem::size_of::<F>()
        }

        let inner = future_size(|handler, ctx| {
            handler.handle_approvals_decide_inner(Value::Null, ctx)
        });
        let inline_scopes = future_size(|handler, ctx| {
            crate::approval::EXPECTED_DECISION_DIGEST.scope(
                None,
                crate::approval::EXPECTED_DECISION_HOME.scope(
                    handler.home_dir.clone(),
                    handler.handle_approvals_decide_inner(Value::Null, ctx),
                ),
            )
        });
        let wrapper = future_size(|handler, ctx| {
            handler.handle_approvals_decide(Value::Null, ctx)
        });
        eprintln!("approvals.decide future bytes: inner={inner}, inline_scopes={inline_scopes}, wrapper={wrapper}");
        assert!(
            wrapper < inner,
            "the task-local wrapper must not embed the decision future: wrapper={wrapper}, inner={inner}"
        );
    }

    fn refusal(frame: &WsFrame) -> String {
        match frame {
            WsFrame::Response { ok: false, error: Some(Value::String(text)), .. } => text.clone(),
            other => panic!("expected an error response, got {other:?}"),
        }
    }

    /// Handshake still says Admin. The users.db row is what decide must read.
    fn claiming_admin(user: &duduclaw_auth::User) -> UserContext {
        let mut ctx = UserContext::admin_fallback();
        ctx.user_id = user.id.clone();
        ctx.email = user.email.clone();
        ctx.role = UserRole::Admin;
        ctx
    }

    /// v1.82: an autopilot rule action card is decided by a current Admin
    /// only (role re-read from users.db), and only on the dashboard.
    #[tokio::test]
    async fn only_a_current_admin_decides_an_autopilot_action_card() {
        let home = tempfile::tempdir().unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let user = db
            .create_user("ada@example.com", "Ada", "fixture-password-only", UserRole::Admin)
            .unwrap();
        let handler = MethodHandler::new(home.path().to_path_buf()).await;
        let ctx = claiming_admin(&user);
        let broker = crate::approval::ApprovalBroker::open(home.path()).unwrap();
        let card = broker
            .request(
                crate::autopilot_approval::REQUESTER,
                crate::autopilot_approval::ACTION_KIND,
                "rule fired",
                json!({}),
                300,
            )
            .await
            .unwrap();
        assert!(crate::approval_notify::is_dashboard_only_kind(crate::autopilot_approval::ACTION_KIND));

        db.update_user(&user.id, None, Some(UserRole::Manager), None).unwrap();
        let frame = handler
            .handle_approvals_decide(json!({"id": card.as_str(), "approve": true}), &ctx)
            .await;
        assert_eq!(refusal(&frame), crate::autopilot_approval::ADMIN_REFUSAL);
        assert_eq!(
            broker.get(&card).await.unwrap().unwrap().status,
            crate::approval::ApprovalStatus::Pending
        );

        db.update_user(&user.id, None, Some(UserRole::Admin), None).unwrap();
        let rec = broker.get(&card).await.unwrap().unwrap();
        let digest = crate::approval::decision_digest_at(home.path(), &rec);
        let frame = handler
            .handle_approvals_decide(
                json!({"id": card.as_str(), "approve": true, "expected_digest": digest}),
                &ctx,
            )
            .await;
        assert!(matches!(frame, WsFrame::Response { ok: true, .. }), "{frame:?}");
        assert_eq!(
            broker.get(&card).await.unwrap().unwrap().status,
            crate::approval::ApprovalStatus::Approved
        );
    }

    /// v1.82 (H1): approving an autopilot action requires the digest of the
    /// card that was shown — absent or wrong is refused for any RPC client;
    /// denying does not need it.
    #[tokio::test]
    async fn approving_an_autopilot_action_requires_the_shown_card_digest() {
        let home = tempfile::tempdir().unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let user = db
            .create_user("ada@example.com", "Ada", "fixture-password-only", UserRole::Admin)
            .unwrap();
        let handler = MethodHandler::new(home.path().to_path_buf()).await;
        let ctx = claiming_admin(&user);
        let broker = crate::approval::ApprovalBroker::open(home.path()).unwrap();
        let file = |payload: Value| {
            let broker = &broker;
            async move {
                broker
                    .request(
                        crate::autopilot_approval::REQUESTER,
                        crate::autopilot_approval::ACTION_KIND,
                        "rule fired",
                        payload,
                        300,
                    )
                    .await
                    .unwrap()
            }
        };
        let card = file(json!({"autopilot_dispatch": {"action": {"type": "delegate", "prompt": "p"}}})).await;
        let status = |id: crate::approval::ApprovalId| {
            let broker = &broker;
            async move { broker.get(&id).await.unwrap().unwrap().status }
        };

        // No digest → refused, still pending.
        let frame = handler
            .handle_approvals_decide(json!({"id": card.as_str(), "approve": true}), &ctx)
            .await;
        assert_eq!(refusal(&frame), crate::autopilot_approval::DIGEST_REQUIRED);
        assert_eq!(status(card.clone()).await, crate::approval::ApprovalStatus::Pending);

        // A wrong (well-formed) digest → refused, still pending.
        let frame = handler
            .handle_approvals_decide(
                json!({"id": card.as_str(), "approve": true, "expected_digest": "0".repeat(64)}),
                &ctx,
            )
            .await;
        assert!(matches!(frame, WsFrame::Response { ok: false, .. }), "{frame:?}");
        assert_eq!(status(card.clone()).await, crate::approval::ApprovalStatus::Pending);

        // The digest of a card that changed after it was shown → refused.
        let shown = broker.get(&card).await.unwrap().unwrap();
        let shown_digest = crate::approval::decision_digest_at(home.path(), &shown);
        broker
            .replace_text(&card, &shown.summary, &json!({"autopilot_dispatch": {"action": {"type": "delegate", "prompt": "other"}}}))
            .await
            .unwrap();
        let frame = handler
            .handle_approvals_decide(
                json!({"id": card.as_str(), "approve": true, "expected_digest": shown_digest}),
                &ctx,
            )
            .await;
        assert!(matches!(frame, WsFrame::Response { ok: false, .. }), "{frame:?}");
        assert_eq!(status(card.clone()).await, crate::approval::ApprovalStatus::Pending);

        // Denying needs no digest.
        let frame = handler
            .handle_approvals_decide(json!({"id": card.as_str(), "approve": false}), &ctx)
            .await;
        assert!(matches!(frame, WsFrame::Response { ok: true, .. }), "{frame:?}");
        assert_eq!(status(card).await, crate::approval::ApprovalStatus::Denied);
    }

    #[tokio::test]
    async fn demoted_or_suspended_admin_cannot_decide_board_or_pilot_cards() {
        let home = tempfile::tempdir().unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let user = db
            .create_user("ada@example.com", "Ada", "fixture-password-only", UserRole::Admin)
            .unwrap();
        // Both restrictions apply: board/pilot wording precedes main refusal.
        crate::trigger_route::tests::write_agent(
            &home.path().join("agents"), "sales", "Sales", "@sales", "main", "",
        );
        let handler = MethodHandler::new(home.path().to_path_buf()).await;
        let mut ctx = claiming_admin(&user);
        let broker = crate::approval::ApprovalBroker::open(home.path()).unwrap();
        let board = broker
            .request("sales", "strategic_plan", "review", json!({}), 300)
            .await
            .unwrap();
        let pilot = broker
            .request("sales", "support_pilot_review", "review", json!({}), 300)
            .await
            .unwrap();

        db.update_user(&user.id, None, Some(UserRole::Manager), None).unwrap();
        let board_frame = handler
            .handle_approvals_decide(json!({"id": board.as_str(), "approve": true, "expected_digest": "0".repeat(64)}), &ctx)
            .await;
        let pilot_frame = handler
            .handle_approvals_decide(json!({"id": pilot.as_str(), "approve": true}), &ctx)
            .await;
        assert_eq!(refusal(&board_frame), "this approval kind requires board (admin) rights to decide");
        assert_eq!(
            refusal(&pilot_frame),
            "synthetic pilot review requires an admin who can inspect Decision Lab"
        );
        assert_eq!(
            broker.get(&board).await.unwrap().unwrap().status,
            crate::approval::ApprovalStatus::Pending
        );
        assert_eq!(
            broker.get(&pilot).await.unwrap().unwrap().status,
            crate::approval::ApprovalStatus::Pending
        );

        db.update_user(&user.id, None, Some(UserRole::Admin), None).unwrap();
        db.set_user_status(&user.id, duduclaw_auth::UserStatus::Suspended).unwrap();
        let board_frame = handler
            .handle_approvals_decide(json!({"id": board.as_str(), "approve": true, "expected_digest": "0".repeat(64)}), &ctx)
            .await;
        let pilot_frame = handler
            .handle_approvals_decide(json!({"id": pilot.as_str(), "approve": true}), &ctx)
            .await;
        // Central admission rejects the revoked identity before consulting
        // specialized kinds or the candidate digest.
        assert_eq!(refusal(&board_frame), PERMISSION_DENIED);
        assert_eq!(refusal(&pilot_frame), PERMISSION_DENIED);
        assert_eq!(
            broker.get(&board).await.unwrap().unwrap().status,
            crate::approval::ApprovalStatus::Pending
        );
        assert_eq!(
            broker.get(&pilot).await.unwrap().unwrap().status,
            crate::approval::ApprovalStatus::Pending
        );

        db.set_user_status(&user.id, duduclaw_auth::UserStatus::Active).unwrap();
        // Reactivation does not restore the credential revoked by suspension.
        ctx = db.live_user_context(&user.id).unwrap().unwrap();
        let board_frame = handler
            .handle_approvals_decide(json!({"id": board.as_str(), "approve": true}), &ctx)
            .await;
        let pilot_frame = handler
            .handle_approvals_decide(json!({"id": pilot.as_str(), "approve": true}), &ctx)
            .await;
        assert!(matches!(board_frame, WsFrame::Response { ok: true, .. }), "{board_frame:?}");
        assert!(matches!(pilot_frame, WsFrame::Response { ok: true, .. }), "{pilot_frame:?}");
        assert_eq!(
            broker.get(&board).await.unwrap().unwrap().status,
            crate::approval::ApprovalStatus::Approved
        );
        assert_eq!(
            broker.get(&pilot).await.unwrap().unwrap().status,
            crate::approval::ApprovalStatus::Approved
        );
    }
}
