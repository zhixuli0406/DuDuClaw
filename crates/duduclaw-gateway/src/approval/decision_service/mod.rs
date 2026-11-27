//! Single entry for RM-143 decisions. Callers supply authenticated identity,
//! never approval-payload identity. Dashboard callers retain their RPC gates.
//! `expected_digest` is inherited from EXPECTED_DECISION_DIGEST for dashboard
//! calls; chat always scopes its receipt digest, including denials.
mod bound;
pub(crate) mod files_org;
pub(crate) mod identity;
mod knowledge;
mod locks;
#[cfg(test)]
pub(crate) mod tests;

use std::{path::PathBuf, sync::Arc};
use duduclaw_agent::registry::AgentRegistry;
use duduclaw_auth::{UserContext, UserDb};
use serde_json::{Value, json};
use tokio::sync::RwLock;
use super::{ApprovalBroker, ApprovalId, ApprovalRecord, ApprovalStatus};
use super::chat_admin::{self, ChatChannel, DecisionActor, ledger::Step};
use crate::trigger_route::texts::Lang;

pub(crate) use locks::decision_lock;
pub(crate) use knowledge::AUDIT_KNOWLEDGE_REVIEW_DECISION_UNRECORDED;
#[cfg(test)]
pub(crate) use locks::force_decide_failure;

#[derive(Clone)]
pub(crate) enum Decider {
    Dashboard(UserContext),
    ChatAdmin(ChatAdminProof),
}

/// Only verified transport/step-up code may construct this proof. No caller
/// supplied `step_required`: the service derives it from live configuration.
#[derive(Clone)]
pub(crate) struct ChatAdminProof {
    pub(crate) user_id: String,
    pub(crate) channel: ChatChannel,
    pub(crate) account: String,
    pub(crate) principal: String,
    pub(crate) dm: String,
    pub(crate) card_id: String,
    pub(crate) step: Step,
    pub(crate) digest: String,
}

#[derive(Clone)]
pub(crate) struct DecisionEnv {
    pub(crate) home: PathBuf,
    pub(crate) registry: Option<Arc<RwLock<AgentRegistry>>>,
    pub(crate) broker: ApprovalBroker,
    pub(crate) users: Option<Arc<UserDb>>,
}

#[derive(Debug)]
pub(crate) struct DecisionOutcome {
    pub(crate) value: Value,
    pub(crate) status: ApprovalStatus,
    pub(crate) decided_by: String,
    pub(crate) display_name: Option<String>,
    pub(crate) channel: Option<ChatChannel>,
}

#[derive(Debug)]
pub(crate) enum DecisionRefusal {
    NotFound,
    Expired(String),
    AlreadyDecided { id: ApprovalId, by: Option<String>, status: ApprovalStatus },
    Forbidden(String),
    DigestMismatch,
    NotSupportedInChat,
    FeatureOff,
    StepUpRequired,
    Internal(String),
    /// Preserve the existing structured org RPC error (code/message/effect).
    Kind(Value),
}

impl DecisionRefusal {
    pub(crate) fn dashboard_text(&self) -> String {
        match self {
            Self::Kind(error) => error.get("message").and_then(Value::as_str).unwrap_or("決定失敗").into(),
            Self::NotFound => "找不到這筆審核".into(),
            Self::Expired(text) | Self::Forbidden(text) | Self::Internal(text) => text.clone(),
            Self::AlreadyDecided { id, status, .. } => format!("approval {id} already {}", status.as_str()),
            Self::DigestMismatch => "approval digest mismatch; refetch the approval before deciding".into(),
            Self::NotSupportedInChat => "請到儀表板處理".into(),
            Self::FeatureOff => "聊天核准已關閉，請到儀表板處理".into(),
            Self::StepUpRequired => "需要再確認一次".into(),
        }
    }

    /// Callers select the language with trigger_route::texts::lang_for(home).
    /// Never expose internal errors, file paths or raw identity in chat.
    pub(crate) fn user_text(&self, lang: Lang) -> String {
        let words = match self {
            Self::NotFound => ["找不到這筆審核，請到儀表板確認。", "Approval not found. Please check the dashboard.", "審査が見つかりません。ダッシュボードで確認してください。"],
            Self::Expired(_) => ["這筆審核已逾期，請到儀表板確認。", "This approval has expired. Please check the dashboard.", "この審査は期限切れです。ダッシュボードで確認してください。"],
            Self::AlreadyDecided { .. } => ["這筆審核已有決定，請到儀表板查看結果。", "This approval has already been decided. Check the dashboard for the result.", "この審査は決定済みです。ダッシュボードで結果を確認してください。"],
            Self::Forbidden(_) => ["目前無法確認你的核准權限，請到儀表板處理。", "Your approval permission could not be confirmed. Please use the dashboard.", "承認権限を確認できません。ダッシュボードで処理してください。"],
            Self::DigestMismatch => ["內容已更新，請到儀表板重新確認。", "The content has changed. Please review it on the dashboard.", "内容が更新されました。ダッシュボードで再確認してください。"],
            Self::NotSupportedInChat => ["請到儀表板處理", "Please use the dashboard.", "ダッシュボードで処理してください。"],
            Self::FeatureOff => ["聊天核准已關閉，請到儀表板處理", "Chat approvals are disabled. Please use the dashboard.", "チャットでの承認は無効です。ダッシュボードで処理してください。"],
            Self::StepUpRequired => ["需要再確認一次", "One more confirmation is needed.", "追加の確認が必要です。"],
            Self::Internal(_) | Self::Kind(_) => ["目前無法完成，請到儀表板確認結果後再試。", "Unable to complete this now. Check the dashboard before trying again.", "処理を完了できません。ダッシュボードで結果を確認してから再試行してください。"],
        };
        words[match lang { Lang::ZhTw => 0, Lang::En => 1, Lang::Ja => 2 }].into()
    }

    pub(crate) fn dashboard_frame(&self) -> crate::protocol::WsFrame {
        match self {
            Self::Kind(error) => crate::protocol::WsFrame::Response {
                id: String::new(), ok: false, payload: None, error: Some(error.clone()),
            },
            _ => crate::protocol::WsFrame::error_response("", &self.dashboard_text()),
        }
    }

    fn from_kind_error(error: String) -> Self {
        match error.as_str() {
            "approval digest mismatch; refetch the approval before deciding" => Self::DigestMismatch,
            crate::on_behalf_of::self_approval::REFUSAL
                | files_org::SELF_APPROVAL_REFUSAL
                | crate::knowledge::add_source_approval::NOT_EDITOR => Self::Forbidden(error),
            _ => Self::Internal(error),
        }
    }

    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::NotFound => "not_found", Self::Expired(_) => "expired",
            Self::AlreadyDecided { .. } => "already_decided", Self::Forbidden(_) => "forbidden",
            Self::DigestMismatch => "digest_mismatch", Self::NotSupportedInChat => "not_supported",
            Self::FeatureOff => "feature_off", Self::StepUpRequired => "step_up_required",
            Self::Internal(_) | Self::Kind(_) => "internal",
        }
    }
}

impl Decider {
    pub(crate) fn user_id(&self) -> &str {
        match self { Self::Dashboard(ctx) => &ctx.user_id, Self::ChatAdmin(proof) => &proof.user_id }
    }
}

/// Unbound kinds whose side effects are owned by this service.
pub(crate) fn handles(rec: &ApprovalRecord) -> bool {
    if rec.binding.is_some() { return false; }
    match rec.action_kind.as_str() {
        crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE
        | crate::knowledge::add_source_approval::ACTION_KIND
        | crate::expert_skills::SKILLS_ACTION_KIND
        | crate::autopilot_approval::ACTION_KIND
        | super::cli_egress::KIND | "db_source_grant"
        | crate::agent_files::approval::ACTION_KIND | crate::org_change_approval::ACTION_KIND => true,
        super::WORKFLOW_ACTIVATION_KIND | "discovery"
        | crate::custom_skills::ACTION_KIND_SKILL_CREATE
        | crate::memory_forget_approval::ACTION_KIND_MEMORY_FORGET_SOURCE => false,
        _ => rec.payload.get("egress_guard").is_some()
            && chat_admin::tier(rec) != chat_admin::Tier::T3,
    }
}

/// Read-only identity preflight for the press flow before retiring/resending a
/// card. Reuse the exact service authority; decide rechecks under its lock.
/// Step-up is not issued here: the receipt digest must be checked first.
pub(crate) async fn validate_chat_identity(
    env: &DecisionEnv, rec: &ApprovalRecord, approve: bool, proof: &ChatAdminProof,
) -> Result<(), DecisionRefusal> {
    match identity::authorize(env, rec, approve, proof).await {
        Ok(_) | Err(DecisionRefusal::StepUpRequired) => Ok(()),
        Err(error) => {
            identity::audit_refusal(env, &rec.id, proof, &error).await;
            Err(error)
        }
    }
}

pub(crate) async fn decide(
    env: &DecisionEnv, id: &ApprovalId, approve: bool, reason: &str, by: &Decider,
) -> Result<DecisionOutcome, DecisionRefusal> {
    decide_with_reach(env, id, approve, reason, by, None).await
}

pub(crate) async fn decide_with_reach(
    env: &DecisionEnv, id: &ApprovalId, approve: bool, reason: &str, by: &Decider,
    reach_digest: Option<&str>,
) -> Result<DecisionOutcome, DecisionRefusal> {
    let expected = match by {
        Decider::Dashboard(_) => super::EXPECTED_DECISION_DIGEST.try_with(Clone::clone).ok().flatten(),
        Decider::ChatAdmin(proof) => Some((id.to_string(), proof.digest.clone())),
    };
    let lock = decision_lock(id.as_str());
    let _guard = lock.lock().await;
    let result = super::EXPECTED_DECISION_DIGEST.scope(expected,
        super::EXPECTED_DECISION_HOME.scope(env.home.clone(),
            Box::pin(decide_locked(env, id, approve, reason, by, reach_digest))),
    ).await;
    if let (Decider::ChatAdmin(proof), Err(error)) = (by, &result) {
        identity::audit_refusal(env, id, proof, error).await;
    }
    result
}

async fn decide_locked(
    env: &DecisionEnv, id: &ApprovalId, approve: bool, reason: &str, by: &Decider,
    reach_digest: Option<&str>,
) -> Result<DecisionOutcome, DecisionRefusal> {
    let rec = env.broker.get(id).await
        .map_err(|e| DecisionRefusal::Internal(format!("讀取審核失敗：{e}")))?
        .ok_or(DecisionRefusal::NotFound)?;
    if rec.action_kind == crate::admin_ai::card::ACTION_KIND {
        let (home, record, decider) = (env.home.clone(), rec.clone(), by.clone());
        let allowed = tokio::task::spawn_blocking(move || match &decider {
            Decider::Dashboard(ctx) => crate::admin_ai::may_decide(&home, &record, &ctx.user_id),
            Decider::ChatAdmin(proof) => crate::admin_ai::may_decide_chat(&home, &record, proof),
        }).await.map_err(|e| DecisionRefusal::Internal(e.to_string()))?;
        if !allowed { return Err(DecisionRefusal::Forbidden(crate::admin_ai::REFUSAL.into())); }
    }
    if rec.action_kind == "db_source_grant" {
        let Decider::Dashboard(ctx) = by else { return Err(DecisionRefusal::NotSupportedInChat); };
        super::operator_cli_gate::require_admin_decider(&env.home, ctx)
            .map_err(|_| DecisionRefusal::Forbidden("資料庫授權只能由管理員在儀表板決定。".into()))?;
    }
    if crate::google_identity::member_card::is_member_card(&rec)
        && (matches!(by, Decider::ChatAdmin(_))
            || !crate::google_identity::member_card::may_decide(&env.home, &rec, by.user_id())) {
        return Err(DecisionRefusal::Forbidden("Only the requesting member may confirm".into()));
    }
    let actor = match by {
        Decider::Dashboard(ctx) => DecisionActor::Dashboard { user_id: ctx.user_id.clone() },
        Decider::ChatAdmin(proof) => {
            if rec.request_kind != super::RequestKind::Approval {
                return Err(DecisionRefusal::NotSupportedInChat);
            }
            let entry = identity::authorize(env, &rec, approve, proof).await?;
            if rec.binding.is_none() && !handles(&rec) {
                return Err(DecisionRefusal::NotSupportedInChat);
            }
            DecisionActor::ChatAdmin(entry)
        }
    };
    if rec.action_kind == crate::autopilot_approval::ACTION_KIND {
        if let Decider::Dashboard(ctx) = by {
            let (home, ctx) = (env.home.clone(), ctx.clone());
            tokio::task::spawn_blocking(move || super::operator_cli_gate::require_admin_decider(&home, &ctx))
                .await.map_err(|e| DecisionRefusal::Internal(e.to_string()))?
                .map_err(|_| DecisionRefusal::Forbidden(crate::autopilot_approval::ADMIN_REFUSAL.into()))?;
        }
        if approve && !super::EXPECTED_DECISION_DIGEST.try_with(|expected| {
            expected.as_ref().is_some_and(|(expected_id, _)| expected_id == id.as_str())
        }).unwrap_or(false) {
            return Err(DecisionRefusal::Forbidden(crate::autopilot_approval::DIGEST_REQUIRED.into()));
        }
    }
    // Knowledge retains its original status/expiry/authorization ordering and
    // sidecar cleanup. Other kinds originally checked expiry before status.
    let value_result: Result<Value, DecisionRefusal> = async { Ok(match rec.action_kind.as_str() {
        _ if rec.binding.is_some() && (matches!(&actor, DecisionActor::ChatAdmin(_))
            || rec.action_kind == crate::admin_ai::card::ACTION_KIND) => {
            chat_preflight(env, &rec, approve, by).await?;
            bound::decide(env, &rec, approve, by, &actor).await?
        }
        crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE => {
            check_state(env, &rec, true).await?;
            check_digest(&rec)?;
            chat_preflight(env, &rec, approve, by).await?;
            knowledge::quarantine(env, id, approve, by, &actor).await.map_err(DecisionRefusal::from_kind_error)?
        }
        crate::knowledge::add_source_approval::ACTION_KIND => {
            let user = identity::knowledge_user(env, by).await?;
            // Preserve collection authority before status/expiry/digest errors.
            let (home, record, user_copy) = (env.home.clone(), rec.clone(), user.clone());
            crate::knowledge::blocking(move || crate::knowledge::add_source_approval::may_decide(&home, &record, &user_copy))
                .await.map_err(|_| DecisionRefusal::Forbidden(crate::knowledge::add_source_approval::NOT_EDITOR.into()))?;
            check_state(env, &rec, true).await?;
            if matches!(by, Decider::ChatAdmin(_)) {
                check_digest(&rec)?;
                chat_preflight(env, &rec, approve, by).await?;
            }
            crate::knowledge::add_source_approval::decide_as_locked(env, id, approve, by, &user, &actor)
                .await.map_err(DecisionRefusal::from_kind_error)?
        }
        crate::agent_files::approval::ACTION_KIND if rec.binding.is_none() => {
            if matches!(by, Decider::ChatAdmin(_)) {
                check_digest(&rec)?;
                chat_preflight(env, &rec, approve, by).await?;
            }
            files_org::agent_file_change(env, id, approve, by, &actor).await?
        }
        crate::org_change_approval::ACTION_KIND if rec.binding.is_none() => {
            if matches!(by, Decider::ChatAdmin(_)) {
                check_digest(&rec)?;
                if let Err(error) = chat_preflight(env, &rec, approve, by).await {
                    files_org::audit_recovery_refusal(env, &rec, &error);
                    return Err(error);
                }
            }
            // The kind owns state/expiry ordering: an applied journal must
            // be recovered before treating its original base as stale.
            files_org::org_change(env, id, approve, reason, by, &actor, reach_digest).await?
        }
        _ if handles(&rec) => {
            check_state(env, &rec, false).await?;
            check_digest(&rec)?;
            chat_preflight(env, &rec, approve, by).await?;
            general(env, &rec, approve, by, &actor).await?
        }
        _ => return Err(DecisionRefusal::NotSupportedInChat),
    }) }.await;
    let status = if approve { ApprovalStatus::Approved } else { ApprovalStatus::Denied };
    let display_name = identity::display_name(env, by.user_id()).await;
    let (decided_by, channel) = match &actor {
        DecisionActor::Dashboard { user_id } => {
            // General kinds collapse immediately after recording (before
            // capability minting), just as the original dashboard handler did.
            if value_result.is_ok() && rec.action_kind == crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE {
                crate::approval_notify::spawn_dashboard_collapse(env.home.clone(), rec.clone(), approve, display_name.clone());
            }
            (format!("dashboard:{user_id}"), None)
        }
        DecisionActor::ChatAdmin(entry) => {
            let decided_by = entry.decided_by().to_string();
            // A hash mismatch can durably deny an add-source card while
            // returning an error. Audit that actual denial too. A failed
            // post-decision capability mint must not hide a durable decision.
            if rec.status == ApprovalStatus::Pending {
                // Consumers may already have changed Approved to Invalidated.
                // The immutable ledger, not that later status, records what
                // this card actually decided (including hash-mismatch denial).
                if let Some((approved, step_required)) = recorded_chat_decision(env, id, entry).await? {
                    if rec.binding.is_none() && !is_general(&rec) {
                        crate::approval_notify::spawn_dashboard_collapse(env.home.clone(), rec.clone(), approved, display_name.clone());
                    }
                    let mut recorded = entry.clone();
                    recorded.step_required = step_required;
                    identity::audit_decision(env, &rec, approved, by, &recorded).await?;
                }
            }
            (decided_by, Some(entry.channel))
        }
    };
    let value = value_result?;
    // An org journal may finish a different Admin's earlier decision. Return
    // the durable identity, not the identity of the person who retried it.
    if rec.action_kind == crate::org_change_approval::ACTION_KIND {
        let saved = env.broker.get(id).await.map_err(DecisionRefusal::Internal)?
            .ok_or(DecisionRefusal::NotFound)?;
        let decided_by = saved.decided_by.ok_or_else(|| DecisionRefusal::Internal("missing recorded decider".into()))?;
        let (user_id, channel) = if let Ok(chat) = decided_by.parse::<chat_admin::ledger::DecidedBy>() {
            (chat.user_id().to_owned(), Some(chat.channel()))
        } else if let Some(user) = decided_by.strip_prefix("dashboard:") {
            // The broker/journal constructs this exact dashboard format.
            (user.to_owned(), None)
        } else { return Err(DecisionRefusal::Internal("invalid recorded decider".into())); };
        let display_name = identity::display_name(env, &user_id).await;
        return Ok(DecisionOutcome { value, status: saved.status, decided_by, display_name, channel });
    }
    Ok(DecisionOutcome { value, status, decided_by, display_name, channel })
}

async fn check_state(env: &DecisionEnv, rec: &ApprovalRecord, status_first: bool) -> Result<(), DecisionRefusal> {
    let already = || DecisionRefusal::AlreadyDecided { id: rec.id.clone(), by: rec.decided_by.clone(), status: rec.status };
    if status_first && rec.status != ApprovalStatus::Pending { return Err(already()); }
    if rec.is_stale(chrono::Utc::now()) {
        let _ = env.broker.poll(&rec.id).await;
        if rec.action_kind == crate::knowledge::add_source_approval::ACTION_KIND {
            crate::knowledge::add_source_approval::remove_sidecar(&env.home, &rec.payload);
        }
        let text = if crate::approval_notify::is_dashboard_only_kind(&rec.action_kind) {
            crate::approval_notify::dashboard_only_expired_text(&rec.action_kind)
        } else { "approval expired" };
        return Err(DecisionRefusal::Expired(text.into()));
    }
    if rec.status != ApprovalStatus::Pending { return Err(already()); }
    Ok(())
}

fn check_digest(rec: &ApprovalRecord) -> Result<(), DecisionRefusal> {
    super::check_expected_digest(rec).map_err(|_| DecisionRefusal::DigestMismatch)
}

async fn chat_preflight(env: &DecisionEnv, rec: &ApprovalRecord, approve: bool, by: &Decider) -> Result<(), DecisionRefusal> {
    let Decider::ChatAdmin(proof) = by else { return Ok(()); };
    let me = duduclaw_core::HumanPrincipal::user(&proof.user_id).map(|p| p.to_string());
    let (home, on_behalf_of) = (env.home.clone(), rec.on_behalf_of.clone());
    let refuses = tokio::task::spawn_blocking(move || crate::on_behalf_of::self_approval::refuses(
        &home, on_behalf_of.as_deref(), me.as_deref(), approve,
    )).await.map_err(|e| DecisionRefusal::Internal(e.to_string()))?;
    if refuses && rec.action_kind != crate::admin_ai::card::ACTION_KIND {
        return Err(DecisionRefusal::Forbidden(crate::on_behalf_of::self_approval::REFUSAL.into()));
    }
    // Validate the receipt BEFORE a side effect, not only when committing the
    // ledger. The transaction still consumes/revalidates it after the effect.
    let receipts = chat_admin::receipts::list_live(&env.broker.store, &rec.id).await
        .map_err(DecisionRefusal::Internal)?;
    let receipt = receipts.iter().find(|r| r.card_id == proof.card_id)
        .ok_or_else(|| DecisionRefusal::Forbidden("請到儀表板處理".into()))?;
    if receipt.purpose != "rm143" || receipt.user_id != proof.user_id
        || receipt.channel != proof.channel.as_str() || receipt.account != proof.account
        || receipt.principal != proof.principal || receipt.dm != proof.dm {
        return Err(DecisionRefusal::Forbidden("請到儀表板處理".into()));
    }
    if receipt.digest != proof.digest { return Err(DecisionRefusal::DigestMismatch); }
    if !chrono::DateTime::parse_from_rfc3339(&receipt.valid_until)
        .is_ok_and(|until| until > chrono::Utc::now()) {
        return Err(DecisionRefusal::Expired("這張卡已失效，請到儀表板處理".into()));
    }
    Ok(())
}

async fn general(env: &DecisionEnv, rec: &ApprovalRecord, approve: bool, by: &Decider, actor: &DecisionActor) -> Result<Value, DecisionRefusal> {
    if rec.payload["drive_write_admin_confirmation"] == true {
        let principal = duduclaw_core::HumanPrincipal::user(by.user_id()).map(|p| p.to_string());
        if crate::on_behalf_of::self_approval::refuses(&env.home, rec.on_behalf_of.as_deref(), principal.as_deref(), approve) {
            return Err(DecisionRefusal::Forbidden(crate::on_behalf_of::self_approval::REFUSAL.into()));
        }
    }
    let decision = match by {
        Decider::Dashboard(ctx) if rec.payload.get("egress_guard").is_some() || rec.action_kind == "db_source_grant" =>
            env.broker.decide_egress_dashboard(&rec.id, ctx, approve).await,
        _ => env.broker.decide_as(&rec.id, approve, actor).await,
    };
    decision.map_err(|e| DecisionRefusal::Internal(format!("decide: {e}")))?;
    let name = identity::display_name(env, by.user_id()).await;
    crate::approval_notify::spawn_dashboard_collapse(env.home.clone(), rec.clone(), approve, name);
    let mut capability_handle = Value::Null;
    if approve && let Some(epoch) = rec.payload.get("scope_epoch").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        let caps = crate::capability::CapabilityBroker::open(&env.home)
            .map_err(|e| DecisionRefusal::Internal(format!("開啟授權憑證儲存失敗：{e}")))?;
        let ttl = rec.payload.get("capability_ttl_seconds").and_then(Value::as_i64).unwrap_or(rec.ttl_seconds);
        let grant = caps.grant_from_approval(&env.broker, &rec.id, epoch, ttl, None).await
            .map_err(|e| DecisionRefusal::Internal(format!("已核准但核發授權憑證失敗（保守拒絕）：{e}")))?;
        capability_handle = json!(grant.handle_id);
    }
    Ok(json!({"id":rec.id.as_str(), "decided":if approve {"approved"} else {"denied"},
        "side_effect": Value::Null, "capability_handle":capability_handle}))
}

fn is_general(rec: &ApprovalRecord) -> bool {
    rec.action_kind != crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE
        && rec.action_kind != crate::knowledge::add_source_approval::ACTION_KIND
        && rec.action_kind != crate::agent_files::approval::ACTION_KIND
        && rec.action_kind != crate::org_change_approval::ACTION_KIND
}

async fn recorded_chat_decision(
    env: &DecisionEnv, id: &ApprovalId, entry: &chat_admin::LedgerEntry,
) -> Result<Option<(bool, bool)>, DecisionRefusal> {
    use rusqlite::OptionalExtension;
    let conn = env.broker.store.conn.lock().await;
    super::verify_live_io().map_err(DecisionRefusal::Internal)?;
    conn.query_row(
        "SELECT approve, step_required FROM chat_admin_decisions WHERE approval_id=?1 AND card_id=?2
         AND user_id=?3 AND channel=?4 AND step=?5 AND digest=?6",
        rusqlite::params![id.as_str(), entry.card_id, entry.user_id.to_string(),
            entry.channel.as_str(), entry.step.as_str(), entry.digest],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional().map_err(|e| DecisionRefusal::Internal(e.to_string()))
}
