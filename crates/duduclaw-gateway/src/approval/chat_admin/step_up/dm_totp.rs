//! Private Telegram control-plane replies. Never log input or put it on the bus.
use super::{Error, Receipt, has_totp, store::{Store, TotpTicket}};
use crate::{approval::{ApprovalId, ApprovalStatus, decision_service::{self, ChatAdminProof, Decider, DecisionEnv}},
    trigger_route::texts::Lang};
use super::super::{card, config, ledger, press::words, receipts};

// No Debug: an event contains a second-factor secret.
pub(crate) struct Event {
    pub account: String,
    pub dm: String,
    pub principal: String,
    // Envelope fact: authorization failure must never disable suppression.
    pub envelope_private: bool,
    // Only grants access to verification/decision, never to message forwarding.
    pub authorized: bool,
    pub reply_to: Option<String>,
    pub prompt_hint: bool,
    pub text: String,
}

pub(crate) fn prompt_text(lang: Lang) -> String {
    words(lang, "請在 3 分鐘內回覆這則訊息，輸入 6 位數驗證碼",
        "Reply to this message within 3 minutes with your 6-digit verification code.",
        "3 分以内にこのメッセージに返信し、6 桁の確認コードを入力してください。")
}

/// A hint only suppresses content; it NEVER supplies identity or authority.
/// It also protects replies after retention expiry or an uncertain prompt send.
pub(crate) fn is_prompt(text: &str) -> bool {
    [Lang::ZhTw, Lang::En, Lang::Ja].into_iter().any(|lang| text == prompt_text(lang))
}

pub(crate) fn code_shaped(text: &str) -> bool {
    // Trimming is suppression-only. The actual verifier receives exact input.
    let text = text.trim();
    (text.len() == 6 && text.bytes().all(|b| b.is_ascii_digit()))
        || (text.len() == 32 && text.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn expired(lang: Lang) -> String {
    words(lang, "這項確認已失效，請到儀表板處理。", "This request has expired. Please use the dashboard.",
        "この確認は期限切れです。ダッシュボードで処理してください。")
}
fn dashboard(lang: Lang) -> String {
    words(lang, "目前無法確認，請到儀表板核准。", "Unable to confirm. Please approve on the dashboard.",
        "確認できません。ダッシュボードで承認してください。")
}

/// Adapter-owned sink also provides a recording seam for deletion tests.
#[async_trait::async_trait]
pub(crate) trait ReplySink: Sync {
    async fn delete_code(&self);
    async fn reply(&self, text: &str);
}

pub(crate) async fn intercept(env: &DecisionEnv, event: Event, lang: Lang, sink: &impl ReplySink) -> bool {
    intercept_with_verify(env, event, lang, sink, |db, home, user, code, now| {
        db.mfa_step_up_verify_totp_only(user, code, &crate::mfa::Cipher(home), now)
    }).await
}

async fn intercept_with_verify<F>(env: &DecisionEnv, event: Event, lang: Lang, sink: &impl ReplySink, verify: F) -> bool
where F: FnOnce(&duduclaw_auth::UserDb, &std::path::Path, &str, &str, i64) -> Result<(), String> + Send + 'static,
{
    if event.reply_to.is_none() && (!event.envelope_private || !code_shaped(&event.text)) { return false; }
    let could_carry_code = event.envelope_private && (code_shaped(&event.text) || event.prompt_hint);
    let authorized = event.authorized;
    let mut prompt_bound = false;
    let result = process(env, event, lang, verify, &mut prompt_bound).await;
    let (text, receipt) = match result {
        Ok(Some(result)) => result,
        Ok(None) => return false,
        Err(_) if !prompt_bound && !could_carry_code => return false,
        Err(error) if authorized && matches!(error.code, "expired" | "used") => (expired(lang), None),
        Err(_) => (dashboard(lang), None),
    };
    // Attempt deletion before slower cosmetic edits/replies, even on refusal.
    // Provider failure must neither retry a decision nor expose the code.
    sink.delete_code().await;
    sink.reply(&text).await;
    if let Some(receipt) = receipt {
        super::super::sync::collapse_step_up_card(&env.home, &env.broker.store, &receipt).await;
    }
    true
}

async fn process<F>(env: &DecisionEnv, event: Event, lang: Lang, verify: F, prompt_bound: &mut bool) -> Result<Option<(String, Option<Receipt>)>, Error>
where F: FnOnce(&duduclaw_auth::UserDb, &std::path::Path, &str, &str, i64) -> Result<(), String> + Send + 'static,
{
    if event.reply_to.is_none() {
        if !event.envelope_private || !code_shaped(&event.text) { return Ok(None); }
        let (home, account, dm, principal) = (env.home.clone(), event.account, event.dm, event.principal);
        let live = tokio::task::spawn_blocking(move || {
            Store::open(&home)?.has_live_totp(&account, &dm, &principal)
        }).await.map_err(Error::unavailable)??;
        // A successful lookup without a live flow leaves a bare code as ordinary text.
        // No verification is attempted on this path.
        if live && !event.authorized { return Ok(Some((dashboard(lang), None))); }
        return Ok(live.then(|| (words(lang,
            "請回覆驗證提示訊息，再輸入驗證碼。",
            "Please reply to the verification prompt with your code.",
            "確認コードの案内メッセージに返信して、コードを入力してください。"), None)));
    }
    let hint = event.envelope_private && event.prompt_hint;
    let (home, account, dm, prompt) = (env.home.clone(), event.account.clone(), event.dm.clone(), event.reply_to.clone());
    let ticket = tokio::task::spawn_blocking(move || {
        match prompt {
            Some(prompt) => Store::open(&home)?.prompt(&account, &dm, &prompt),
            None => Ok(None),
        }
    }).await.map_err(Error::unavailable)??;
    let Some(bound) = ticket else {
        return Ok(hint.then(|| (if event.authorized { expired(lang) } else { dashboard(lang) }, None)));
    };
    // Once a prompt is bound, later errors still consume even malformed input.
    *prompt_bound = true;
    // An invalid sender is still consumed, but cannot burn the owner's attempts.
    if !event.envelope_private || !event.authorized || event.principal != bound.principal || event.account != bound.account
        || event.dm != bound.dm || bound.channel != "telegram" {
        return Ok(Some((dashboard(lang), None)));
    }
    if bound.status != "live" || bound.attempts >= 5 || bound.ticket.expires <= chrono::Utc::now().timestamp() {
        return Ok(Some((expired(lang), None)));
    }
    let receipt = receipts::by_card_id(&env.broker.store, &bound.ticket.card_id).await
        .map_err(Error::unavailable)?.ok_or_else(Error::missing)?;
    if !matches_receipt(&bound, &receipt) { return Err(Error::forbidden()); }
    if !chrono::DateTime::parse_from_rfc3339(&receipt.valid_until).is_ok_and(|t| t > chrono::Utc::now()) {
        return Ok(Some((expired(lang), None)));
    }
    let id = ApprovalId::from(bound.ticket.approval_id.clone());
    let row = env.broker.get(&id).await.map_err(Error::unavailable)?.ok_or_else(Error::missing)?;
    if row.status != ApprovalStatus::Pending || row.is_stale(chrono::Utc::now()) {
        return Ok(Some((expired(lang), None)));
    }
    let proof = ChatAdminProof { user_id:bound.ticket.user_id.clone(), channel:config::ChatChannel::Telegram,
        account:bound.account.clone(), dm:bound.dm.clone(), principal:bound.principal.clone(),
        card_id:bound.ticket.card_id.clone(), digest:bound.ticket.digest.clone(), step:ledger::Step::Totp };
    // Re-read live Admin, handshake and settings BEFORE touching the factor.
    decision_service::validate_chat_identity(env, &row, true, &proof).await
        .map_err(|e| Error::decision(e, lang))?;
    let (home, db, current) = (env.home.clone(), env.users.clone(), row.clone());
    let verified = tokio::task::spawn_blocking(move || -> Result<Option<i64>, Error> {
        let db = db.as_deref().ok_or_else(Error::forbidden)?;
        // A live setting can disable an already-issued prompt. Refuse before
        // verifying the factor or changing ticket status/attempts.
        if !config::effective(&home, Some(db))
            .is_some_and(|cfg| cfg.step_up_methods.contains(&config::StepMethod::DmTotp)) {
            return Err(Error::forbidden());
        }
        if !has_totp(db, &bound.ticket.user_id)? { return Err(Error::forbidden()); }
        if crate::approval::decision_digest_at(&home, &current) != bound.ticket.digest { return Err(Error::digest()); }
        let mut store = Store::open(&home)?;
        // Recheck after the async identity preflight. CAS below also checks
        // expiry/attempts, so a concurrent fifth failure cannot grant approval.
        let live = store.prompt(&bound.account, &bound.dm, event.reply_to.as_deref().ok_or_else(Error::missing)?)?
            .ok_or_else(Error::missing)?;
        if live.status != "live" || live.attempts >= 5 || live.ticket.expires <= chrono::Utc::now().timestamp() {
            return Err(Error::expired());
        }
        if verify(db, &home, &bound.ticket.user_id, &event.text, chrono::Utc::now().timestamp()).is_err() {
            return Ok(Some(store.fail_totp(&bound.ticket.id)?));
        }
        store.consume_totp(&bound.ticket)?;
        Ok(None)
    }).await.map_err(Error::unavailable)??;
    if let Some(attempts) = verified {
        let text = if attempts >= 5 { dashboard(lang) } else {
            words(lang, "驗證碼不正確或已使用，請重新輸入。", "The code is incorrect or already used. Please try again.",
                "コードが正しくないか、使用済みです。もう一度入力してください。")
        };
        return Ok(Some((text, None)));
    }
    // Ticket is already spent. The service rechecks live authority, receipt,
    // digest and pending status under its own decision lock.
    let text = match decision_service::decide(env, &id, true, "", &Decider::ChatAdmin(proof)).await {
        Ok(result) => card::result_text(card::kind_label(&row, lang), result.status == ApprovalStatus::Approved,
            &result.display_name.unwrap_or_else(|| words(lang, "管理員", "Administrator", "管理者")),
            chrono::Utc::now(), "Telegram", lang),
        Err(error) => error.user_text(lang),
    };
    Ok(Some((text, Some(receipt))))
}

fn matches_receipt(bound: &TotpTicket, receipt: &Receipt) -> bool {
    receipt.purpose == "rm143" && receipt.status == "live"
        && receipt.card_id == bound.ticket.card_id && receipt.approval_id == bound.ticket.approval_id
        && receipt.user_id == bound.ticket.user_id && receipt.digest == bound.ticket.digest
        && receipt.channel == bound.channel && receipt.account == bound.account
        && receipt.dm == bound.dm && receipt.principal == bound.principal
}

#[cfg(test)]
pub(crate) mod tests;
