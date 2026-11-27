//! Profile-file and organization side effects, serialized by the decision service.
//! These functions never acquire decision_lock; compatibility RPCs enter via decide.
use std::{path::Path, sync::Arc};
use tokio::sync::RwLock;
use duduclaw_agent::registry::AgentRegistry;
use serde_json::{Value, json};
use tracing::{info, warn};
use super::{DecisionEnv, Decider, DecisionRefusal};
use crate::approval::{ApprovalId, chat_admin::DecisionActor};
use crate::protocol::WsFrame;
use crate::agent_files::{approval as fa, flow};
use crate::org_change_approval::{self as oc, journal, Proposal};
use crate::dept_grants::reach;
use duduclaw_core::org_change::{self, OrgChangeError};
use duduclaw_core::org_store::{self, OrgEntry, OrgMutationGuard, OrgStore, OrgStoreError};

fn require_dashboard_admin(home: &Path, by: &Decider) -> Result<(), String> {
    match by {
        Decider::Dashboard(ctx) => crate::approval::operator_cli_gate::require_admin_decider(home, ctx),
        // identity::authorize has already verified live identity and proof.
        Decider::ChatAdmin(_) => Ok(()),
    }
}

fn actor_decided_by(actor: &DecisionActor) -> String {
    match actor {
        DecisionActor::Dashboard { user_id } => format!("dashboard:{user_id}"),
        DecisionActor::ChatAdmin(entry) => entry.decided_by().to_string(),
    }
}

fn result(frame: WsFrame) -> Result<Value, DecisionRefusal> {
    match frame {
        WsFrame::Response { ok: true, payload: Some(value), .. } => Ok(value),
        WsFrame::Response { ok: false, error: Some(Value::String(error)), .. } =>
            Err(DecisionRefusal::from_kind_error(error)),
        WsFrame::Response { ok: false, error: Some(error), .. } => Err(DecisionRefusal::Kind(error)),
        _ => Err(DecisionRefusal::Internal("invalid decision response".into())),
    }
}

pub(super) async fn agent_file_change(
    env: &DecisionEnv, id: &ApprovalId, approve: bool, by: &Decider, actor: &DecisionActor,
) -> Result<Value, DecisionRefusal> {
    result(Box::pin(agent_file_change_frame(env, actor, id, approve, by)).await)
}

/// Preserve the existing mismatch audit when chat preflight refuses a
/// recovery whose side effect has already landed (e.g. an expired receipt).
pub(super) fn audit_recovery_refusal(env: &DecisionEnv, rec: &crate::approval::ApprovalRecord, error: &DecisionRefusal) {
    if let Ok(Some(applied)) = journal::read(&env.home, rec.id.as_str()) {
        if applied.matches(rec) && applied.state == journal::State::Applied && !applied.decision_recorded {
            audit(&env.home, "org_change_decision_unrecorded", &applied.agent_id,
                duduclaw_security::audit::Severity::Warning,
                json!({"approval_id":rec.id.as_str(), "applied":true, "journaled":true,
                    "decided_by":applied.decided_by, "error":error.dashboard_text()}));
        }
    }
}

pub(super) async fn org_change(
    env: &DecisionEnv, id: &ApprovalId, approve: bool, reason: &str, by: &Decider, actor: &DecisionActor,
    reach_digest: Option<&str>,
) -> Result<Value, DecisionRefusal> {
    result(Box::pin(org_change_frame(env, actor, &env.broker, id, approve, reason, by, reach_digest)).await)
}

async fn agent_file_change_frame(
    env: &DecisionEnv,
    actor: &DecisionActor,
    approval_id: &crate::approval::ApprovalId,
    approve: bool,
    by: &Decider,
) -> WsFrame {
    let broker = &env.broker;
    if require_dashboard_admin(&env.home, by).is_err() {
        return WsFrame::error_response("", "只有管理員可以在儀表板決定設定檔修改的審核");
    }
    let rec = match broker.get(approval_id).await {
        Ok(Some(r)) if r.action_kind == fa::ACTION_KIND => r,
        Ok(_) => return WsFrame::error_response("", "找不到這筆審核"),
        Err(e) => return WsFrame::error_response("", &format!("讀取審核失敗：{e}")),
    };
    if rec.status != crate::approval::ApprovalStatus::Pending {
        return WsFrame::error_response("", &format!("approval {} already {}", rec.id, rec.status.as_str()));
    }
    if rec.is_stale(chrono::Utc::now()) {
        let _ = broker.poll(approval_id).await;
        fa::remove_sidecar(&env.home, approval_id.as_str());
        return WsFrame::error_response("", crate::approval_notify::dashboard_only_expired_text(fa::ACTION_KIND));
    }
    if let Err(error) = crate::approval::check_expected_digest(&rec) {
        return WsFrame::error_response("", &error);
    }
    let decided_by = actor_decided_by(actor);
    if !approve {
        if let Err(e) = broker.decide_as(approval_id, false, actor).await {
            return WsFrame::error_response("", &format!("decide: {e}"));
        }
        fa::remove_sidecar(&env.home, approval_id.as_str());
        collapse_decided(env, &rec, false, by).await;
        return WsFrame::ok_response("", json!({ "id": rec.id.as_str(), "decided": "denied", "side_effect": Value::Null }));
    }
    match apply_agent_file_change(env, &rec, &decided_by, by).await {
        Ok(side_effect) => {
            if let Err(e) = broker.decide_as(approval_id, true, actor).await {
                crate::security_autopilot::audit_and_emit(
                    &env.home,
                    &duduclaw_security::audit::AuditEvent::new(
                        "agent_file_change_decision_unrecorded",
                        rec.agent_id.as_str(),
                        duduclaw_security::audit::Severity::Warning,
                        json!({ "approval_id": rec.id.as_str(), "side_effect": side_effect, "error": duduclaw_core::truncate_chars(&e, 300) }),
                    ),
                );
                return WsFrame::error_response(
                    "",
                    &format!("修改已套用，但這筆審核的決定沒有記錄成功（{}）；已寫入稽核紀錄。", duduclaw_core::truncate_chars(&e, 300)),
                );
            }
            fa::remove_sidecar(&env.home, approval_id.as_str());
            collapse_decided(env, &rec, true, by).await;
            WsFrame::ok_response("", json!({ "id": rec.id.as_str(), "decided": "approved", "side_effect": side_effect }))
        }
        Err(ApplyRefusal::Changed) => {
            let _ = broker.decide_as(approval_id, false, actor).await;
            fa::remove_sidecar(&env.home, approval_id.as_str());
            collapse_decided(env, &rec, false, by).await;
            WsFrame::error_response("", "檔案在審核期間被修改，這份申請已拒絕；請申請者重新送出")
        }
        Err(ApplyRefusal::Other(e)) => {
            WsFrame::error_response("", &format!("沒有套用，這筆審核仍待決定，可以再試一次：{e}"))
        }
    }
}

async fn collapse_decided(env: &DecisionEnv, rec: &crate::approval::ApprovalRecord, approve: bool, by: &Decider) {
    if matches!(by, Decider::ChatAdmin(_)) { return; }
    let name = super::identity::display_name(env, by.user_id()).await;
    crate::approval_notify::spawn_dashboard_collapse(env.home.clone(), rec.clone(), approve, name);
}

async fn apply_agent_file_change(
    env: &DecisionEnv,
    rec: &crate::approval::ApprovalRecord,
    decided_by: &str,
    by: &Decider,
) -> Result<Value, ApplyRefusal> {
    use crate::agent_files::writer::{Commit, CommitError, CommitMeta, commit};
    let (agent_id, key) = fa::target(rec).ok_or_else(|| ApplyRefusal::Other("not a profile file change".into()))?;
    let content = fa::approved_content(&env.home, rec).map_err(ApplyRefusal::Other)?;
    let p = &rec.payload;
    let s = |k: &str| p.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let base_hash = s("base_hash");
    let mut meta = CommitMeta::new(&s("requested_by"), "approval");
    meta.verdict = crate::agent_files::verdict::Verdict::parse(&s("verdict"));
    meta.reason_categories = p
        .get("reason_categories")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    meta.confirm_reason = Some(s("reason")).filter(|r| !r.is_empty());
    meta.approval_id = Some(rec.id.as_str().to_string());
    meta.decided_by = Some(decided_by.to_string());
    meta.severity = duduclaw_security::audit::Severity::Warning;
    let home = env.home.clone();
    let decider = by.clone();
    let result = commit(
        env.registry.as_ref().ok_or_else(|| ApplyRefusal::Other("agent registry unavailable".into()))?,
        Commit {
            home: &env.home,
            agent_id: &agent_id,
            key,
            expected_hash: Some(&base_hash),
            meta,
            baseline: flow::baseline_for(&content, &base_hash),
        },
        move |_| Ok(content),
        move || require_dashboard_admin(&home, &decider),
    )
    .await;
    match result {
        Ok(o) => Ok(json!({ "applied": true, "version_id": o.version_id, "hash": o.hash, "registry_reloaded": o.registry_reloaded })),
        Err(CommitError::Conflict { .. }) => Err(ApplyRefusal::Changed),
        Err(e) => Err(ApplyRefusal::Other(e.message())),
    }
}
enum ApplyRefusal {
    /// The file no longer matches the request's `base_hash`.
    Changed,
    Other(String),
}

/// `{code, message}` error frame (the dashboard branches on `code`).
pub(crate) fn coded(code: &str, message: &str) -> WsFrame {
    coded_with(code, message, json!({}))
}

pub(crate) fn coded_with(code: &str, message: &str, extra: Value) -> WsFrame {
    let mut error = json!({ "code": code, "message": message });
    if let (Some(e), Some(x)) = (error.as_object_mut(), extra.as_object()) {
        e.extend(x.clone());
    }
    WsFrame::Response { id: String::new(), ok: false, payload: None, error: Some(error) }
}

pub(crate) fn change_error(e: &OrgChangeError) -> WsFrame {
    coded(e.code(), &e.to_string())
}

pub(crate) fn authority_error(e: &OrgStoreError) -> WsFrame {
    coded(e.code(), &e.to_string())
}

/// Always recompute from the locked org entry; a missing/unreadable grant
/// store cannot prove an empty impact. Empty impact preserves legacy clients.
pub(crate) fn confirm_reach(home: &Path, before: &OrgEntry, after: &OrgEntry, supplied: Option<&str>)
    -> Result<Value, WsFrame> {
    let lines = reach::for_move(home, &before.department, &after.department)
        .map_err(|_| coded("reach_unavailable", reach::UNAVAILABLE))?;
    let current = reach::preview(&lines);
    if !lines.is_empty() && supplied != current["reach_digest"].as_str() {
        return Err(coded_with("reach_changed", reach::CHANGED, json!({"current": current})));
    }
    Ok(current)
}

pub(crate) fn reach_refusal_code(frame: &WsFrame) -> &str {
    match frame {
        WsFrame::Response { error: Some(error), .. } => error["code"].as_str().unwrap_or("reach_unavailable"),
        _ => "reach_unavailable",
    }
}

/// Shown when the payload's proposer tries to approve their own request
/// under `[approval] forbid_self_approval`.
pub(super) const SELF_APPROVAL_REFUSAL: &str =
    "這份組織調整是你自己提出的，依設定不能由你自己同意，請另一位管理員決定（拒絕仍可以）。";

/// The authority moved but the display mirror did not follow.
pub(crate) fn mirror_failed(error: &str, extra: Value) -> WsFrame {
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

/// RM-027 round 5: the mirror of an applied approval is still behind (the
/// recovery could not write it yet); it is retried automatically.
const MIRROR_STILL_PENDING: &str =
    "顯示用的 agent.toml 目前仍被其他寫入佔用或無法寫入，系統會在稍後自動重試同步";

/// Validate exactly the fields the request names (canonicalised values)
/// against `store` — the authority as re-read inside the org lock.
pub(crate) fn validate(
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

pub(crate) fn lock_failed(message: &str) -> WsFrame {
    coded("org_lock_unavailable", message)
}

/// A journal entry exists for the card but was written for a different
/// payload (or id): fail closed, change nothing.
fn journal_mismatch() -> WsFrame {
    coded(
        "journal_mismatch",
        "這筆組織調整的套用紀錄和目前的審核內容不一致，沒有做任何變更；請管理員檢查 org_change_journal 目錄。",
    )
}

/// `<home>/agents/<id>/agent.toml`'s org pair (display mirror).
pub(crate) fn mirror_of(home: &Path, agent_id: &str) -> Option<OrgEntry> {
    org_store::read_mirror(&home.join("agents").join(agent_id).join("agent.toml"))
}

pub(crate) fn audit(home: &Path, event: &str, agent_id: &str, severity: duduclaw_security::audit::Severity, details: Value) {
    crate::security_autopilot::audit_and_emit(
        home,
        &duduclaw_security::audit::AuditEvent::new(event, agent_id, severity, details),
    );
}

/// `org_change_rejected`: the request ended without being applied (`why`:
/// `rejected` by the Admin, `expired`, `stale`, `invalid`, `malformed`).
/// Reach refusals leave the card pending; other rows follow the terminal decision.
fn audit_rejected(
    home: &Path,
    rec: &crate::approval::ApprovalRecord,
    by: &Decider,
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
            "rejected_by": by.user_id(),
            "proposer": p.map(|p| p.proposer.clone()),
            "before": p.map(|p| oc::entry_json(&p.before)),
            "after": p.map(|p| oc::entry_json(&p.after)),
            "why": why,
            "reason": duduclaw_core::truncate_chars(decision_reason, oc::MAX_REASON_CHARS),
        }),
    );
}

/// Record a decision in the broker (the one call every `org_change`
/// decision goes through, so a test can make it fail). Under a decision
/// lease the decision is recorded through it (the lease keeps everyone
/// else, the TTL sweep included, off the row).
async fn record_decision(
    broker: &crate::approval::ApprovalBroker,
    id: &crate::approval::ApprovalId,
    approve: bool,
    actor: &DecisionActor,
    lease: Option<&crate::approval::DecisionLease>,
) -> Result<(), String> {
    #[cfg(test)]
    {
        if decide_failpoint::armed(id.as_str()) {
            return Err("injected decide failure".into());
        }
    }
    match lease {
        Some(lease) => broker.decide_as_with_lease(id, lease, approve, actor).await,
        None => broker.decide_as(id, approve, actor).await,
    }
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
pub(crate) struct OrgCommit {
    pub(crate) registry_reloaded: bool,
    /// `Some` when the authority moved but the mirror did not follow.
    pub(crate) mirror_error: Option<String>,
}

async fn org_change_frame(
    env: &DecisionEnv, actor: &DecisionActor,
    broker: &crate::approval::ApprovalBroker,
    approval_id: &crate::approval::ApprovalId,
    approve: bool,
    decision_reason: &str,
    by: &Decider,
    reach_digest: Option<&str>,
) -> WsFrame {
    if require_dashboard_admin(&env.home, by).is_err() {
        return WsFrame::error_response("", "只有管理員可以在儀表板決定組織調整的審核");
    }
    let rec = match broker.get(approval_id).await {
        Ok(Some(r)) if r.action_kind == oc::ACTION_KIND => r,
        Ok(_) => return WsFrame::error_response("", "找不到這筆審核"),
        Err(e) => return WsFrame::error_response("", &format!("讀取審核失敗：{e}")),
    };
    // An earlier attempt already applied this approval but could not
    // record it: finish that decision, never re-judge it as stale. One
    // that stopped around the commit (prepared) is settled first.
    match journal::read(&env.home, rec.id.as_str()) {
        Ok(Some(entry)) if !entry.matches(&rec) => return journal_mismatch(),
        Ok(Some(entry)) if entry.state == journal::State::Applied => {
            return finish_journaled(env, broker, &rec, entry, approve, by).await;
        }
        Ok(Some(_prepared)) => match settle_prepared(env, &rec).await {
            Ok(Some(applied)) => return finish_journaled(env, broker, &rec, applied, approve, by).await,
            Ok(None) => {}
            Err(frame) => return frame,
        },
        Ok(None) => {}
        Err(e) => {
            return WsFrame::error_response("", &format!("讀取這筆組織調整的套用紀錄失敗，沒有做任何變更：{e}"));
        }
    }
    if rec.status != crate::approval::ApprovalStatus::Pending {
        return WsFrame::error_response("", &format!("approval {} already {}", rec.id, rec.status.as_str()));
    }
    if rec.is_stale(chrono::Utc::now()) {
        return expire_org_card(env, broker, &rec, by, decision_reason).await;
    }
    if let Err(error) = crate::approval::check_expected_digest(&rec) {
        return WsFrame::error_response("", &error);
    }
    let Some(proposal) = Proposal::from_record(&rec) else {
        // A malformed card is never applied; deny it so it stops waiting.
        return deny_org_card(env, actor, broker, &rec, by, None, "malformed", decision_reason,
                "這筆組織調整的內容不完整，已拒絕；請重新提出。", None)
            .await;
    };
    // `[approval] forbid_self_approval`: also judged on the payload's
    // proposer (covers cards filed without `on_behalf_of`). Denying one's
    // own request stays possible.
    if approve
        && proposal.proposer == by.user_id()
        && crate::on_behalf_of::self_approval::enabled(&env.home)
    {
        return WsFrame::error_response("", SELF_APPROVAL_REFUSAL);
    }

    if !approve {
        if let Err(e) = record_decision(broker, approval_id, false, actor, None).await {
            return WsFrame::error_response("", &format!("decide: {e}"));
        }
        audit_rejected(&env.home, &rec, by, Some(&proposal), "rejected", decision_reason);
        collapse_org_card(env, &rec, false, by).await;
        return WsFrame::ok_response("", json!({ "id": rec.id.as_str(), "decided": "denied", "side_effect": Value::Null }));
    }

    // The lease is taken atomically, inside the TTL, before any wait.
    let lease = match broker.take_decision_lease(approval_id, oc::DECISION_LEASE_SECS).await {
        Ok(Some(lease)) => lease,
        Ok(None) => {
            return match broker.get(approval_id).await {
                Ok(Some(now)) if now.is_stale(chrono::Utc::now()) => {
                    expire_org_card(env, broker, &now, by, decision_reason).await
                }
                Ok(Some(now)) if now.status != crate::approval::ApprovalStatus::Pending => {
                    WsFrame::error_response("", &format!("approval {} already {}", now.id, now.status.as_str()))
                }
                _ => coded(
                    "decision_in_progress",
                    "另一位管理員正在處理這筆組織調整的審核，這次沒有做任何變更；請稍後重新整理再看結果。",
                ),
            };
        }
        Err(e) => return WsFrame::error_response("", &format!("沒有套用，無法鎖定這筆審核：{e}")),
    };
    // RM-027 round 4: the lease is kept alive in the background for the
    // whole decision — the org-lock wait, prepared journal, authority,
    // mirror and the terminal decision — so the TTL sweep can never
    // expire the card under a commit that is still landing. Round 5: the
    // mirror's `agent.toml` lock wait is bounded (`oc::MIRROR_LOCK_WAIT`)
    // and a committed lease lives at most `oc::COMMITTED_LEASE_MAX`;
    // past that the applied journal's recovery records the outcome.
    let keeper =
        broker.keep_decision_lease(approval_id, &lease, oc::DECISION_LEASE_SECS, oc::COMMITTED_LEASE_MAX);
    let frame = apply_leased(env, actor, broker, &lease, &keeper, approval_id, by, decision_reason, reach_digest).await;
    drop(keeper);
    // A lease the decision did not consume is given back (no-op otherwise).
    broker.release_decision_lease(approval_id, &lease).await;
    frame
}

/// The approve path once the decision lease is held: everything from the
/// org lock to the recorded decision.
async fn apply_leased(
    env: &DecisionEnv, actor: &DecisionActor,
    broker: &crate::approval::ApprovalBroker,
    lease: &crate::approval::DecisionLease,
    keeper: &crate::approval::LeaseKeeper,
    approval_id: &crate::approval::ApprovalId,
    by: &Decider,
    decision_reason: &str,
    reach_digest: Option<&str>,
) -> WsFrame {
    let Some(registry) = env.registry.as_ref() else {
        return WsFrame::error_response("", "agent registry unavailable");
    };
    let org_lock = match lock_org(&env.home).await {
        Ok(g) => g,
        Err(e) => return lock_failed(&e),
    };
    // Re-read the card now that the org lock is held: the wait may have
    // outlived the approval's TTL (or the lease).
    let rec = match broker.get(approval_id).await {
        Ok(Some(r)) if r.action_kind == oc::ACTION_KIND => r,
        Ok(_) => return WsFrame::error_response("", "找不到這筆審核"),
        Err(e) => return WsFrame::error_response("", &format!("讀取審核失敗，沒有套用：{e}")),
    };
    if rec.status != crate::approval::ApprovalStatus::Pending {
        return WsFrame::error_response("", &format!("approval {} already {}", rec.id, rec.status.as_str()));
    }
    if rec.is_stale(chrono::Utc::now()) {
        drop(org_lock);
        broker.release_decision_lease(approval_id, lease).await;
        return expire_org_card(env, broker, &rec, by, decision_reason).await;
    }
    match broker.renew_decision_lease(approval_id, lease, oc::DECISION_LEASE_SECS).await {
        Ok(true) => {}
        Ok(false) => {
            drop(org_lock);
            broker.release_decision_lease(approval_id, lease).await;
            return match broker.get(approval_id).await {
                Ok(Some(now)) if now.is_stale(chrono::Utc::now()) => {
                    expire_org_card(env, broker, &now, by, decision_reason).await
                }
                _ => coded(
                    "decision_lease_lost",
                    "等待組織資料寫入鎖的時間太久，這次沒有套用；這筆審核仍待決定，請再按一次「同意」。",
                ),
            };
        }
        Err(e) => return WsFrame::error_response("", &format!("沒有套用，無法確認這筆審核的鎖定：{e}")),
    }
    if let Err(error) = crate::approval::check_expected_digest(&rec) {
        return WsFrame::error_response("", &error);
    }
    let Some(proposal) = Proposal::from_record(&rec) else {
        return deny_org_card(env, actor, broker, &rec, by, None, "malformed", decision_reason,
                "這筆組織調整的內容不完整，已拒絕；請重新提出。", Some(lease))
            .await;
    };
    let store = match org_lock.load() {
        // Not the request's fault: it stays pending until the authority
        // is repaired.
        Ok(s) => s,
        Err(e) => return authority_error(&e),
    };
    if org_change::entry_hash_in(&store, &env.home, &proposal.agent_id) != proposal.base {
        return deny_org_card(env, actor, broker, &rec, by, Some(&proposal), "stale", decision_reason,
                "審核期間這位 AI 員工的組織位置已被調整，這份申請已拒絕；需要的話請重新提出。", Some(lease))
            .await;
    }
    let rt = proposal.after.reports_to != proposal.before.reports_to;
    let dept = proposal.after.department != proposal.before.department;
    if let Err(e) = validate(&store, &env.home, &proposal.agent_id, &proposal.after, rt, dept) {
        let message = format!("這份申請已不再適用，已拒絕：{e}");
        return deny_org_card(env, actor, broker, &rec, by, Some(&proposal), "invalid", decision_reason, &message, Some(lease))
            .await;
    }
    let before = org_change::effective_in(&store, &env.home, &proposal.agent_id).unwrap_or_default();
    if let Err(frame) = confirm_reach(&env.home, &before, &proposal.after, reach_digest) {
        audit_rejected(&env.home, &rec, by, Some(&proposal), reach_refusal_code(&frame), decision_reason);
        return frame;
    }
    // No prepared journal entry ⇒ no apply: a crash after the commit must
    // always leave a trace a retry can complete from.
    let prepared = journal::Applied::prepared(&rec, &proposal.agent_id, &proposal.before, &proposal.after, by.user_id())
        .with_actor(actor);
    if let Err(e) = journal::record(&env.home, &prepared) {
        return coded(
            "journal_unavailable",
            &format!(
                "無法寫入這筆組織調整的套用紀錄，為了避免套用後無從追蹤，這次沒有套用；這筆審核仍待決定，請管理員檢查 org_change_journal 目錄後再試：{}",
                duduclaw_core::truncate_chars(&e, 300)
            ),
        );
    }
    // RM-027 round 4: the final in-TTL check, right before the authority
    // commit. A lease that lapsed or a TTL that passed since the org lock
    // was taken aborts cleanly here — nothing was committed, the prepared
    // entry is dropped and the card is expired / left pending. Past this
    // point the keeper extends the lease without the TTL until the
    // terminal decision is recorded.
    match broker.renew_decision_lease(approval_id, lease, oc::DECISION_LEASE_SECS).await {
        Ok(true) if !keeper.lost() => keeper.mark_committed(),
        Ok(_) => {
            journal::clear(&env.home, rec.id.as_str());
            drop(org_lock);
            broker.release_decision_lease(approval_id, lease).await;
            return match broker.get(approval_id).await {
                Ok(Some(now)) if now.is_stale(chrono::Utc::now()) => {
                    expire_org_card(env, broker, &now, by, decision_reason).await
                }
                _ => coded(
                    "decision_lease_lost",
                    "這筆審核的鎖定在套用前失效，這次沒有套用；這筆審核仍待決定，請再按一次「同意」。",
                ),
            };
        }
        Err(e) => {
            journal::clear(&env.home, rec.id.as_str());
            return WsFrame::error_response("", &format!("沒有套用，無法確認這筆審核的鎖定：{e}"));
        }
    }
    let commit = match commit_org_entry(&env.home, registry, &org_lock, &proposal.agent_id, &proposal.after, "org.change_approved", by)
        .await
    {
        Ok(c) => c,
        Err(e) => {
            // Nothing was committed: the prepared entry has no meaning.
            journal::clear(&env.home, rec.id.as_str());
            return WsFrame::error_response("", &format!("沒有套用，這筆審核仍待決定，可以再試一次：{e}"));
        }
    };
    // A mirror that did not follow (a refused write, or its bounded
    // `agent.toml` lock wait ran out) is journaled as pending: the org
    // lock is released right below, the approval is still recorded
    // (honestly: the authority moved), and the recovery finishes the
    // mirror later.
    let applied = prepared.into_applied(commit.registry_reloaded, commit.mirror_error.is_none());
    if let Err(e) = journal::record(&env.home, &applied) {
        // The prepared entry stays: a retry settles it against the
        // authority (which now holds `after`) under the org lock.
        warn!(approval_id = %rec.id, error = %e, "org_change journal could not be marked applied (prepared entry kept)");
    }
    drop(org_lock);
    complete_applied(env, broker, &rec, &applied, Some(&proposal), by, commit.mirror_error, Some(lease)).await
}

/// Expiry = deny: nothing is applied. Audit only once the expiry is
/// actually recorded.
async fn expire_org_card(
    env: &DecisionEnv,
    broker: &crate::approval::ApprovalBroker,
    rec: &crate::approval::ApprovalRecord,
    by: &Decider,
    decision_reason: &str,
) -> WsFrame {
    match broker.poll(&rec.id).await {
        Ok(status) if status != crate::approval::ApprovalStatus::Pending => {
            audit_rejected(&env.home, rec, by, Proposal::from_record(rec).as_ref(), "expired", decision_reason);
            WsFrame::error_response("", oc::EXPIRED_TEXT)
        }
        Ok(_) => WsFrame::error_response("", oc::EXPIRED_TEXT),
        Err(e) => WsFrame::error_response("", &format!("{}（逾期狀態沒有記錄成功：{e}）", oc::EXPIRED_TEXT)),
    }
}

/// A *prepared* journal entry: an earlier attempt stopped around the
/// authority commit. Settled under the org lock (so no attempt is
/// mid-commit), against the authority: `Ok(Some(applied))` — the commit
/// landed (the entry is upgraded and the original decision completes);
/// `Ok(None)` — it did not (the entry is dropped and the request is
/// judged normally). When the authority holds neither the base nor the
/// approved placement, nothing is guessed: the entry is dropped with an
/// audit row and the request is judged normally (a moved base denies it).
async fn settle_prepared(
    env: &DecisionEnv,
    rec: &crate::approval::ApprovalRecord,
) -> Result<Option<journal::Applied>, WsFrame> {
    let org_lock = lock_org(&env.home).await.map_err(|e| lock_failed(&e))?;
    // Re-read under the lock: a concurrent attempt may have finished it.
    let entry = match journal::read(&env.home, rec.id.as_str()) {
        Ok(Some(e)) if !e.matches(rec) => return Err(journal_mismatch()),
        Ok(Some(e)) => e,
        Ok(None) => return Ok(None),
        Err(e) => {
            return Err(WsFrame::error_response("", &format!("讀取這筆組織調整的套用紀錄失敗，沒有做任何變更：{e}")));
        }
    };
    if entry.state == journal::State::Applied {
        return Ok(Some(entry));
    }
    let store = org_lock.load().map_err(|e| authority_error(&e))?;
    let base_unchanged = Proposal::from_record(rec)
        .is_some_and(|p| org_change::entry_hash_in(&store, &env.home, &p.agent_id) == p.base);
    if base_unchanged {
        journal::clear(&env.home, rec.id.as_str());
        return Ok(None);
    }
    if store.get(&entry.agent_id) == Some(&entry.after) {
        let mirror_synced = mirror_of(&env.home, &entry.agent_id).as_ref() == Some(&entry.after);
        let applied = entry.into_applied(false, mirror_synced);
        if let Err(e) = journal::record(&env.home, &applied) {
            return Err(WsFrame::error_response(
                "",
                &format!("這筆組織調整上次已經套用，但套用紀錄無法更新，這次沒有做任何變更；請再試一次：{e}"),
            ));
        }
        return Ok(Some(applied));
    }
    audit(
        &env.home,
        "org_change_journal_ambiguous",
        &entry.agent_id,
        duduclaw_security::audit::Severity::Warning,
        json!({
            "approval_id": rec.id.as_str(),
            "agent_id": entry.agent_id,
            "before": oc::entry_json(&entry.before),
            "after": oc::entry_json(&entry.after),
            "authority": store.get(&entry.agent_id).map(oc::entry_json),
        }),
    );
    journal::clear(&env.home, rec.id.as_str());
    Ok(None)
}

/// Record the approval of an already-applied change; terminal audit and
/// the card collapse happen only once the decision is recorded.
#[allow(clippy::too_many_arguments)]
async fn complete_applied(
    env: &DecisionEnv,
    broker: &crate::approval::ApprovalBroker,
    rec: &crate::approval::ApprovalRecord,
    applied: &journal::Applied,
    proposal: Option<&Proposal>,
    by: &Decider,
    mirror_error: Option<String>,
    lease: Option<&crate::approval::DecisionLease>,
) -> WsFrame {
    let side_effect = json!({
        "applied": true,
        "agent_id": applied.agent_id,
        "before": oc::entry_json(&applied.before),
        "after": oc::entry_json(&applied.after),
        "registry_reloaded": applied.registry_reloaded,
        "mirror_synced": applied.mirror_synced,
    });
    let recorded = match applied.decision_actor() {
        Ok(saved_actor) => record_decision(broker, &rec.id, true, &saved_actor, lease).await,
        Err(error) => Err(error),
    };
    if let Err(e) = recorded {
        audit(
            &env.home,
            "org_change_decision_unrecorded",
            &applied.agent_id,
            duduclaw_security::audit::Severity::Warning,
            json!({
                "approval_id": rec.id.as_str(),
                "side_effect": side_effect,
                "journaled": journal::exists(&env.home, rec.id.as_str()),
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
    finalize_applied(env, rec, applied, proposal, by).await;
    if let Some(e) = mirror_error {
        return mirror_failed(
            &e,
            json!({
                "id": rec.id.as_str(),
                "decided": "approved",
                "side_effect": side_effect,
                "mirror_pending": true,
                "lock_wait_timeout": crate::agent_files::writer::is_lock_wait_timeout(&e),
            }),
        );
    }
    WsFrame::ok_response("", json!({ "id": rec.id.as_str(), "decided": "approved", "side_effect": side_effect }))
}

/// The decision is recorded as approved: drop the journal entry, write
/// the terminal audit row, retire the channel notice. RM-027 round 5: a
/// mirror that is still behind keeps the entry as the "mirror pending"
/// record (`decision_recorded`) and schedules its recovery.
async fn finalize_applied(
    env: &DecisionEnv,
    rec: &crate::approval::ApprovalRecord,
    applied: &journal::Applied,
    proposal: Option<&Proposal>,
    by: &Decider,
) {
    if applied.mirror_synced {
        journal::clear(&env.home, rec.id.as_str());
    } else {
        let pending = applied.clone().mirror_pending_after_decision();
        if let Err(e) = journal::record(&env.home, &pending) {
            warn!(approval_id = %rec.id, error = %e,
                  "org_change mirror-pending journal entry could not be written (re-apply the placement to re-sync)");
        }
        spawn_mirror_recovery(env, rec.id.as_str());
    }
    audit(
        &env.home,
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
            "decided_by": applied.decided_by,
            "recorded_by": by.user_id(),
            "proposer": proposal.map(|p| p.proposer.clone()),
            "reason": proposal.map(|p| p.reason.clone()),
            "mirror_synced": applied.mirror_synced,
        }),
    );
    if matches!(by, Decider::Dashboard(_)) {
        let name = super::identity::display_name(env, &applied.approved_by).await;
        crate::approval_notify::spawn_dashboard_collapse(env.home.clone(), rec.clone(), true, name);
    }
}

/// A retry of a card whose approval was already applied (journaled).
async fn finish_journaled(
    env: &DecisionEnv,
    broker: &crate::approval::ApprovalBroker,
    rec: &crate::approval::ApprovalRecord,
    applied: journal::Applied,
    approve: bool,
    by: &Decider,
) -> WsFrame {
    let proposal = Proposal::from_record(rec);
    // RM-027 round 5: a mirror the first attempt could not write (e.g.
    // its bounded `agent.toml` lock wait ran out) is finished first —
    // under the org lock, against the authority, bounded again. An
    // approve click on an applied card is its recovery path. A denial
    // never touches it (the change is live either way).
    let applied = if approve && !applied.mirror_synced {
        try_finish_mirror(env, rec, applied).await
    } else {
        applied
    };
    let mirror_error = (!applied.mirror_synced).then(|| MIRROR_STILL_PENDING.to_string());
    match &rec.status {
        crate::approval::ApprovalStatus::Approved if applied.decision_recorded => {
            // The decision and its audit landed earlier; only the mirror
            // was pending. Nothing about the decision is redone.
            let side_effect = json!({
                "applied": true,
                "agent_id": applied.agent_id,
                "before": oc::entry_json(&applied.before),
                "after": oc::entry_json(&applied.after),
                "mirror_synced": applied.mirror_synced,
            });
            let body = json!({ "id": rec.id.as_str(), "decided": "approved", "side_effect": side_effect });
            match mirror_error {
                Some(e) => {
                    let mut extra = body;
                    extra["mirror_pending"] = json!(true);
                    mirror_failed(&e, extra)
                }
                None => WsFrame::ok_response("", body),
            }
        }
        crate::approval::ApprovalStatus::Pending => {
            if !approve {
                // The change is live; recording a denial would lie.
                return coded_with(
                    "already_applied",
                    "這筆組織調整已經套用，只是審核的決定還沒記錄成功；請按「同意」完成紀錄（不會重複套用）。",
                    json!({ "approval_id": rec.id.as_str() }),
                );
            }
            if proposal.as_ref().is_some_and(|p| p.proposer == by.user_id())
                && crate::on_behalf_of::self_approval::enabled(&env.home)
            {
                return WsFrame::error_response("", SELF_APPROVAL_REFUSAL);
            }
            // RM-027 round 4: the recovery records the decision under a
            // decision lease like the first attempt — without one the TTL
            // sweep could expire the card between this read and the write.
            let lease = match broker.take_decision_lease(&rec.id, oc::DECISION_LEASE_SECS).await {
                Ok(Some(lease)) => lease,
                Ok(None) => {
                    return match broker.poll(&rec.id).await {
                        // Another attempt recorded the approval meanwhile.
                        Ok(crate::approval::ApprovalStatus::Approved) => {
                            finalize_applied(env, rec, &applied, proposal.as_ref(), by).await;
                            WsFrame::ok_response("", json!({ "id": rec.id.as_str(), "decided": "approved", "side_effect": {
                                "applied": true,
                                "agent_id": applied.agent_id,
                                "before": oc::entry_json(&applied.before),
                                "after": oc::entry_json(&applied.after),
                            } }))
                        }
                        // The TTL passed: report the recorded outcome
                        // against the apply instead of hiding it.
                        Ok(status) if status != crate::approval::ApprovalStatus::Pending => {
                            journaled_status_mismatch(env, rec, &applied, &status)
                        }
                        _ => coded(
                            "decision_in_progress",
                            "另一位管理員正在處理這筆組織調整的審核，這次沒有做任何變更；請稍後重新整理再看結果。",
                        ),
                    };
                }
                Err(e) => {
                    return WsFrame::error_response("", &format!("無法鎖定這筆審核，決定沒有記錄：{e}"));
                }
            };
            let keeper =
                broker.keep_decision_lease(&rec.id, &lease, oc::DECISION_LEASE_SECS, oc::COMMITTED_LEASE_MAX);
            // The change is already committed: the TTL no longer applies.
            keeper.mark_committed();
            let frame = complete_applied(env, broker, rec, &applied, proposal.as_ref(), by, mirror_error, Some(&lease))
                .await;
            drop(keeper);
            broker.release_decision_lease(&rec.id, &lease).await;
            frame
        }
        crate::approval::ApprovalStatus::Approved => {
            // The decision did land; only the bookkeeping was missing.
            finalize_applied(env, rec, &applied, proposal.as_ref(), by).await;
            WsFrame::ok_response("", json!({ "id": rec.id.as_str(), "decided": "approved", "side_effect": {
                "applied": true,
                "agent_id": applied.agent_id,
                "before": oc::entry_json(&applied.before),
                "after": oc::entry_json(&applied.after),
            } }))
        }
        other => journaled_status_mismatch(env, rec, &applied, other),
    }
}

/// A journaled apply whose card ended in another terminal state (e.g.
/// the TTL sweep expired it after the apply): say so instead of
/// pretending it was not applied.
fn journaled_status_mismatch(
    env: &DecisionEnv,
    rec: &crate::approval::ApprovalRecord,
    applied: &journal::Applied,
    other: &crate::approval::ApprovalStatus,
) -> WsFrame {
    audit(
        &env.home,
        "org_change_decision_mismatch",
        &applied.agent_id,
        duduclaw_security::audit::Severity::Warning,
        json!({
            "approval_id": rec.id.as_str(),
            "recorded_status": other.as_str(),
            "applied": true,
            "approved_by": applied.approved_by,
            "decided_by": applied.decided_by,
            "before": oc::entry_json(&applied.before),
            "after": oc::entry_json(&applied.after),
        }),
    );
    journal::clear(&env.home, rec.id.as_str());
    WsFrame::error_response(
        "",
        &format!(
            "這筆組織調整其實已經套用，但審核紀錄顯示為「{}」；實際結果已寫入稽核紀錄。需要還原的話，請在組織圖直接調整。",
            other.as_str()
        ),
    )
}

/// Deny a card that can no longer be applied (stale, invalid, malformed),
/// audit why, retire its channel notice, and answer with `message`. The
/// audit row and the collapse follow only a recorded denial.
#[allow(clippy::too_many_arguments)]
async fn deny_org_card(
    env: &DecisionEnv, actor: &DecisionActor,
    broker: &crate::approval::ApprovalBroker,
    rec: &crate::approval::ApprovalRecord,
    by: &Decider,
    proposal: Option<&Proposal>,
    why: &str,
    decision_reason: &str,
    message: &str,
    lease: Option<&crate::approval::DecisionLease>,
) -> WsFrame {
    if let Err(e) = record_decision(broker, &rec.id, false, actor, lease).await {
        return WsFrame::error_response(
            "",
            &format!(
                "{message}（但拒絕沒有記錄成功：{}；這筆審核仍待決定，請再試一次）",
                duduclaw_core::truncate_chars(&e, 300)
            ),
        );
    }
    audit_rejected(&env.home, rec, by, proposal, why, decision_reason);
    collapse_org_card(env, rec, false, by).await;
    WsFrame::error_response("", message)
}

async fn collapse_org_card(env: &DecisionEnv, rec: &crate::approval::ApprovalRecord, approve: bool, by: &Decider) {
    if matches!(by, Decider::ChatAdmin(_)) { return; }
    let name = super::identity::display_name(env, by.user_id()).await;
    crate::approval_notify::spawn_dashboard_collapse(env.home.clone(), rec.clone(), approve, name);
}

/// One recovery attempt for the card's "mirror pending" journal entry;
/// returns the entry as it stands afterwards (`mirror_synced` when the
/// mirror now follows the authority).
async fn try_finish_mirror(
    env: &DecisionEnv,
    rec: &crate::approval::ApprovalRecord,
    applied: journal::Applied,
) -> journal::Applied {
    let Some(registry) = env.registry.as_ref() else { return applied; };
    match finish_pending_mirror(&env.home, registry, rec.id.as_str()).await {
        Ok(true) => journal::Applied { mirror_synced: true, ..applied },
        Ok(false) => applied,
        Err(e) => {
            warn!(approval_id = %rec.id, error = %e, "org_change mirror recovery failed (retried later)");
            applied
        }
    }
}

/// Finish a "mirror pending" entry in the background (RM-027 round 5),
/// at [`oc::MIRROR_RECOVERY_DELAYS`]; stops as soon as it is settled.
fn spawn_mirror_recovery(env: &DecisionEnv, approval_id: &str) {
    let home = env.home.clone();
    let Some(registry) = env.registry.clone() else { return; };
    let id = approval_id.to_string();
    tokio::spawn(async move {
        for delay in oc::MIRROR_RECOVERY_DELAYS {
            tokio::time::sleep(delay).await;
            match finish_pending_mirror(&home, &registry, &id).await {
                Ok(true) => return,
                Ok(false) => {}
                Err(e) => warn!(approval_id = %id, error = %e, "org_change mirror recovery attempt failed"),
            }
        }
        warn!(
            approval_id = %id,
            "org_change mirror still pending after the background retries — it finishes on the next \
             approve click on the card or when an Admin re-applies the placement"
        );
    });
}

/// Commit one agent's new placement while holding the org lock: the
/// `org.toml` **authority first** (current Admin re-checked right
/// before), then the `agent.toml` mirror. `Err` = nothing changed. A
/// mirror that does not follow is recorded as drift and returned in
/// [`OrgCommit::mirror_error`] — the authority is the truth either way.
pub(crate) async fn commit_org_entry(
    home: &Path, registry: &Arc<RwLock<AgentRegistry>>,
    org_lock: &OrgMutationGuard,
    agent_id: &str,
    after: &OrgEntry,
    source: &'static str,
    by: &Decider,
) -> Result<OrgCommit, String> {
    require_dashboard_admin(home, by)
        .map_err(|_| "只有管理員可以套用組織調整".to_string())?;
    if let Err(e) = org_lock.upsert(agent_id, after.clone()) {
        audit(
            home,
            "org_change_authority_write_failed",
            agent_id,
            duduclaw_security::audit::Severity::Warning,
            json!({ "agent_id": agent_id, "after": oc::entry_json(after), "error": e.to_string() }),
        );
        return Err(e.to_string());
    }
    match sync_mirror(home, registry, agent_id, after, source, by).await {
        Ok(registry_reloaded) => Ok(OrgCommit { registry_reloaded, mirror_error: None }),
        Err(e) => {
            record_mirror_drift(home, agent_id, after, source, &e);
            Ok(OrgCommit { registry_reloaded: false, mirror_error: Some(e) })
        }
    }
}

/// Write the placement into the `agent.toml` display mirror through the
/// writer `agents.update` uses (current Admin re-checked inside the file
/// lock). Returns whether the registry re-scan succeeded. RM-027 round 5:
/// the `agent.toml` lock wait is bounded ([`oc::MIRROR_LOCK_WAIT`]) —
/// every caller holds the global org lock.
pub(crate) async fn sync_mirror(
    home: &Path, registry: &Arc<RwLock<AgentRegistry>>,
    agent_id: &str,
    after: &OrgEntry,
    source: &'static str,
    by: &Decider,
) -> Result<bool, String> {
    let guard_home = home.to_path_buf();
    let decider = by.clone();
    sync_mirror_with(home, registry, agent_id, after, source, by.user_id(), move || {
        require_dashboard_admin(&guard_home, &decider)
            .map_err(|_| "只有管理員可以套用組織調整".to_string())
    })
    .await
}

/// Explicit drift record: the authority holds `after`, the mirror did not
/// follow (`duduclaw doctor` / `org.placement` report it too).
pub(crate) fn record_mirror_drift(home: &Path, agent_id: &str, after: &OrgEntry, source: &str, error: &str) {
    let drift: Vec<Value> = org_store::detect_drift(home)
        .into_iter()
        .filter(|d| d.agent_id == agent_id)
        .map(|d| json!({ "store": oc::entry_json(&d.store), "mirror": oc::entry_json(&d.mirror) }))
        .collect();
    warn!(agent = %agent_id, error = %error, "org authority committed but the agent.toml mirror did not follow");
    audit(
        home,
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
/// Write `after` into `agent_id`'s `agent.toml` display mirror. The caller
/// holds the global org lock, so the `agent.toml` lock wait is bounded by
/// [`oc::mirror_lock_wait`] (RM-027 round 5): past it nothing is written and
/// the error carries `lock_wait_timeout` — the org lock is never pinned by a
/// stuck writer. `guard` is re-checked inside the file lock.
async fn sync_mirror_with<G>(
    home: &Path,
    registry: &Arc<RwLock<duduclaw_agent::registry::AgentRegistry>>,
    agent_id: &str,
    after: &OrgEntry,
    source: &'static str,
    user_id: &str,
    guard: G,
) -> Result<bool, String>
where
    G: FnOnce() -> Result<(), String>,
{
    let reports_to = after.reports_to.clone();
    let department = after.department.clone();
    let edit = crate::channel_reply::AgentTomlEdit { source, user_id, guard };
    crate::channel_reply::update_agent_toml_edit_bounded(registry, agent_id, edit, oc::mirror_lock_wait(home), move |table| {
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

/// RM-027 round 5 — the recovery of a "mirror pending" journal entry: the
/// authority committed an approved placement but the `agent.toml` mirror did
/// not follow (its bounded lock wait ran out, or the write was refused).
///
/// Under the global org lock: re-read the entry; when the authority still
/// holds `after` and the mirror differs, write the mirror (bounded again,
/// system guard — the mirror only follows what an authorized Admin's
/// approval already committed to the authority). A mirror that already
/// matches, or an authority that a later change moved on (that change owns
/// the mirror now), settles the entry too. Settled ⇒ the entry is removed
/// once the decision is recorded (`decision_recorded`), else kept with
/// `mirror_synced = true` for the decision recovery.
///
/// `Ok(true)` = settled (or nothing to do); `Ok(false)` = still pending.
pub(crate) async fn finish_pending_mirror(
    home: &Path,
    registry: &Arc<RwLock<duduclaw_agent::registry::AgentRegistry>>,
    approval_id: &str,
) -> Result<bool, String> {
    let org_lock = lock_org(home).await?;
    let Some(entry) = journal::read(home, approval_id)? else { return Ok(true) };
    if entry.state != journal::State::Applied {
        // Not committed (yet): the prepared-entry settlement owns it.
        return Ok(false);
    }
    let settle = |entry: journal::Applied| -> Result<bool, String> {
        if entry.decision_recorded {
            journal::clear(home, approval_id);
        } else {
            journal::record(home, &journal::Applied { mirror_synced: true, ..entry })?;
        }
        Ok(true)
    };
    if entry.mirror_synced {
        return settle(entry);
    }
    let store = org_lock.load().map_err(|e| e.to_string())?;
    if store.get(&entry.agent_id) != Some(&entry.after) {
        info!(
            approval_id,
            agent = %entry.agent_id,
            "org_change mirror recovery: the authority moved on since — the later change owns the mirror"
        );
        return settle(entry);
    }
    if mirror_of(home, &entry.agent_id).as_ref() == Some(&entry.after) {
        return settle(entry);
    }
    match sync_mirror_with(home, registry, &entry.agent_id, &entry.after, "org.mirror_recovery", "system", || Ok(())).await {
        Ok(reloaded) => {
            audit(
                home,
                "org_change_mirror_resynced",
                &entry.agent_id,
                duduclaw_security::audit::Severity::Info,
                json!({
                    "agent_id": entry.agent_id,
                    "placement": oc::entry_json(&entry.after),
                    "approval_id": approval_id,
                    "actor": "system",
                    "source": "org.mirror_recovery",
                    "registry_reloaded": reloaded,
                }),
            );
            let settled = settle(entry);
            drop(org_lock);
            settled
        }
        Err(e) => {
            warn!(approval_id, agent = %entry.agent_id, error = %e, "org_change mirror recovery: the mirror is still behind");
            Ok(false)
        }
    }
}
