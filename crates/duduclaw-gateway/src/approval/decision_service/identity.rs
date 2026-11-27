//! Blocking live identity/configuration reads; no database or registry guards
//! survive an await. No solo-mode or payload-identity fallback exists here.
use super::*;
use super::chat_admin::LedgerEntry;
use duduclaw_auth::UserRole;
use duduclaw_security::audit::{AuditEvent, Severity};

fn forbidden() -> DecisionRefusal { DecisionRefusal::Forbidden("請到儀表板處理".into()) }

fn requires_step_up(approve: bool, tier: chat_admin::Tier, key: Option<&str>, exempt: &[String]) -> bool {
    approve && tier == chat_admin::Tier::T2
        && key.is_none_or(|key| !exempt.iter().any(|kind| kind == key))
}

pub(super) async fn authorize(env: &DecisionEnv, rec: &ApprovalRecord, approve: bool, proof: &ChatAdminProof) -> Result<LedgerEntry, DecisionRefusal> {
    let agents = match &env.registry {
        Some(registry) => Some(registry.read().await.list().iter().map(|a| a.config.agent.name.clone()).collect::<Vec<_>>()),
        None => None,
    };
    let (home, users, rec, proof) = (env.home.clone(), env.users.clone(), rec.clone(), proof.clone());
    tokio::task::spawn_blocking(move || {
        if !matches!(chat_admin::chat_tier(Some(&home), &rec), chat_admin::Tier::T1 | chat_admin::Tier::T2) {
            return Err(DecisionRefusal::NotSupportedInChat);
        }
        // Even outside native protected mode, a known Personal employee's
        // card must not acquire a new chat-admin decision path.
        if let Some(employee) = crate::personal_decision::card_employee(&home, &rec).map_err(|_| forbidden())? {
            if crate::personal_visibility::personal_owner(&home, &employee).map_err(|_| forbidden())?.is_some() {
                return Err(DecisionRefusal::NotSupportedInChat);
            }
        }
        let cfg = chat_admin::config::effective(&home, users.as_deref()).ok_or(DecisionRefusal::FeatureOff)?;
        if !cfg.channels.contains(&proof.channel) { return Err(DecisionRefusal::FeatureOff); }
        if approve && proof.step == Step::Web && !cfg.step_up_methods.contains(&chat_admin::StepMethod::Web) {
            return Err(DecisionRefusal::StepUpRequired);
        }
        if proof.step == Step::Totp && (!cfg.step_up_methods.contains(&chat_admin::StepMethod::DmTotp)
            || proof.channel != chat_admin::ChatChannel::Telegram) {
            return Err(DecisionRefusal::StepUpRequired);
        }
        let user_id = uuid::Uuid::parse_str(&proof.user_id).map_err(|_| forbidden())?;
        // Ledger UUID serialization must not silently alter the identity key.
        if user_id.to_string() != proof.user_id { return Err(forbidden()); }
        let db = users.as_ref().ok_or_else(forbidden)?;
        let live = db.live_user_context(&proof.user_id).map_err(|_| forbidden())?.ok_or_else(forbidden)?;
        if live.role != UserRole::Admin || live.must_change_password { return Err(forbidden()); }
        let saved = db.approval_proof(&proof.user_id, proof.channel.as_str())
            .map_err(|_| forbidden())?.ok_or_else(forbidden)?;
        if saved.channel_user_id != proof.principal || saved.proof_account != proof.account || saved.proof_dm != proof.dm {
            return Err(forbidden());
        }
        // Without a live registry enumerate configured employee directories.
        // Missing agents/ is an empty installation; any other read error denies.
        let agents = match agents {
            Some(agents) => agents,
            None => match std::fs::read_dir(home.join("agents")) {
                Ok(entries) => {
                    let mut ids = Vec::new();
                    for entry in entries {
                        let entry = entry.map_err(|_| forbidden())?;
                        if entry.file_type().map_err(|_| forbidden())?.is_dir() {
                            ids.push(entry.file_name().into_string().map_err(|_| forbidden())?);
                        }
                    }
                    ids
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(_) => return Err(forbidden()),
            },
        };
        let accounts = crate::chat_link_intercept::configured_accounts(&home, proof.channel.as_str(), &agents)
            .map_err(|_| forbidden())?;
        if !accounts.iter().any(|account| account.account == proof.account) { return Err(forbidden()); }
        let step_required = requires_step_up(approve, chat_admin::tier(&rec),
            chat_admin::kind_key(&rec), &cfg.step_up_exempt_kinds);
        if step_required && proof.step == Step::Tap { return Err(DecisionRefusal::StepUpRequired); }
        Ok(LedgerEntry { user_id, channel: proof.channel, step: proof.step, step_required,
            card_id: proof.card_id, digest: proof.digest })
    }).await.map_err(|e| DecisionRefusal::Internal(e.to_string()))?
}

pub(super) async fn knowledge_user(env: &DecisionEnv, by: &Decider) -> Result<duduclaw_knowledge::requester::ResolvedUser, DecisionRefusal> {
    let (home, users, by) = (env.home.clone(), env.users.clone(), by.clone());
    tokio::task::spawn_blocking(move || {
        let live = match &by {
            Decider::Dashboard(ctx) => crate::review_evidence::audience::fresh_dashboard_context(&home, ctx)
                .map_err(|_| DecisionRefusal::Forbidden("permission denied".into()))?,
            Decider::ChatAdmin(proof) => {
                let live = users.as_ref().ok_or_else(forbidden)?.live_user_context(&proof.user_id)
                    .map_err(|_| forbidden())?.ok_or_else(forbidden)?;
                if live.role != UserRole::Admin || live.must_change_password { return Err(forbidden()); }
                live
            }
        };
        Ok(crate::knowledge::dashboard_user(&live, users.as_ref()))
    }).await.map_err(|e| DecisionRefusal::Internal(e.to_string()))?
}

pub(crate) async fn display_name(env: &DecisionEnv, user: &str) -> Option<String> {
    let (db, user) = (env.users.clone()?, user.to_owned());
    tokio::task::spawn_blocking(move || db.get_user(&user).ok().flatten()
        .map(|u| u.display_name.trim().to_owned()).filter(|name| !name.is_empty()))
        .await.ok().flatten()
}

pub(super) async fn audit_decision(env: &DecisionEnv, rec: &ApprovalRecord, approve: bool, by: &Decider, entry: &LedgerEntry) -> Result<(), DecisionRefusal> {
    let Decider::ChatAdmin(proof) = by else { return Ok(()); };
    let details = json!({"approval_id":rec.id.as_str(), "kind":rec.action_kind,
        "user_id":proof.user_id, "channel":proof.channel.as_str(), "account":proof.account,
        "principal":duduclaw_auth::otp::mask_target(&proof.principal),
        "step":entry.step.as_str(), "step_required":entry.step_required,
        "approve":approve, "at":chrono::Utc::now().to_rfc3339()});
    let (home, db, user, agent) = (env.home.clone(), env.users.clone(), proof.user_id.clone(), rec.agent_id.clone());
    tokio::task::spawn_blocking(move || {
        let event = AuditEvent::new("approval_chat_admin_decided", &agent, Severity::Info, details.clone());
        let security = duduclaw_security::audit::try_append_audit_event(&home, &event);
        let activity = db.as_ref().ok_or_else(|| "users database unavailable".to_string())
            .and_then(|db| db.log_action(Some(&user), "approval.chat_admin_decision", Some(&user), Some(&details.to_string()), None));
        if security.is_err() || activity.is_err() {
            return Err(DecisionRefusal::Internal("決定已記錄，但稽核紀錄未能完整儲存，請到儀表板確認。".into()));
        }
        Ok(())
    }).await.map_err(|e| DecisionRefusal::Internal(e.to_string()))?
}

fn admit_refusal(channel: &str, account: &str, principal: &str) -> bool {
    use std::{collections::HashMap, sync::{Mutex, OnceLock}, time::{Duration, Instant}};
    type Sender = (String, String, String);
    static LIMIT: OnceLock<Mutex<HashMap<Sender, (Instant, u8)>>> = OnceLock::new();
    let now = Instant::now();
    let mut limits = LIMIT.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner());
    limits.retain(|_, (at, _)| now.duration_since(*at) < Duration::from_secs(60));
    let (_, count) = limits.entry((channel.into(), account.into(), principal.into())).or_insert((now, 0));
    if *count >= 5 { return false; }
    *count += 1;
    true
}

pub(super) async fn audit_refusal(env: &DecisionEnv, id: &ApprovalId, proof: &ChatAdminProof, error: &DecisionRefusal) {
    audit_refusal_reason(&env.home, id, proof.channel.as_str(), &proof.account,
        &proof.principal, error.code()).await;
}

/// Shared rate limit for service and pre-service transport refusals. Identity
/// here is for masked auditing only; it is never an authorization proof.
pub(crate) async fn audit_refusal_reason(
    home: &std::path::Path, id: &ApprovalId, channel: &str, account: &str,
    principal: &str, reason: &'static str,
) {
    if !admit_refusal(channel, account, principal) { return; }
    let details = json!({"reason":reason, "approval_id":id.as_str(),
        "channel":channel, "principal":duduclaw_auth::otp::mask_target(principal)});
    let home = home.to_path_buf();
    let result = tokio::task::spawn_blocking(move || duduclaw_security::audit::try_append_audit_event(
        &home, &AuditEvent::new("approval_chat_admin_refused", "chat_admin", Severity::Warning, details),
    )).await;
    if !matches!(result, Ok(Ok(()))) { tracing::warn!("chat-admin refusal audit could not be written"); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_up_defaults_to_all_t2_and_unknown_t2_keys_fail_closed() {
        for key in chat_admin::STEP_UP_KIND_KEYS {
            assert!(requires_step_up(true, chat_admin::Tier::T2, Some(key), &[]));
            assert!(!requires_step_up(false, chat_admin::Tier::T2, Some(key), &[]));
            let exempt = vec![(*key).to_owned()];
            assert!(!requires_step_up(true, chat_admin::Tier::T2, Some(key), &exempt));
            assert!(requires_step_up(true, chat_admin::Tier::T2, Some("different"), &exempt));
            assert!(requires_step_up(true, chat_admin::Tier::T2, None, &exempt));
        }
        assert!(!requires_step_up(true, chat_admin::Tier::T1, None, &[]));
        assert!(!requires_step_up(true, chat_admin::Tier::T1, Some("knowledge_quarantine"), &[]));
    }

    #[test]
    fn refusal_audit_is_limited_to_five_per_sender_per_minute() {
        let sender = uuid::Uuid::new_v4().to_string();
        for _ in 0..5 { assert!(admit_refusal("telegram", "fixture", &sender)); }
        assert!(!admit_refusal("telegram", "fixture", &sender));
        assert!(admit_refusal("telegram", "other-account", &sender));
    }
}
