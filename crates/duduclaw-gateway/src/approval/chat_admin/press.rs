//! Receipt-bound, verified-transport entry for RM-143. No payload identity.
use std::{future::Future, path::{Path, PathBuf}, sync::Arc};
use chrono::Utc;
use crate::{approval::{ApprovalBroker, ApprovalId, ApprovalRecord, ApprovalStatus, DecisionContext,
    decision_service::{self, ChatAdminProof, Decider, DecisionEnv, DecisionRefusal}},
    channel_reply::ReplyContext,
    decision_action::{DecisionAct, DecisionAction, DecisionSource},
    decision_notify::{check_trusted_decision_access, DecisionAccessScope},
    trigger_route::texts::Lang};
use super::{card, config, deliver, ledger, receipts};

/// All fields come from the receiving adapter's verified event/configuration.
#[derive(Clone, Debug)]
pub(crate) struct PressOrigin {
    pub message_id: Option<String>,
    pub private: bool,
    pub account: String,
    pub dm: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PressOutcome {
    pub reply: String,
    pub card_text: Option<String>,
    pub card_link: Option<CardLink>,
    pub refused_reason: Option<&'static str>,
}

// Do not derive Debug: the URL can carry a single-use ticket.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CardLink { pub label: String, pub url: String }

impl std::fmt::Debug for CardLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CardLink([redacted])")
    }
}

impl PressOutcome {
    pub(crate) fn from_result(result: Result<String, String>) -> Self {
        match result {
            Ok(reply) => Self { reply, card_text: None, card_link: None, refused_reason: None },
            Err(reply) => Self { reply, card_text: None, card_link: None, refused_reason: Some("legacy_refused") },
        }
    }

    /// P14 adapters will also consume card_text to rewrite the pressed card.
    pub(crate) fn into_reply(self) -> Result<String, String> {
        if self.refused_reason.is_some() { Err(self.reply) } else { Ok(self.reply) }
    }

    pub(crate) fn refused(reply: String, reason: &'static str, rewrite: bool) -> Self {
        Self { card_text: rewrite.then(|| reply.clone()), card_link: None, reply, refused_reason: Some(reason) }
    }
}

pub(crate) fn words(lang: Lang, zh: &str, en: &str, ja: &str) -> String {
    match lang { Lang::ZhTw => zh, Lang::En => en, Lang::Ja => ja }.into()
}

pub(crate) async fn language(home: &Path) -> Lang {
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || crate::trigger_route::texts::lang_for(&home))
        .await.unwrap_or(Lang::ZhTw)
}

/// Both filesystem classification and live effective-config reads are blocking.
pub(crate) async fn owns(home: &Path, row: &ApprovalRecord) -> bool {
    let (home, row) = (home.to_path_buf(), row.clone());
    tokio::task::spawn_blocking(move || {
        let users = crate::decision_notify::open_user_db(&home).ok().flatten();
        config::effective(&home, users.as_ref()).is_some()
            && matches!(super::chat_tier(Some(&home), &row), super::Tier::T1 | super::Tier::T2)
    }).await.unwrap_or(false)
}

async fn refuse(home: &Path, context: &DecisionContext, id: &ApprovalId,
    origin: &PressOrigin, reason: &'static str, lang: Lang) -> PressOutcome {
    decision_service::identity::audit_refusal_reason(home, id, &context.channel,
        &origin.account, &context.principal_id, reason).await;
    PressOutcome::refused(DecisionRefusal::Forbidden(String::new()).user_text(lang), reason, false)
}

pub(crate) async fn handle(
    ctx: &ReplyContext, context: &DecisionContext, action: &DecisionAction,
    row: &ApprovalRecord, origin: &PressOrigin, scope: DecisionAccessScope<'_>,
) -> PressOutcome {
    handle_with_resend(ctx, context, action, row, origin, scope,
        |home, broker, row, user| async move {
            deliver::deliver_to_user(&home, &broker, &row, &user).await.delivered > 0
        }).await
}

// An injected resend operation lets tests prove drift never reaches decide,
// without making network calls or installing a process-global mock.
async fn handle_with_resend<F, Fut>(
    ctx: &ReplyContext, context: &DecisionContext, action: &DecisionAction,
    row: &ApprovalRecord, origin: &PressOrigin, scope: DecisionAccessScope<'_>, resend: F,
) -> PressOutcome
where F: FnOnce(PathBuf, ApprovalBroker, ApprovalRecord, String) -> Fut,
      Fut: Future<Output = bool>,
{
    let home = &ctx.home_dir;
    let lang = language(home).await;
    let id = ApprovalId::from(action.id.clone());
    // 1. Preserve the adapter's full access scope, including guild/session.
    if check_trusted_decision_access(ctx, context, scope, "").await.is_err() {
        return refuse(home, context, &id, origin, "channel_access", lang).await;
    }
    // 2. Unknown privacy must never grant authority.
    if !origin.private {
        return refuse(home, context, &id, origin, "not_private", lang).await;
    }
    if action.source != DecisionSource::Approval
        || !matches!(action.act, DecisionAct::Approve | DecisionAct::Deny)
        || uuid::Uuid::parse_str(&action.id).is_err() || row.id.as_str() != action.id {
        return refuse(home, context, &id, origin, "invalid_action", lang).await;
    }
    let owned_home = home.clone();
    let opened = tokio::task::spawn_blocking(move || {
        let broker = ApprovalBroker::open(&owned_home)?;
        let users = crate::decision_notify::open_user_db(&owned_home)?.map(Arc::new);
        Ok::<_, String>((broker, users))
    }).await;
    let Ok(Ok((broker, users))) = opened else {
        return refuse(home, context, &id, origin, "store_unavailable", lang).await;
    };
    let env = DecisionEnv { home: home.clone(), registry: Some(ctx.registry.clone()), broker, users };
    // 3. Lookup uses the actual receiving account, never callback metadata.
    let receipt = if context.channel == "line" {
        let (users, principal) = (env.users.clone(), context.principal_id.clone());
        let user = tokio::task::spawn_blocking(move || users.as_ref()?
            .find_verified_user_id_by_channel("line", &principal).ok().flatten()).await.ok().flatten();
        let Some(user) = user else {
            return refuse(home, context, &id, origin, "receipt_missing", lang).await;
        };
        match receipts::lookup_without_message_id(&env.broker.store, &id, &user, &origin.dm).await {
            Ok(rows) => {
                let mut matching = rows.into_iter().filter(|r| r.channel == "line"
                    && r.account == origin.account && r.principal == context.principal_id);
                // A retired LINE card carries the same callback as its
                // replacement. Without a message ID we cannot distinguish
                // which content was pressed: multiple deliveries must deny.
                let first = matching.next();
                if matching.next().is_some() {
                    return refuse(home, context, &id, origin, "receipt_ambiguous", lang).await;
                }
                first
            }
            Err(_) => return refuse(home, context, &id, origin, "receipt_unreadable", lang).await,
        }
    } else {
        let Some(message) = origin.message_id.as_deref().filter(|m| !m.is_empty()) else {
            return refuse(home, context, &id, origin, "message_id_missing", lang).await;
        };
        match receipts::lookup_message(&env.broker.store, &context.channel, &origin.account, &origin.dm, message).await {
            Ok(receipt) => receipt,
            Err(_) => return refuse(home, context, &id, origin, "receipt_unreadable", lang).await,
        }
    };
    let Some(receipt) = receipt else {
        return refuse(home, context, &id, origin, "receipt_missing", lang).await;
    };
    if let Some(reason) = receipt_mismatch(&receipt, context, action, origin) {
        return refuse(home, context, &id, origin, reason, lang).await;
    }
    // Re-read instead of hashing a potentially stale routing snapshot.
    let current = match env.broker.get(&id).await {
        Ok(Some(rec)) => rec,
        _ => return refuse(home, context, &id, origin, "row_unavailable", lang).await,
    };
    let settled = current.status != ApprovalStatus::Pending;
    if receipt.status != "live" && !(receipt.status == "consumed" && settled) {
        let mut outcome = refuse(home, context, &id, origin, "receipt_not_live", lang).await;
        let (owned_home, users) = (home.clone(), env.users.clone());
        let enabled = tokio::task::spawn_blocking(move || config::effective(&owned_home, users.as_deref()).is_some())
            .await.unwrap_or(false);
        if !enabled {
            outcome.reply = card::feature_off_text(lang).into();
            outcome.card_text = Some(outcome.reply.clone());
        }
        return outcome;
    }
    // 4. Invalid expiry is expired too; terminal transitions cannot revive it.
    if !settled && !chrono::DateTime::parse_from_rfc3339(&receipt.valid_until).is_ok_and(|t| t > Utc::now()) {
        let marked = receipts::mark(&env.broker.store, &receipt.card_id, receipts::TerminalStatus::Expired).await;
        if marked.is_err() {
            return refuse(home, context, &id, origin, "receipt_mark_failed", lang).await;
        }
        let mut outcome = refuse(home, context, &id, origin, "card_expired", lang).await;
        outcome.reply = words(lang, "這張卡已失效，請到儀表板處理", "This card has expired. Please use the dashboard.", "このカードは期限切れです。ダッシュボードで処理してください。");
        outcome.card_text = Some(card::expired_text(card::kind_label(&current, lang), lang));
        return outcome;
    }
    let Ok(channel) = ledger::parse_channel(&context.channel) else {
        return refuse(home, context, &id, origin, "unsupported_channel", lang).await;
    };
    // 5. Only the service checks live Admin/handshake/configured-bot identity.
    let proof = ChatAdminProof { user_id: receipt.user_id.clone(), channel,
        account: origin.account.clone(), principal: context.principal_id.clone(), dm: origin.dm.clone(),
        card_id: receipt.card_id.clone(), digest: receipt.digest.clone(), step: ledger::Step::Tap };
    if let Err(error) = decision_service::validate_chat_identity(&env, &current, action.approve(), &proof).await {
        return if matches!(error, DecisionRefusal::FeatureOff) {
            PressOutcome::refused(card::feature_off_text(lang).into(), "feature_off", true)
        } else { PressOutcome::refused(error.user_text(lang), error.code(), false) };
    }
    if settled {
        decision_service::identity::audit_refusal_reason(home, &id, &context.channel,
            &origin.account, &context.principal_id, "already_decided").await;
        return already_decided(&env, current.decided_by.as_deref(), current.status, lang).await;
    }
    // 6. A changed card cannot be used even for a denial.
    let (digest_home, digest_row) = (home.clone(), current.clone());
    let digest = tokio::task::spawn_blocking(move || crate::approval::decision_digest_at(&digest_home, &digest_row)).await;
    let Ok(digest) = digest else {
        return refuse(home, context, &id, origin, "digest_unavailable", lang).await;
    };
    if current.status == ApprovalStatus::Pending && digest != receipt.digest {
        if !matches!(receipts::mark(&env.broker.store, &receipt.card_id, receipts::TerminalStatus::Retired).await, Ok(true)) {
            return refuse(home, context, &id, origin, "receipt_mark_failed", lang).await;
        }
        let sent = resend(home.clone(), env.broker.clone(), current, receipt.user_id).await;
        let reply = if sent {
            words(lang, "內容已更新，已送出新卡", "The content has changed. A new card was sent.", "内容が更新されました。新しいカードを送信しました。")
        } else { DecisionRefusal::DigestMismatch.user_text(lang) };
        decision_service::identity::audit_refusal_reason(home, &id, &context.channel,
            &origin.account, &context.principal_id, "digest_mismatch").await;
        return PressOutcome::refused(reply, "digest_mismatch", true);
    }
    // 7. All writes, identity checks and side effects share the decision lock.
    match decision_service::decide(&env, &id, action.approve(), "", &Decider::ChatAdmin(proof)).await {
        Ok(result) => {
            // 8. Text helpers sanitize display names and kind labels.
            let name = result.display_name.unwrap_or_else(|| admin_label(lang));
            let text = card::result_text(card::kind_label(&current, lang),
                result.status == ApprovalStatus::Approved, &name, Utc::now(),
                channel_label(result.channel.unwrap_or(channel)), lang);
            PressOutcome { reply: text.clone(), card_text: Some(text), card_link: None, refused_reason: None }
        }
        Err(DecisionRefusal::StepUpRequired) => super::step_up::begin(&env, &current, &receipt, lang).await,
        // 9. Use the recorded decider, never the presser's display name.
        Err(DecisionRefusal::AlreadyDecided { by, status, .. }) => already_decided(&env, by.as_deref(), status, lang).await,
        Err(DecisionRefusal::FeatureOff) => PressOutcome::refused(card::feature_off_text(lang).into(), "feature_off", true),
        Err(error) => PressOutcome::refused(error.user_text(lang), error.code(), false),
    }
}

fn receipt_mismatch(r: &receipts::Receipt, context: &DecisionContext,
    action: &DecisionAction, origin: &PressOrigin) -> Option<&'static str> {
    if r.purpose != "rm143" { Some("receipt_purpose") }
    else if r.approval_id != action.id { Some("approval_id_mismatch") }
    else if r.principal != context.principal_id { Some("principal_mismatch") }
    else if r.channel != context.channel { Some("channel_mismatch") }
    else if r.account != origin.account { Some("account_mismatch") }
    else if r.dm != origin.dm { Some("dm_mismatch") }
    else { None }
}

fn admin_label(lang: Lang) -> String { words(lang, "管理員", "Administrator", "管理者") }

pub(super) fn channel_label(channel: config::ChatChannel) -> &'static str {
    match channel {
        config::ChatChannel::Telegram => "Telegram", config::ChatChannel::Googlechat => "Google Chat",
        config::ChatChannel::Line => "LINE", config::ChatChannel::Slack => "Slack", config::ChatChannel::Discord => "Discord",
    }
}

async fn already_decided(env: &DecisionEnv, by: Option<&str>, status: ApprovalStatus, lang: Lang) -> PressOutcome {
    let parsed = by.and_then(|by| {
        if let Ok(chat) = by.parse::<ledger::DecidedBy>() {
            Some((chat.user_id().to_owned(), channel_label(chat.channel()).to_owned()))
        } else { by.strip_prefix("dashboard:").map(|uid|
            (uid.to_owned(), words(lang, "儀表板", "Dashboard", "ダッシュボード"))) }
    });
    let text = if let Some((uid, channel)) = parsed {
        let name = decision_service::identity::display_name(env, &uid).await.unwrap_or_else(|| admin_label(lang));
        card::already_decided_text(&name, &channel, status == ApprovalStatus::Approved, lang)
    } else {
        DecisionRefusal::AlreadyDecided { id: ApprovalId::from(String::new()), by: None, status }.user_text(lang)
    };
    PressOutcome::refused(text, "already_decided", true)
}

#[cfg(test)]
pub(crate) mod tests;
