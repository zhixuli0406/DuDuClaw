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
//! reported; the authority is never left behind a changed mirror. Stale state
//! fails closed: a change made against a `base` that no longer matches is
//! refused (and a waiting proposal is denied).

use super::*;
use crate::org_change_approval::{self as oc, journal, Proposal};
use duduclaw_core::org_change::{self, OrgChangeError};
use duduclaw_core::org_store::{self, OrgEntry, OrgMutationGuard, OrgStore, OrgStoreError};

/// `{code, message}` error frame (the dashboard branches on `code`).
fn coded(code: &str, message: &str) -> WsFrame {
    coded_with(code, message, json!({}))
}

fn coded_with(code: &str, message: &str, extra: Value) -> WsFrame {
    let mut error = json!({ "code": code, "message": message });
    if let (Some(e), Some(x)) = (error.as_object_mut(), extra.as_object()) {
        e.extend(x.clone());
    }
    WsFrame::Response { id: String::new(), ok: false, payload: None, error: Some(error) }
}

fn change_error(e: &OrgChangeError) -> WsFrame {
    coded(e.code(), &e.to_string())
}

fn authority_error(e: &OrgStoreError) -> WsFrame {
    coded(e.code(), &e.to_string())
}

const NO_CHANGE: &str = "這個調整和目前的組織位置相同，沒有需要變更的內容。";

/// Shown when the payload's proposer tries to approve their own request
/// under `[approval] forbid_self_approval`.
const SELF_APPROVAL_REFUSAL: &str =
    "這份組織調整是你自己提出的，依設定不能由你自己同意，請另一位管理員決定（拒絕仍可以）。";

/// The authority moved but the display mirror did not follow.
fn mirror_failed(error: &str, extra: Value) -> WsFrame {
    coded_with(
        "mirror_sync_failed",
        &format!(
            "組織調整已經生效（組織權威資料 org.toml 已更新），但顯示用的 agent.toml 沒有同步成功：{}。\
             組織圖可能暫時顯示舊的位置；請重新整理後，對這位 AI 員工再套用一次相同的位置以重新同步。\
             請勿執行 `duduclaw org sync`——它會用舊的顯示資料蓋掉這次的調整。",
            duduclaw_core::truncate_chars(error, 300)
        ),
        extra,
    )
}

/// Optional string param: absent / `null` ⇒ `None`; any other type is refused.
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

/// Validate exactly the fields the request names (canonicalised values)
/// against `store` — the authority as re-read inside the org lock.
fn validate(
    store: &OrgStore,
    home: &Path,
    agent_id: &str,
    after: &OrgEntry,
    rt: bool,
    dept: bool,
) -> Result<(), OrgChangeError> {
    org_change::validate_org_change_in(
        store,
        home,
        agent_id,
        rt.then_some(after.reports_to.as_str()),
        dept.then_some(after.department.as_str()),
    )
}

/// Take the global, cross-process org mutation lock
/// (`<home>/locks/org-mutation.lock`) on a blocking thread; the guard is held
/// across the caller's `.await`s and released on drop. Shared by every org
/// writer — see [`org_store::OrgMutationGuard`]. Not re-entrant.
pub(crate) async fn lock_org(home: &Path) -> Result<OrgMutationGuard, String> {
    let home = home.to_path_buf();
    match tokio::task::spawn_blocking(move || org_store::lock_org_mutations(&home)).await {
        Ok(Ok(guard)) => Ok(guard),
        Ok(Err(e)) => Err(format!("暫時無法取得組織資料的寫入鎖，沒有做任何變更：{e}")),
        Err(e) => Err(format!("暫時無法取得組織資料的寫入鎖，沒有做任何變更：{e}")),
    }
}

fn lock_failed(message: &str) -> WsFrame {
    coded("org_lock_unavailable", message)
}

/// `<home>/agents/<id>/agent.toml`'s org pair (display mirror).
fn mirror_of(home: &Path, agent_id: &str) -> Option<OrgEntry> {
    org_store::read_mirror(&home.join("agents").join(agent_id).join("agent.toml"))
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

fn audit(home: &Path, event: &str, agent_id: &str, severity: duduclaw_security::audit::Severity, details: Value) {
    crate::security_autopilot::audit_and_emit(
        home,
        &duduclaw_security::audit::AuditEvent::new(event, agent_id, severity, details),
    );
}

/// `org_change_rejected`: the request ended without being applied (`why`:
/// `rejected` by the Admin, `expired`, `stale`, `invalid`, `malformed`).
/// Written only after the terminal state was recorded.
fn audit_rejected(
    home: &Path,
    rec: &crate::approval::ApprovalRecord,
    ctx: &UserContext,
    p: Option<&Proposal>,
    why: &str,
    decision_reason: &str,
) {
    audit(
        home,
        "org_change_rejected",
        &rec.agent_id,
        duduclaw_security::audit::Severity::Info,
        json!({
            "agent_id": rec.agent_id,
            "approval_id": rec.id.as_str(),
            "rejected_by": ctx.user_id,
            "proposer": p.map(|p| p.proposer.clone()),
            "before": p.map(|p| oc::entry_json(&p.before)),
            "after": p.map(|p| oc::entry_json(&p.after)),
            "why": why,
            "reason": duduclaw_core::truncate_chars(decision_reason, oc::MAX_REASON_CHARS),
        }),
    );
}

/// Record a decision in the broker (the one call every `org_change`
/// decision goes through, so a test can make it fail).
async fn record_decision(
    broker: &crate::approval::ApprovalBroker,
    id: &crate::approval::ApprovalId,
    approve: bool,
    decided_by: &str,
) -> Result<(), String> {
    #[cfg(test)]
    {
        if decide_failpoint::armed(id.as_str()) {
            return Err("injected decide failure".into());
        }
    }
    broker.decide(id, approve, decided_by).await
}

/// Test seam: make `record_decision` fail for one approval id.
#[cfg(test)]
pub(crate) mod decide_failpoint {
    use std::collections::BTreeSet;
    use std::sync::Mutex;

    static ARMED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

    pub(crate) fn arm(id: &str) {
        ARMED.lock().unwrap().insert(id.to_string());
    }

    pub(crate) fn disarm(id: &str) {
        ARMED.lock().unwrap().remove(id);
    }

    pub(crate) fn armed(id: &str) -> bool {
        ARMED.lock().map(|s| s.contains(id)).unwrap_or(false)
    }
}

/// What a committed org change did to the display mirror.
struct OrgCommit {
    registry_reloaded: bool,
    /// `Some` when the authority moved but the mirror did not follow.
    mirror_error: Option<String>,
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
        // `[approval] forbid_self_approval` judges `on_behalf_of`: stamp the
        // trusted dashboard principal of the proposer (never a request value).
        let principal = duduclaw_core::HumanPrincipal::user(&fresh.user_id).map(|p| p.to_string());
        let filed = crate::on_behalf_of::ON_BEHALF_OF
            .scope(
                principal,
                broker.request(&agent_id, oc::ACTION_KIND, &oc::summary(&proposal), proposal.to_payload(), oc::TTL_SECS),
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
                return match self.sync_mirror(&agent_id, &after, "org.apply_change", ctx).await {
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
                        self.record_mirror_drift(&agent_id, &after, "org.apply_change", &e);
                        mirror_failed(&e, json!({ "applied": true, "agent_id": agent_id }))
                    }
                };
            }
            return coded("no_change", NO_CHANGE);
        }
        let commit = match self.commit_org_entry(&org_lock, &agent_id, &after, "org.apply_change", ctx).await {
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
                }),
            ));
        }
        items.sort_by(|a, b| b.0.cmp(&a.0));
        let pending: Vec<Value> = items.into_iter().map(|(_, v)| v).collect();
        WsFrame::ok_response("", json!({ "count": pending.len(), "pending": pending }))
    }

    /// `approvals.decide` for `org_change` (dashboard only, current Admin
    /// only). Inside the org lock: re-validates and re-checks the base,
    /// applies FIRST (authority, then mirror), journals the apply, and records
    /// the decision only after the write succeeded. A stale or no longer
    /// valid request is denied; an expired one counts as a denial. A card
    /// whose apply is journaled completes that original decision on retry.
    pub(crate) async fn decide_org_change(
        &self,
        broker: &crate::approval::ApprovalBroker,
        approval_id: &crate::approval::ApprovalId,
        approve: bool,
        decision_reason: &str,
        ctx: &UserContext,
    ) -> WsFrame {
        if crate::approval::operator_cli_gate::require_admin_decider(&self.home_dir, ctx).is_err() {
            return WsFrame::error_response("", "只有管理員可以在儀表板決定組織調整的審核");
        }
        let lock = super::approvals_decide::decision_lock(approval_id.as_str());
        let _held = lock.lock().await;
        let rec = match broker.get(approval_id).await {
            Ok(Some(r)) if r.action_kind == oc::ACTION_KIND => r,
            Ok(_) => return WsFrame::error_response("", "找不到這筆審核"),
            Err(e) => return WsFrame::error_response("", &format!("讀取審核失敗：{e}")),
        };
        // An earlier attempt already applied this approval but could not
        // record it: finish that decision, never re-judge it as stale.
        match journal::read(&self.home_dir, rec.id.as_str()) {
            Ok(Some(applied)) => return self.finish_journaled(broker, &rec, applied, approve, ctx).await,
            Ok(None) => {}
            Err(e) => {
                return WsFrame::error_response("", &format!("讀取這筆組織調整的套用紀錄失敗，沒有做任何變更：{e}"));
            }
        }
        if rec.status != crate::approval::ApprovalStatus::Pending {
            return WsFrame::error_response("", &format!("approval {} already {}", rec.id, rec.status.as_str()));
        }
        let decided_by = format!("dashboard:{}", ctx.user_id);
        if rec.is_stale(chrono::Utc::now()) {
            // Expiry = deny: nothing is applied. Audit only once the expiry
            // is actually recorded.
            return match broker.poll(approval_id).await {
                Ok(status) if status != crate::approval::ApprovalStatus::Pending => {
                    audit_rejected(&self.home_dir, &rec, ctx, Proposal::from_record(&rec).as_ref(), "expired", decision_reason);
                    WsFrame::error_response("", oc::EXPIRED_TEXT)
                }
                Ok(_) => WsFrame::error_response("", oc::EXPIRED_TEXT),
                Err(e) => WsFrame::error_response("", &format!("{}（逾期狀態沒有記錄成功：{e}）", oc::EXPIRED_TEXT)),
            };
        }
        if let Err(error) = crate::approval::check_expected_digest(&rec) {
            return WsFrame::error_response("", &error);
        }
        let Some(proposal) = Proposal::from_record(&rec) else {
            // A malformed card is never applied; deny it so it stops waiting.
            return self
                .deny_org_card(broker, &rec, ctx, None, "malformed", decision_reason,
                    "這筆組織調整的內容不完整，已拒絕；請重新提出。")
                .await;
        };
        // `[approval] forbid_self_approval`: also judged on the payload's
        // proposer (covers cards filed without `on_behalf_of`). Denying one's
        // own request stays possible.
        if approve
            && proposal.proposer == ctx.user_id
            && crate::on_behalf_of::self_approval::enabled(&self.home_dir)
        {
            return WsFrame::error_response("", SELF_APPROVAL_REFUSAL);
        }

        if !approve {
            if let Err(e) = record_decision(broker, approval_id, false, &decided_by).await {
                return WsFrame::error_response("", &format!("decide: {e}"));
            }
            audit_rejected(&self.home_dir, &rec, ctx, Some(&proposal), "rejected", decision_reason);
            self.collapse_org_card(&rec, false, ctx).await;
            return WsFrame::ok_response("", json!({ "id": rec.id.as_str(), "decided": "denied", "side_effect": Value::Null }));
        }

        let org_lock = match lock_org(&self.home_dir).await {
            Ok(g) => g,
            Err(e) => return lock_failed(&e),
        };
        let store = match org_lock.load() {
            // Not the request's fault: it stays pending until the authority
            // is repaired.
            Ok(s) => s,
            Err(e) => return authority_error(&e),
        };
        if org_change::entry_hash_in(&store, &self.home_dir, &proposal.agent_id) != proposal.base {
            return self
                .deny_org_card(broker, &rec, ctx, Some(&proposal), "stale", decision_reason,
                    "審核期間這位 AI 員工的組織位置已被調整，這份申請已拒絕；需要的話請重新提出。")
                .await;
        }
        let rt = proposal.after.reports_to != proposal.before.reports_to;
        let dept = proposal.after.department != proposal.before.department;
        if let Err(e) = validate(&store, &self.home_dir, &proposal.agent_id, &proposal.after, rt, dept) {
            let message = format!("這份申請已不再適用，已拒絕：{e}");
            return self
                .deny_org_card(broker, &rec, ctx, Some(&proposal), "invalid", decision_reason, &message)
                .await;
        }
        let commit = match self
            .commit_org_entry(&org_lock, &proposal.agent_id, &proposal.after, "org.change_approved", ctx)
            .await
        {
            Ok(c) => c,
            Err(e) => {
                return WsFrame::error_response("", &format!("沒有套用，這筆審核仍待決定，可以再試一次：{e}"));
            }
        };
        let applied = journal::Applied {
            approval_id: rec.id.as_str().to_string(),
            agent_id: proposal.agent_id.clone(),
            before: proposal.before.clone(),
            after: proposal.after.clone(),
            approved_by: ctx.user_id.clone(),
            applied_at: chrono::Utc::now().to_rfc3339(),
            registry_reloaded: commit.registry_reloaded,
            mirror_synced: commit.mirror_error.is_none(),
        };
        if let Err(e) = journal::record(&self.home_dir, &applied) {
            // Still try to record the decision; if that fails too, the
            // unrecorded-decision audit row below is the only trace.
            warn!(approval_id = %rec.id, error = %e, "org_change apply journal could not be written");
        }
        drop(org_lock);
        self.complete_applied(broker, &rec, &applied, Some(&proposal), ctx, commit.mirror_error).await
    }

    /// Record the approval of an already-applied change; terminal audit and
    /// the card collapse happen only once the decision is recorded.
    async fn complete_applied(
        &self,
        broker: &crate::approval::ApprovalBroker,
        rec: &crate::approval::ApprovalRecord,
        applied: &journal::Applied,
        proposal: Option<&Proposal>,
        ctx: &UserContext,
        mirror_error: Option<String>,
    ) -> WsFrame {
        let side_effect = json!({
            "applied": true,
            "agent_id": applied.agent_id,
            "before": oc::entry_json(&applied.before),
            "after": oc::entry_json(&applied.after),
            "registry_reloaded": applied.registry_reloaded,
            "mirror_synced": applied.mirror_synced,
        });
        let decided_by = format!("dashboard:{}", applied.approved_by);
        if let Err(e) = record_decision(broker, &rec.id, true, &decided_by).await {
            audit(
                &self.home_dir,
                "org_change_decision_unrecorded",
                &applied.agent_id,
                duduclaw_security::audit::Severity::Warning,
                json!({
                    "approval_id": rec.id.as_str(),
                    "side_effect": side_effect,
                    "journaled": journal::exists(&self.home_dir, rec.id.as_str()),
                    "error": duduclaw_core::truncate_chars(&e, 300),
                }),
            );
            return coded_with(
                "decision_unrecorded",
                &format!(
                    "組織調整已套用，但這筆審核的決定沒有記錄成功（{}）；請再按一次「同意」完成紀錄，不會重複套用。",
                    duduclaw_core::truncate_chars(&e, 300)
                ),
                json!({ "applied": true, "side_effect": side_effect }),
            );
        }
        self.finalize_applied(rec, applied, proposal, ctx).await;
        if let Some(e) = mirror_error {
            return mirror_failed(&e, json!({ "id": rec.id.as_str(), "decided": "approved", "side_effect": side_effect }));
        }
        WsFrame::ok_response("", json!({ "id": rec.id.as_str(), "decided": "approved", "side_effect": side_effect }))
    }

    /// The decision is recorded as approved: drop the journal entry, write
    /// the terminal audit row, retire the channel notice.
    async fn finalize_applied(
        &self,
        rec: &crate::approval::ApprovalRecord,
        applied: &journal::Applied,
        proposal: Option<&Proposal>,
        ctx: &UserContext,
    ) {
        journal::clear(&self.home_dir, rec.id.as_str());
        audit(
            &self.home_dir,
            "org_change_applied",
            &applied.agent_id,
            duduclaw_security::audit::Severity::Warning,
            json!({
                "agent_id": applied.agent_id,
                "before": oc::entry_json(&applied.before),
                "after": oc::entry_json(&applied.after),
                "actor": applied.approved_by,
                "source": "approvals.decide",
                "approval_id": rec.id.as_str(),
                "approved_by": applied.approved_by,
                "recorded_by": ctx.user_id,
                "proposer": proposal.map(|p| p.proposer.clone()),
                "reason": proposal.map(|p| p.reason.clone()),
                "mirror_synced": applied.mirror_synced,
            }),
        );
        self.collapse_org_card(rec, true, ctx).await;
    }

    /// A retry of a card whose approval was already applied (journaled).
    async fn finish_journaled(
        &self,
        broker: &crate::approval::ApprovalBroker,
        rec: &crate::approval::ApprovalRecord,
        applied: journal::Applied,
        approve: bool,
        ctx: &UserContext,
    ) -> WsFrame {
        let proposal = Proposal::from_record(rec);
        match &rec.status {
            crate::approval::ApprovalStatus::Pending => {
                if !approve {
                    // The change is live; recording a denial would lie.
                    return coded_with(
                        "already_applied",
                        "這筆組織調整已經套用，只是審核的決定還沒記錄成功；請按「同意」完成紀錄（不會重複套用）。",
                        json!({ "approval_id": rec.id.as_str() }),
                    );
                }
                if proposal.as_ref().is_some_and(|p| p.proposer == ctx.user_id)
                    && crate::on_behalf_of::self_approval::enabled(&self.home_dir)
                {
                    return WsFrame::error_response("", SELF_APPROVAL_REFUSAL);
                }
                self.complete_applied(broker, rec, &applied, proposal.as_ref(), ctx, None).await
            }
            crate::approval::ApprovalStatus::Approved => {
                // The decision did land; only the bookkeeping was missing.
                self.finalize_applied(rec, &applied, proposal.as_ref(), ctx).await;
                WsFrame::ok_response("", json!({ "id": rec.id.as_str(), "decided": "approved", "side_effect": {
                    "applied": true,
                    "agent_id": applied.agent_id,
                    "before": oc::entry_json(&applied.before),
                    "after": oc::entry_json(&applied.after),
                } }))
            }
            other => {
                // e.g. the TTL sweep expired it after the apply: say so
                // instead of pretending it was not applied.
                audit(
                    &self.home_dir,
                    "org_change_decision_mismatch",
                    &applied.agent_id,
                    duduclaw_security::audit::Severity::Warning,
                    json!({
                        "approval_id": rec.id.as_str(),
                        "recorded_status": other.as_str(),
                        "applied": true,
                        "approved_by": applied.approved_by,
                        "before": oc::entry_json(&applied.before),
                        "after": oc::entry_json(&applied.after),
                    }),
                );
                journal::clear(&self.home_dir, rec.id.as_str());
                WsFrame::error_response(
                    "",
                    &format!(
                        "這筆組織調整其實已經套用，但審核紀錄顯示為「{}」；實際結果已寫入稽核紀錄。需要還原的話，請在組織圖直接調整。",
                        other.as_str()
                    ),
                )
            }
        }
    }

    /// Deny a card that can no longer be applied (stale, invalid, malformed),
    /// audit why, retire its channel notice, and answer with `message`. The
    /// audit row and the collapse follow only a recorded denial.
    #[allow(clippy::too_many_arguments)]
    async fn deny_org_card(
        &self,
        broker: &crate::approval::ApprovalBroker,
        rec: &crate::approval::ApprovalRecord,
        ctx: &UserContext,
        proposal: Option<&Proposal>,
        why: &str,
        decision_reason: &str,
        message: &str,
    ) -> WsFrame {
        let decided_by = format!("dashboard:{}", ctx.user_id);
        if let Err(e) = record_decision(broker, &rec.id, false, &decided_by).await {
            return WsFrame::error_response(
                "",
                &format!(
                    "{message}（但拒絕沒有記錄成功：{}；這筆審核仍待決定，請再試一次）",
                    duduclaw_core::truncate_chars(&e, 300)
                ),
            );
        }
        audit_rejected(&self.home_dir, rec, ctx, proposal, why, decision_reason);
        self.collapse_org_card(rec, false, ctx).await;
        WsFrame::error_response("", message)
    }

    async fn collapse_org_card(&self, rec: &crate::approval::ApprovalRecord, approve: bool, ctx: &UserContext) {
        let name = self.user_display_name(&ctx.user_id).await;
        crate::approval_notify::spawn_dashboard_collapse(self.home_dir.clone(), rec.clone(), approve, name);
    }

    /// Commit one agent's new placement while holding the org lock: the
    /// `org.toml` **authority first** (current Admin re-checked right
    /// before), then the `agent.toml` mirror. `Err` = nothing changed. A
    /// mirror that does not follow is recorded as drift and returned in
    /// [`OrgCommit::mirror_error`] — the authority is the truth either way.
    async fn commit_org_entry(
        &self,
        org_lock: &OrgMutationGuard,
        agent_id: &str,
        after: &OrgEntry,
        source: &'static str,
        ctx: &UserContext,
    ) -> Result<OrgCommit, String> {
        crate::approval::operator_cli_gate::require_admin_decider(&self.home_dir, ctx)
            .map_err(|_| "只有管理員可以套用組織調整".to_string())?;
        if let Err(e) = org_lock.upsert(agent_id, after.clone()) {
            audit(
                &self.home_dir,
                "org_change_authority_write_failed",
                agent_id,
                duduclaw_security::audit::Severity::Warning,
                json!({ "agent_id": agent_id, "after": oc::entry_json(after), "error": e.to_string() }),
            );
            return Err(e.to_string());
        }
        match self.sync_mirror(agent_id, after, source, ctx).await {
            Ok(registry_reloaded) => Ok(OrgCommit { registry_reloaded, mirror_error: None }),
            Err(e) => {
                self.record_mirror_drift(agent_id, after, source, &e);
                Ok(OrgCommit { registry_reloaded: false, mirror_error: Some(e) })
            }
        }
    }

    /// Write the placement into the `agent.toml` display mirror through the
    /// writer `agents.update` uses (current Admin re-checked inside the file
    /// lock). Returns whether the registry re-scan succeeded.
    async fn sync_mirror(
        &self,
        agent_id: &str,
        after: &OrgEntry,
        source: &'static str,
        ctx: &UserContext,
    ) -> Result<bool, String> {
        let home = self.home_dir.clone();
        let decider = ctx.clone();
        let reports_to = after.reports_to.clone();
        let department = after.department.clone();
        let edit = crate::channel_reply::AgentTomlEdit {
            source,
            user_id: &ctx.user_id,
            guard: move || {
                crate::approval::operator_cli_gate::require_admin_decider(&home, &decider)
                    .map_err(|_| "只有管理員可以套用組織調整".to_string())
            },
        };
        crate::channel_reply::update_agent_toml_edit(&self.registry, agent_id, edit, move |table| {
            let agent = table
                .get_mut("agent")
                .and_then(|v| v.as_table_mut())
                .ok_or_else(|| "agent.toml missing [agent] section".to_string())?;
            agent.insert("reports_to".into(), toml::Value::String(reports_to));
            if department.is_empty() {
                agent.remove("department");
            } else {
                agent.insert("department".into(), toml::Value::String(department));
            }
            Ok(())
        })
        .await
    }

    /// Explicit drift record: the authority holds `after`, the mirror did not
    /// follow (`duduclaw doctor` / `org.placement` report it too).
    fn record_mirror_drift(&self, agent_id: &str, after: &OrgEntry, source: &str, error: &str) {
        let drift: Vec<Value> = org_store::detect_drift(&self.home_dir)
            .into_iter()
            .filter(|d| d.agent_id == agent_id)
            .map(|d| json!({ "store": oc::entry_json(&d.store), "mirror": oc::entry_json(&d.mirror) }))
            .collect();
        warn!(agent = %agent_id, error = %error, "org authority committed but the agent.toml mirror did not follow");
        audit(
            &self.home_dir,
            "org_change_mirror_drift",
            agent_id,
            duduclaw_security::audit::Severity::Warning,
            json!({
                "agent_id": agent_id,
                "authority": oc::entry_json(after),
                "source": source,
                "drift": drift,
                "error": duduclaw_core::truncate_chars(error, 300),
            }),
        );
    }
}
