//! RM-027 — org chart editing: `org.propose_change`, `org.apply_change`,
//! `org.pending_changes`, and the `approvals.decide` branch for `org_change`.
//!
//! Owner decisions:
//! - a **Manager's** change is a proposal: it files an `org_change` approval
//!   and takes effect only when a current Admin approves it in the dashboard;
//! - an **Admin's** own change takes effect immediately (`org.apply_change`,
//!   after the dashboard's before/after confirmation, which hands back the
//!   `base` it showed), and is audited.
//!
//! Every write path runs inside the **global org mutation lock**
//! ([`org_store::lock_org_mutations`], shared with `agents.update`, the MCP
//! `agent_update` org path and `duduclaw org sync`): re-read the authority
//! strictly (a seeded but lost / unreadable / corrupt / newer-schema
//! `org.toml` refuses the write), compare the `base`, run the shared rule
//! `duduclaw_core::org_change::validate_org_change_in` over the whole tree,
//! then commit — the `org.toml` **authority first**, the `agent.toml` display
//! mirror second. A mirror that fails to follow is recorded as drift and
//! reported; the authority is never left behind a changed mirror. The mirror
//! write's `agent.toml` lock wait is bounded (`MIRROR_LOCK_WAIT`, round 5),
//! so the org lock is never pinned by a stuck writer; for an approval the
//! journal then keeps a "mirror pending" entry that the recovery (a
//! background retry, the next approve click) finishes. Stale state
//! fails closed: a change made against a `base` that no longer matches is
//! refused (and a waiting proposal is denied).

use super::*;
use crate::dept_grants::reach;
use crate::org_change_approval::{self as oc, journal, Proposal};
use duduclaw_core::org_change::{self, OrgChangeError};
use duduclaw_core::org_store::{self, OrgEntry};

use crate::approval::decision_service::{Decider, files_org::{
    coded, coded_with, change_error, authority_error, mirror_failed, validate,
    confirm_reach, reach_refusal_code,
    lock_failed, mirror_of, audit, commit_org_entry, sync_mirror, record_mirror_drift,
}};
pub(crate) use crate::approval::decision_service::files_org::lock_org;
#[cfg(test)]
pub(crate) use crate::approval::decision_service::files_org::decide_failpoint;

const NO_CHANGE: &str = "這個調整和目前的組織位置相同，沒有需要變更的內容。";

fn opt_str(params: &Value, key: &str) -> Result<Option<String>, WsFrame> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(WsFrame::error_response("", &format!("{key} must be a string"))),
    }
}

/// `(agent_id, reports_to?, department?)` — at least one field to change.
fn parse_change(params: &Value) -> Result<(String, Option<String>, Option<String>), WsFrame> {
    let agent_id = match params.get("agent_id").and_then(Value::as_str).map(str::trim) {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => return Err(WsFrame::error_response("", "agent_id is required")),
    };
    let reports_to = opt_str(params, "reports_to")?;
    let department = opt_str(params, "department")?;
    if reports_to.is_none() && department.is_none() {
        return Err(WsFrame::error_response("", "reports_to or department is required"));
    }
    Ok((agent_id, reports_to, department))
}

/// The placement asked for, on top of the current one (absent = unchanged).
fn requested_after(before: &OrgEntry, reports_to: Option<&str>, department: Option<&str>) -> OrgEntry {
    OrgEntry::new(
        reports_to.unwrap_or(before.reports_to.as_str()),
        department.unwrap_or(before.department.as_str()),
    )
}

/// A non-Admin may reorganise an employee they could hand work to: a direct
/// Operator/Owner binding or an Operator-level team grant (`can_assign_to`).
fn may_move(fresh: &UserContext, agent_id: &str) -> bool {
    fresh.is_admin() || fresh.can_assign_to(agent_id)
}

/// …and may name as the new supervisor an employee they can see: any direct
/// binding or a monitoring team grant.
fn may_target(fresh: &UserContext, target: &str) -> bool {
    fresh.is_admin()
        || fresh.has_agent_access(target, AccessLevel::Viewer)
        || super::team_visibility::may_monitor(fresh, target)
}

impl MethodHandler {
    /// `org.propose_change {agent_id, reports_to?, department?, reason?}` —
    /// Manager or above. Files an `org_change` approval; nothing moves yet.
    pub(crate) async fn handle_org_propose_change(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let fresh = match crate::review_evidence::audience::fresh_dashboard_context(&self.home_dir, ctx) {
            Ok(f) if f.has_role(UserRole::Manager) && !f.requires_password_change() => f,
            _ => return WsFrame::error_response("", PERMISSION_DENIED),
        };
        let (agent_id, reports_to, department) = match parse_change(&params) {
            Ok(v) => v,
            Err(frame) => return frame,
        };
        let reason = match opt_str(&params, "reason") {
            Ok(r) => duduclaw_core::truncate_chars(r.as_deref().unwrap_or("").trim(), oc::MAX_REASON_CHARS),
            Err(frame) => return frame,
        };
        // Subject ACL before existence, so a Manager cannot probe ids.
        if !may_move(&fresh, &agent_id) {
            return WsFrame::error_response("", PERMISSION_DENIED);
        }
        if !org_change::agent_exists(&self.home_dir, &agent_id) {
            return change_error(&OrgChangeError::AgentNotFound(agent_id));
        }
        if let Err(frame) = self.deny_main_for_non_admin(&fresh, &agent_id).await {
            return frame;
        }

        // The pending check + filing are atomic against every other org
        // writer, and the base is taken from the authority this lock pins.
        let org_lock = match lock_org(&self.home_dir).await {
            Ok(g) => g,
            Err(e) => return lock_failed(&e),
        };
        let store = match org_lock.load() {
            Ok(s) => s,
            Err(e) => return authority_error(&e),
        };
        let before = org_change::effective_in(&store, &self.home_dir, &agent_id).unwrap_or_default();
        let after = requested_after(&before, reports_to.as_deref(), department.as_deref());
        if after.reports_to != before.reports_to && !fresh.is_admin() {
            if after.reports_to.is_empty() {
                return coded("root_requires_admin", "只有管理員可以把 AI 員工移到最上層（沒有主管）。");
            }
            if !may_target(&fresh, &after.reports_to) {
                return WsFrame::error_response("", PERMISSION_DENIED);
            }
            if org_change::agent_exists(&self.home_dir, &after.reports_to)
                && let Err(frame) = self.deny_main_for_non_admin(&fresh, &after.reports_to).await
            {
                return frame;
            }
        }
        if let Err(e) = validate(&store, &self.home_dir, &agent_id, &after, reports_to.is_some(), department.is_some()) {
            return change_error(&e);
        }
        if after == before {
            return coded("no_change", NO_CHANGE);
        }

        let broker = match crate::approval::ApprovalBroker::open(&self.home_dir) {
            Ok(b) => b,
            Err(e) => return WsFrame::error_response("", &format!("open approvals: {e}")),
        };
        let waiting = match broker.list_by_kind(oc::ACTION_KIND).await {
            Ok(rows) => rows.into_iter().find(|r| oc::live_pending(r) && r.agent_id == agent_id),
            // Cannot prove there is no other request: refuse (fail closed).
            Err(e) => return WsFrame::error_response("", &format!("list approvals: {e}")),
        };
        if let Some(w) = waiting {
            return coded_with(
                "pending_exists",
                &format!(
                    "「{agent_id}」已有一筆組織調整在等待管理員審核（編號 {}），請等這筆審核結束後再提出。",
                    duduclaw_core::truncate_chars(w.id.as_str(), 8)
                ),
                json!({ "approval_id": w.id.as_str() }),
            );
        }

        let proposal = Proposal {
            agent_id: agent_id.clone(),
            before: before.clone(),
            after: after.clone(),
            base: org_change::entry_hash_in(&store, &self.home_dir, &agent_id),
            proposer: fresh.user_id.clone(),
            proposer_email: fresh.email.clone(),
            reason,
        };
        let reach_preview = reach::proposal_preview(&self.home_dir, &before.department, &after.department);
        let mut payload = proposal.to_payload();
        payload.as_object_mut().expect("proposal object")
            .extend(reach_preview.as_object().expect("reach object").clone());
        let summary = oc::summary(&proposal, reach_preview["sentence"].as_str());
        // `[approval] forbid_self_approval` judges `on_behalf_of`: stamp the
        // trusted dashboard principal of the proposer (never a request value).
        let principal = duduclaw_core::HumanPrincipal::user(&fresh.user_id).map(|p| p.to_string());
        let filed = crate::on_behalf_of::ON_BEHALF_OF
            .scope(
                principal,
                broker.request(&agent_id, oc::ACTION_KIND, &summary, payload, oc::TTL_SECS),
            )
            .await;
        drop(org_lock);
        let id = match filed {
            Ok(id) => id,
            Err(e) => return WsFrame::error_response("", &format!("file approval: {e}")),
        };
        audit(
            &self.home_dir,
            "org_change_requested",
            &agent_id,
            duduclaw_security::audit::Severity::Info,
            json!({
                "agent_id": agent_id,
                "approval_id": id.as_str(),
                "before": oc::entry_json(&before),
                "after": oc::entry_json(&after),
                "base": proposal.base,
                "proposer": proposal.proposer,
                "reason": proposal.reason,
            }),
        );
        let expires_at = match broker.get(&id).await {
            Ok(Some(rec)) => oc::expires_at(&rec),
            _ => Value::Null,
        };
        WsFrame::ok_response(
            "",
            json!({
                "status": "pending_approval",
                "approval_id": id.as_str(),
                "agent_id": agent_id,
                "before": oc::entry_json(&before),
                "after": oc::entry_json(&after),
                "base": proposal.base,
                "expires_at": expires_at,
                "reach": reach_preview["reach"],
                "reach_digest": reach_preview["reach_digest"],
                "sentence": reach_preview["sentence"],
            }),
        )
    }

    /// `org.apply_change {agent_id, reports_to?, department?, expected_base}` —
    /// a current Admin only. Applies immediately when `expected_base` still
    /// matches the agent's org entry; a stale base is refused. Re-applying the
    /// current placement while the mirror lags behind the authority re-syncs
    /// the mirror (the retry after a `mirror_sync_failed`).
    pub(crate) async fn handle_org_apply_change(&self, params: Value, ctx: &UserContext) -> WsFrame {
        if crate::approval::operator_cli_gate::require_admin_decider(&self.home_dir, ctx).is_err() {
            return WsFrame::error_response("", "只有管理員可以直接調整組織圖；請改用「提出調整」送交管理員審核。");
        }
        let (agent_id, reports_to, department) = match parse_change(&params) {
            Ok(v) => v,
            Err(frame) => return frame,
        };
        let expected_base = match params.get("expected_base").and_then(Value::as_str) {
            Some(b) if !b.is_empty() => b.to_string(),
            _ => return WsFrame::error_response("", "expected_base is required"),
        };
        if !org_change::agent_exists(&self.home_dir, &agent_id) {
            return change_error(&OrgChangeError::AgentNotFound(agent_id));
        }

        let org_lock = match lock_org(&self.home_dir).await {
            Ok(g) => g,
            Err(e) => return lock_failed(&e),
        };
        let store = match org_lock.load() {
            Ok(s) => s,
            Err(e) => return authority_error(&e),
        };
        let base = org_change::entry_hash_in(&store, &self.home_dir, &agent_id);
        let before = org_change::effective_in(&store, &self.home_dir, &agent_id).unwrap_or_default();
        if base != expected_base {
            return coded_with(
                "stale_base",
                "組織圖在你確認之後已被調整，這次沒有套用；請重新整理，確認最新的調整前後再送出。",
                json!({ "current": oc::entry_json(&before), "current_base": base }),
            );
        }
        let after = requested_after(&before, reports_to.as_deref(), department.as_deref());
        if let Err(e) = validate(&store, &self.home_dir, &agent_id, &after, reports_to.is_some(), department.is_some()) {
            return change_error(&e);
        }
        if after == before {
            // The authority already says this; if the mirror is behind (an
            // earlier mirror sync failed), bring it along now.
            if store.contains(&agent_id) && mirror_of(&self.home_dir, &agent_id).as_ref() != Some(&after) {
                return match sync_mirror(&self.home_dir, &self.registry, &agent_id, &after, "org.apply_change", &Decider::Dashboard(ctx.clone())).await {
                    Ok(reloaded) => {
                        audit(
                            &self.home_dir,
                            "org_change_mirror_resynced",
                            &agent_id,
                            duduclaw_security::audit::Severity::Info,
                            json!({ "agent_id": agent_id, "placement": oc::entry_json(&after), "actor": ctx.user_id }),
                        );
                        WsFrame::ok_response(
                            "",
                            json!({
                                "status": "mirror_synced",
                                "agent_id": agent_id,
                                "placement": oc::entry_json(&after),
                                "base": base,
                                "registry_reloaded": reloaded,
                            }),
                        )
                    }
                    Err(e) => {
                        record_mirror_drift(&self.home_dir, &agent_id, &after, "org.apply_change", &e);
                        mirror_failed(&e, json!({ "applied": true, "agent_id": agent_id }))
                    }
                };
            }
            return coded("no_change", NO_CHANGE);
        }
        let reach_preview = match confirm_reach(&self.home_dir, &before, &after, params["reach_digest"].as_str()) {
            Ok(preview) => preview,
            Err(frame) => {
                audit(&self.home_dir, "org_change_rejected", &agent_id,
                    duduclaw_security::audit::Severity::Info,
                    json!({"agent_id": agent_id, "rejected_by": ctx.user_id,
                        "source": "org.apply_change", "before": oc::entry_json(&before),
                        "after": oc::entry_json(&after), "why": reach_refusal_code(&frame)}));
                return frame;
            }
        };
        let commit = match commit_org_entry(&self.home_dir, &self.registry, &org_lock, &agent_id, &after, "org.apply_change", &Decider::Dashboard(ctx.clone())).await {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &e),
        };

        // A proposal still waiting for this employee was made against the
        // placement that just changed; withdraw it rather than leave a card
        // that can only be refused as stale. A card whose approval was
        // already applied (journaled) is not withdrawn: its retry records it.
        let mut withdrawn: Vec<String> = Vec::new();
        if let Ok(broker) = crate::approval::ApprovalBroker::open(&self.home_dir)
            && let Ok(rows) = broker.list_by_kind(oc::ACTION_KIND).await
        {
            for rec in rows.into_iter().filter(|r| oc::live_pending(r) && r.agent_id == agent_id) {
                if journal::exists(&self.home_dir, rec.id.as_str()) {
                    continue;
                }
                if broker.withdraw(&rec.id, "superseded_by_admin_apply").await.unwrap_or(false) {
                    withdrawn.push(rec.id.as_str().to_string());
                }
            }
        }
        let new_base = org_change::entry_hash(&self.home_dir, &agent_id);
        drop(org_lock);

        audit(
            &self.home_dir,
            "org_change_applied",
            &agent_id,
            duduclaw_security::audit::Severity::Warning,
            json!({
                "agent_id": agent_id,
                "before": oc::entry_json(&before),
                "after": oc::entry_json(&after),
                "actor": ctx.user_id,
                "source": "org.apply_change",
                "approval_id": Value::Null,
                "approved_by": Value::Null,
                "proposer": Value::Null,
                "withdrawn_pending": withdrawn,
                "mirror_synced": commit.mirror_error.is_none(),
            }),
        );
        let result = json!({
            "status": "applied",
            "agent_id": agent_id,
            "before": oc::entry_json(&before),
            "after": oc::entry_json(&after),
            "base": new_base,
            "registry_reloaded": commit.registry_reloaded,
            "withdrawn_pending": withdrawn,
            "reach": reach_preview["reach"],
            "reach_digest": reach_preview["reach_digest"],
            "sentence": reach_preview["sentence"],
        });
        if let Some(e) = commit.mirror_error {
            let mut extra = result;
            extra["applied"] = json!(true);
            return mirror_failed(&e, extra);
        }
        WsFrame::ok_response("", result)
    }

    /// `org.placement {agent_id}` — Manager or above. The employee's
    /// **authoritative** placement (`org.toml`, or the `agent.toml` fallback)
    /// and its `base`: what the before/after confirmation shows and what
    /// `org.apply_change` expects back. `agents.org_chart` shows the display
    /// mirror, which can drift from the authority. Read-only, so tolerant of
    /// an unusable authority — `authority_error` says why writes would refuse.
    pub(crate) async fn handle_org_placement(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let fresh = match crate::review_evidence::audience::fresh_dashboard_context(&self.home_dir, ctx) {
            Ok(f) if f.has_role(UserRole::Manager) && !f.requires_password_change() => f,
            _ => return WsFrame::error_response("", PERMISSION_DENIED),
        };
        let agent_id = match params.get("agent_id").and_then(Value::as_str).map(str::trim) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => return WsFrame::error_response("", "agent_id is required"),
        };
        if !may_move(&fresh, &agent_id) && !may_target(&fresh, &agent_id) {
            return WsFrame::error_response("", PERMISSION_DENIED);
        }
        if !org_change::agent_exists(&self.home_dir, &agent_id) {
            return change_error(&OrgChangeError::AgentNotFound(agent_id));
        }
        let store = org_store::load(&self.home_dir);
        let placement = org_change::effective_entry(&self.home_dir, &agent_id).unwrap_or_default();
        let mirror = mirror_of(&self.home_dir, &agent_id).unwrap_or_default();
        let authority_error = org_store::load_for_write(&self.home_dir).err().map(|e| e.to_string());
        WsFrame::ok_response(
            "",
            json!({
                "agent_id": agent_id,
                "placement": oc::entry_json(&placement),
                "source": if store.contains(&agent_id) { "authority" } else { "mirror" },
                "mirror_drift": mirror != placement,
                "base": org_change::entry_hash(&self.home_dir, &agent_id),
                "authority_error": authority_error,
            }),
        )
    }

    /// `org.pending_changes {agent_id?}` — Manager or above. Waiting
    /// `org_change` requests: an Admin sees all; a Manager sees the ones they
    /// proposed and the ones for employees they may reorganise. A mere
    /// Viewer binding shows nothing (the card carries the proposer's email
    /// and free-text reason).
    pub(crate) async fn handle_org_pending_changes(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let fresh = match crate::review_evidence::audience::fresh_dashboard_context(&self.home_dir, ctx) {
            Ok(f) if f.has_role(UserRole::Manager) && !f.requires_password_change() => f,
            _ => return WsFrame::error_response("", PERMISSION_DENIED),
        };
        let filter = match opt_str(&params, "agent_id") {
            Ok(f) => f.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
            Err(frame) => return frame,
        };
        let broker = match crate::approval::ApprovalBroker::open(&self.home_dir) {
            Ok(b) => b,
            Err(e) => return WsFrame::error_response("", &format!("open approvals: {e}")),
        };
        let rows = match broker.list_by_kind(oc::ACTION_KIND).await {
            Ok(r) => r,
            Err(e) => return WsFrame::error_response("", &format!("list approvals: {e}")),
        };
        let mut items: Vec<(String, Value)> = Vec::new();
        for rec in rows.iter().filter(|r| oc::live_pending(r)) {
            let Some(p) = Proposal::from_record(rec) else { continue };
            if filter.as_deref().is_some_and(|f| f != p.agent_id) {
                continue;
            }
            let visible = fresh.is_admin() || p.proposer == fresh.user_id || may_move(&fresh, &p.agent_id);
            if !visible {
                continue;
            }
            let current_base = org_change::entry_hash(&self.home_dir, &p.agent_id);
            items.push((
                rec.created_at.clone(),
                json!({
                    "approval_id": rec.id.as_str(),
                    "agent_id": p.agent_id,
                    "before": oc::entry_json(&p.before),
                    "after": oc::entry_json(&p.after),
                    "base": p.base,
                    "stale": p.base != current_base,
                    "proposer": p.proposer,
                    "proposer_email": p.proposer_email,
                    "reason": p.reason,
                    "created_at": rec.created_at,
                    "expires_at": oc::expires_at(rec),
                    "reach": rec.payload.get("reach"),
                    "reach_digest": rec.payload.get("reach_digest"),
                    "sentence": rec.payload.get("sentence"),
                }),
            ));
        }
        items.sort_by(|a, b| b.0.cmp(&a.0));
        let pending: Vec<Value> = items.into_iter().map(|(_, v)| v).collect();
        WsFrame::ok_response("", json!({ "count": pending.len(), "pending": pending }))
    }

    /// Compatibility entry point; all decisions share the service lock.
    pub(crate) async fn decide_org_change(
        &self,
        broker: &crate::approval::ApprovalBroker,
        approval_id: &crate::approval::ApprovalId,
        approve: bool,
        decision_reason: &str,
        ctx: &UserContext,
        reach_digest: Option<&str>,
    ) -> WsFrame {
        let env = crate::approval::decision_service::DecisionEnv {
            home: self.home_dir.clone(), registry: Some(self.registry.clone()),
            broker: broker.clone(), users: self.user_db.read().await.clone(),
        };
        match Box::pin(crate::approval::decision_service::decide_with_reach(
            &env, approval_id, approve, decision_reason,
            &crate::approval::decision_service::Decider::Dashboard(ctx.clone()),
            reach_digest,
        )).await {
            Ok(outcome) => WsFrame::ok_response("", outcome.value),
            Err(error) => error.dashboard_frame(),
        }
    }
}
