//! Cosmetic retirement of delivered RM-143 cards. Never grants a decision.
use std::{collections::HashSet, path::{Path, PathBuf}, time::Duration};

use chrono::{DateTime, Utc};
use duduclaw_auth::otp::ChatApprovalProof;

use crate::{approval::{ApprovalBroker, ApprovalId, ApprovalRecord, ApprovalStatus, ApprovalStore},
    decision_card::PushedMessage, trigger_route::texts::Lang};
use super::{card, config, deliver::{Sender, Target, transport::Network}, ledger, press, receipts::{self, Receipt, TerminalStatus}};

async fn open_store(home: &Path) -> Option<ApprovalStore> {
    let home = home.to_path_buf();
    match tokio::task::spawn_blocking(move || ApprovalStore::open(&home)).await {
        Ok(Ok(store)) => Some(store),
        _ => { tracing::warn!("chat-admin card sync store unavailable"); None }
    }
}

/// The caller may still hold the pre-decision snapshot. Re-read the durable
/// decision, including its timestamp and actor, before rendering any result.
pub(crate) async fn collapse_after_decision(
    home: &Path, rec: &ApprovalRecord, approve: bool, decider_name: Option<String>,
) {
    let Some(store) = open_store(home).await else { return; };
    let current = match store.get(&rec.id).await {
        Ok(Some(rec)) => rec,
        _ => { tracing::warn!("chat-admin card sync decision unavailable"); return; }
    };
    let lang = press::language(home).await;
    if matches!(current.status, ApprovalStatus::Expired | ApprovalStatus::Invalidated) {
        expire_with_sender(&store, &current, lang, &Network::new(home)).await;
        return;
    }
    if !matches!(current.status, ApprovalStatus::Approved | ApprovalStatus::Denied) { return; }
    let label = decision_channel_label(&current, lang);
    let name = recorded_name(home, &current).await.or(decider_name);
    let approve = match current.status {
        ApprovalStatus::Approved => true, ApprovalStatus::Denied => false, _ => approve,
    };
    collapse_with_sender(&store, &current, approve, name, &label, lang, &Network::new(home)).await;
}

pub(crate) fn decision_channel_label(rec: &ApprovalRecord, lang: Lang) -> String {
    if let Some(chat) = rec.decided_by.as_deref().and_then(|by| by.parse::<ledger::DecidedBy>().ok()) {
        press::channel_label(chat.channel()).into()
    } else {
        press::words(lang, "儀表板", "Dashboard", "ダッシュボード")
    }
}

async fn recorded_name(home: &Path, rec: &ApprovalRecord) -> Option<String> {
    let by = rec.decided_by.as_deref()?;
    let user = if let Ok(chat) = by.parse::<ledger::DecidedBy>() { chat.user_id().to_owned() }
        else { by.strip_prefix("dashboard:")?.to_owned() };
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let db = crate::decision_notify::open_user_db(&home).ok()??;
        db.get_user(&user).ok()?
    }).await.ok().flatten()
        .map(|user| user.display_name.trim().to_owned()).filter(|name| !name.is_empty())
}

pub(super) async fn collapse_with_sender(
    store: &ApprovalStore, rec: &ApprovalRecord, approve: bool, name: Option<String>,
    label: &str, lang: Lang, sender: &impl Sender,
) {
    let Some(at) = rec.decided_at.as_deref().and_then(|at| DateTime::parse_from_rfc3339(at).ok()) else {
        tracing::warn!("chat-admin card sync decision timestamp unavailable"); return;
    };
    let name = name.unwrap_or_else(|| press::words(lang, "管理員", "Administrator", "管理者"));
    let text = card::result_text(card::kind_label(rec, lang), approve, &name, at.with_timezone(&Utc), label, lang);
    let live = match receipts::list_live(store, &rec.id).await {
        Ok(live) => live,
        Err(_) => { tracing::warn!("chat-admin card sync receipts unavailable"); return; }
    };
    rewrite(store, live, &text, TerminalStatus::Retired, sender).await;
}

/// Call only after the expiry/invalidation/withdrawal transaction commits.
pub(crate) async fn expire_cards(home: &Path, rec: &ApprovalRecord) {
    let Some(store) = open_store(home).await else { return; };
    expire_with_sender(&store, rec, press::language(home).await, &Network::new(home)).await;
}

pub(crate) fn spawn_expire(home: PathBuf, rec: ApprovalRecord) {
    let authority = duduclaw_mcp_boundary::io_authority::capture();
    tokio::spawn(async move { authority.scope(expire_cards(&home, &rec)).await });
}

pub(super) async fn expire_with_sender(store: &ApprovalStore, rec: &ApprovalRecord, lang: Lang, sender: &impl Sender) {
    let live = match receipts::list_live(store, &rec.id).await {
        Ok(live) => live,
        Err(_) => { tracing::warn!("chat-admin card expiry receipts unavailable"); return; }
    };
    rewrite(store, live, &card::expired_text(card::kind_label(rec, lang), lang), TerminalStatus::Expired, sender).await;
}

/// Exported for approvals.chat_admin.set (P18). Does not require the gateway
/// lock: the settings caller invokes this directly after disabling the feature.
pub(crate) async fn sweep_feature_off(home: &Path) {
    let Some(store) = open_store(home).await else { return; };
    sweep_feature_off_with_sender(home, &store, &Network::new(home)).await;
}

async fn feature_enabled(home: &Path) -> Option<bool> {
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let users = crate::decision_notify::open_user_db(&home).ok().flatten();
        config::effective(&home, users.as_ref()).is_some()
    }).await.ok()
}

pub(super) async fn sweep_feature_off_with_sender(home: &Path, store: &ApprovalStore, sender: &impl Sender) {
    let live = match receipts::list_all_live(store).await {
        Ok(live) if !live.is_empty() => live,
        Ok(_) => return,
        Err(_) => { tracing::warn!("chat-admin feature-off receipts unavailable"); return; }
    };
    if feature_enabled(home).await == Some(true) { return; }
    let lang = press::language(home).await;
    rewrite(store, live, card::feature_off_text(lang), TerminalStatus::Retired, sender).await;
}

/// Runs from the existing gateway maintenance task, at most once per minute.
/// The supervisor owns throttling and prevents overlapping passes.
pub(crate) async fn maintenance(home: &Path) {
    if !duduclaw_core::gateway_instance::held(home) { return; }
    let check_home = home.to_path_buf();
    if !tokio::task::spawn_blocking(move || check_home.join("approvals.db").exists()).await.unwrap_or(false) { return; }
    let Some(store) = open_store(home).await else { return; };
    let live = match receipts::list_all_live(&store).await {
        Ok(live) if !live.is_empty() => live,
        Ok(_) => return,
        Err(_) => { tracing::warn!("chat-admin maintenance receipts unavailable"); return; }
    };
    if feature_enabled(home).await != Some(true) {
        // Use the exported path, including its effective-config recheck.
        sweep_feature_off(home).await;
        return;
    }
    let broker = ApprovalBroker::new(std::sync::Arc::new(store));
    // Preserve P12's existing once-only reminder path while making TTL expiry
    // independent of dashboard polling for approvals with delivered cards.
    if broker.expire_stale().await.is_err() {
        tracing::warn!("chat-admin maintenance approval expiry unavailable");
    }
    maintain_live(home, &broker.store, live, &Network::new(home)).await;
}

pub(super) async fn maintain_live(home: &Path, store: &ApprovalStore, live: Vec<Receipt>, sender: &impl Sender) {
    let lang = press::language(home).await;
    let mut seen = HashSet::new();
    for receipt in live {
        if !seen.insert(receipt.approval_id.clone()) { continue; }
        let rec = match store.get(&ApprovalId::from(receipt.approval_id.clone())).await {
            Ok(Some(rec)) => rec,
            _ => { tracing::warn!("chat-admin maintenance approval unavailable"); continue; }
        };
        if rec.status == ApprovalStatus::Pending {
            let cards = match receipts::list_live(store, &rec.id).await {
                Ok(cards) => cards,
                Err(_) => { tracing::warn!("chat-admin maintenance receipts unavailable"); continue; }
            };
            let expired = cards.into_iter().filter(|r|
                !DateTime::parse_from_rfc3339(&r.valid_until).is_ok_and(|at| at > Utc::now())).collect();
            rewrite(store, expired, &card::expired_text(card::kind_label(&rec, lang), lang), TerminalStatus::Expired, sender).await;
        } else if matches!(rec.status, ApprovalStatus::Approved | ApprovalStatus::Denied) {
            let label = decision_channel_label(&rec, lang);
            collapse_with_sender(store, &rec, rec.status == ApprovalStatus::Approved,
                recorded_name(home, &rec).await, &label, lang, sender).await;
        } else {
            // Also reconciles direct SQL invalidations, startup migrations and
            // system withdrawals that do not emit an approval lifecycle event.
            expire_with_sender(store, &rec, lang, sender).await;
        }
    }
}

pub(super) fn target(receipt: &Receipt) -> Option<Target> {
    ledger::parse_channel(&receipt.channel).ok()?;
    let agent = if receipt.account == receipt.channel { None } else {
        let (channel, agent) = receipt.account.split_once(':')?;
        if channel != receipt.channel || !duduclaw_core::is_valid_agent_id(agent) { return None; }
        Some(agent.to_owned())
    };
    Some(Target { user_id: receipt.user_id.clone(), agent,
        proof: ChatApprovalProof { channel: receipt.channel.clone(), channel_user_id: receipt.principal.clone(),
            proof_account: receipt.account.clone(), proof_dm: receipt.dm.clone(), proven_at: 0 } })
}

/// A step-up decision consumes its receipt before any provider edit. Re-read the
/// committed result and edit that exact original delivery, never another bot.
pub(crate) async fn collapse_step_up_card(home: &Path, store: &ApprovalStore, receipt: &Receipt) {
    let Ok(Some(current)) = store.get(&ApprovalId::from(receipt.approval_id.clone())).await else { return; };
    if !matches!(current.status, ApprovalStatus::Approved | ApprovalStatus::Denied) { return; }
    let Some(at) = current.decided_at.as_deref().and_then(|v| DateTime::parse_from_rfc3339(v).ok()) else { return; };
    let (Some(target), Some(mid)) = (target(receipt), &receipt.message_id) else { return; };
    let lang = press::language(home).await;
    let name = recorded_name(home, &current).await.unwrap_or_else(|| press::words(lang, "管理員", "Administrator", "管理者"));
    let text = card::result_text(card::kind_label(&current, lang), current.status == ApprovalStatus::Approved,
        &name, at.with_timezone(&Utc), &decision_channel_label(&current, lang), lang);
    let message = PushedMessage { edit_chat_id: receipt.dm.clone(), message_id: mid.clone() };
    let sender = Network::new(home);
    if !tokio::time::timeout(Duration::from_secs(15), sender.edit(&target, &message, &text)).await.unwrap_or(false) {
        tracing::warn!("chat-admin step-up result card edit failed");
    }
}

async fn rewrite(store: &ApprovalStore, live: Vec<Receipt>, text: &str, status: TerminalStatus, sender: &impl Sender) {
    for receipt in live.into_iter().filter(|r| r.purpose == "rm143" && r.status == "live") {
        // Claim the terminal transition before network I/O. A competing press
        // can consume the receipt only before this CAS, in which case skip it.
        // Never edit a consumed card, or hold a DB guard across an HTTP await.
        match receipts::mark(store, &receipt.card_id, status).await {
            Ok(true) => (), Ok(false) => continue,
            Err(_) => { tracing::warn!("chat-admin card sync retirement failed"); continue; }
        }
        // LINE has no edit API; retirement alone invalidates its old buttons.
        if receipt.channel == "line" { continue; }
        let edited = match (target(&receipt), receipt.message_id.as_ref()) {
            (Some(target), Some(message_id)) => {
                let message = PushedMessage { edit_chat_id: receipt.dm.clone(), message_id: message_id.clone() };
                tokio::time::timeout(Duration::from_secs(15), sender.edit(&target, &message, text)).await.unwrap_or(false)
            }
            _ => false,
        };
        if !edited {
            // Static diagnostic: no message, provider error, token, or chat ID.
            tracing::warn!("chat-admin card edit failed; receipt remains terminal");
        }
    }
}
