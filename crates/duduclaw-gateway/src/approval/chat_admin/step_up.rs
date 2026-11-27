//! RM-143 P15 dashboard confirmation API (no frontend in this package).
//!
//! Link: `<configured external HTTPS base>/approvals/confirm#ticket=<64 lowercase hex>`.
//! The ticket is a locator, NEVER a login credential. The page reads the fragment
//! and sends `X-Approval-Ticket` on GET and POST `/api/me/approvals/step-up`, with
//! `Authorization: Bearer <dashboard access JWT>`. Never put it in a query, log,
//! analytics event, RPC params or localStorage. Use cache:no-store,
//! referrerPolicy:no-referrer, credentials:omit. Responses also set no-store and
//! no-referrer. POST requires JSON (4096-byte limit) and an allowed Origin.
//!
//! Login integration TODO for the later page package: AuthGuard currently saves
//! only the RM-147 `/me/google/authorize#ticket=…` fragment, and LoginPage only
//! permits that return target or browser-watch. Extend BOTH return allowlists
//! (web/src/lib/google-authorization.ts is the precedent) to the exact relative
//! `/approvals/confirm#ticket=[a-f0-9]{64}` shape, preserving pathname + hash in
//! `location.state.from` through login/MFA. Reject arbitrary return URLs. A
//! server redirect cannot recover a fragment. This package does not edit web/.
//!
//! GET 200: `{status:"live", kind_label:string, approval:<exact approvals.get
//! Admin projection>, digest:string, has_totp:boolean, expires:<Unix seconds>,
//! agent_file_change:<agent_files.pending_get projection including diff>|null}`.
//! GET does not bind a session, consume, or extend a ticket. Full content comes
//! from MethodHandler::handle_approvals_list (the approvals.get implementation),
//! after the same live chat authority checks; file-change details also reuse
//! the approval center's existing Admin-only agent_files.pending_get projection.
//!
//! Before approve, call the existing dashboard WS RPC
//! `isolation.connector_step_up` with `{password:string,code:string}` (TOTP only).
//! It uses migration_step_up and returns `{verified:true,step_up_csrf:[32 bytes]}`.
//! `step_up_csrf` is a JSON array of exactly 32 integers in 0..255, not hex/base64.
//! Use the SAME JWT session for WS and HTTP; proof lasts five minutes and is
//! bound to user, session and credential epoch. Unlike migration.confirmations
//! this RPC does not require a migration database or active isolation mode.
//! POST: `{decision:"approve"|"reject",expected_digest:string,
//! step_up_csrf?:[32 bytes]}`. Reject needs no password, TOTP or CSRF proof.
//! POST 200: `{status:"done",decision:"approve"|"reject",decided_by:string}`.
//!
//! Errors are JSON `{status:<code>,error:<plain localized sentence>}`:
//! 401 unauthenticated; 404 not_found for unknown/malformed/foreign tickets;
//! 410 expired for the owner's expired ticket (GET AND POST, retained one day);
//! 409 used, digest_mismatch, already_decided; 403 forbidden, feature_off,
//! not_supported, step_up_required; 400 invalid_request; 503 unavailable/internal.
//! The page must handle 410 as a normal expired-link sentence after login.
//! A validated POST atomically changes live -> done BEFORE calling decide.
//! Even a refusal/crash leaves it spent; never retry an uncertain side effect.
//! Concurrent POSTs have one winner. Invalid digest/step-up does not spend it.

mod store;
pub(crate) mod dm_totp;
pub(crate) mod routes;
#[cfg(test)]
pub(crate) mod tests;

use std::path::Path;
use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};
use duduclaw_auth::{UserContext, UserDb, UserRole};
use serde_json::{json, Value};
use crate::{approval::{ApprovalRecord, decision_service::{DecisionEnv, DecisionRefusal}},
    trigger_route::texts::Lang};
use super::{card, config, receipts::Receipt, press::{CardLink, PressOutcome, words}};

#[derive(Debug)]
pub(crate) struct Error { status: StatusCode, code: &'static str, message: String }
impl Error {
    fn new(status: StatusCode, code: &'static str, message: &str) -> Self {
        Self { status, code, message: message.into() }
    }
    fn missing() -> Self { Self::new(StatusCode::NOT_FOUND, "not_found", "找不到這張確認票，請回原對話重新操作。") }
    fn unavailable(_: impl std::fmt::Display) -> Self { Self::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "目前無法確認，請到儀表板處理。") }
    pub(crate) fn unauthenticated() -> Self { Self::new(StatusCode::UNAUTHORIZED, "unauthenticated", "請重新登入管理員帳號。") }
    pub(crate) fn forbidden() -> Self { Self::new(StatusCode::FORBIDDEN, "forbidden", "目前無法確認你的核准權限，請到儀表板處理。") }
    pub(crate) fn invalid() -> Self { Self::new(StatusCode::BAD_REQUEST, "invalid_request", "確認內容不正確，請重新載入。") }
    fn expired() -> Self { Self::new(StatusCode::GONE, "expired", "這張確認票已逾期，請回原對話重新操作。") }
    fn used() -> Self { Self::new(StatusCode::CONFLICT, "used", "這張確認票已使用，請到儀表板查看結果。") }
    fn step_up() -> Self { Self::new(StatusCode::FORBIDDEN, "step_up_required", "請再以密碼與驗證器驗證碼確認身分。") }
    fn digest() -> Self { Self::new(StatusCode::CONFLICT, "digest_mismatch", "內容已更新，請到儀表板重新確認。") }
    fn decision(error: DecisionRefusal, lang: Lang) -> Self {
        let status = match &error {
            DecisionRefusal::NotFound => StatusCode::NOT_FOUND,
            DecisionRefusal::Expired(_) => StatusCode::GONE,
            DecisionRefusal::AlreadyDecided { .. } | DecisionRefusal::DigestMismatch => StatusCode::CONFLICT,
            DecisionRefusal::Internal(_) | DecisionRefusal::Kind(_) => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::FORBIDDEN,
        };
        Self { status, code: error.code(), message: error.user_text(lang) }
    }
    fn body(&self) -> Value { json!({"status":self.code,"error":self.message}) }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response { response((self.status, Json(self.body())).into_response()) }
}
pub(crate) fn response(value: Response) -> Response {
    crate::google_authorization_requests::routes::private_response(value)
}

fn has_totp(db: &UserDb, user: &str) -> Result<bool, Error> {
    db.mfa_factors(user).map_err(Error::unavailable)?
        .pointer("/factors/totp").and_then(Value::as_bool).ok_or_else(|| Error::unavailable("factor"))
}

/// Re-read authority; role/credential epoch from an old access token is not enough.
pub(crate) fn active_admin(db: &UserDb, ctx: &UserContext, session: &str) -> Result<UserContext, Error> {
    if ctx.member_key_id.is_some() || uuid::Uuid::parse_str(&ctx.user_id).is_err()
        || uuid::Uuid::parse_str(session).is_err() {
        return Err(Error::unauthenticated());
    }
    let live = db.live_user_context(&ctx.user_id).map_err(|_| Error::forbidden())?
        .ok_or_else(Error::forbidden)?;
    if live.role != UserRole::Admin || live.must_change_password
        || live.credential_epoch != ctx.credential_epoch {
        return Err(Error::forbidden());
    }
    if !db.remember_session_valid(Some(session), &ctx.user_id, chrono::Utc::now().timestamp())
        .map_err(|_| Error::unauthenticated())? { return Err(Error::unauthenticated()); }
    Ok(live)
}

fn landing_base(home: &Path) -> Option<String> {
    let base = crate::deep_link::configured_public_base(home).ok()??;
    crate::google_authorization_requests::url::external_base(&base)
        .then(|| format!("{}approvals/confirm", base.as_str()))
}

/// Issues web locators and/or a receipt-bound Telegram prompt, then asks the
/// receiving adapter to edit its card. Never decides or consumes the receipt.
pub(crate) async fn begin(env: &DecisionEnv, row: &ApprovalRecord, receipt: &Receipt, lang: Lang) -> PressOutcome {
    begin_with_prompt(env, row, receipt, lang, |receipt, text| async move {
        let network = super::deliver::transport::Network::new(&env.home);
        tokio::time::timeout(std::time::Duration::from_secs(15), network.totp_prompt(&receipt, &text))
            .await.ok().flatten()
    }).await
}

async fn begin_with_prompt<F, Fut>(env: &DecisionEnv, row: &ApprovalRecord, receipt: &Receipt, lang: Lang, send: F) -> PressOutcome
where F: FnOnce(Receipt, String) -> Fut, Fut: std::future::Future<Output = Option<String>>,
{
    let original = receipt.clone();
    let fallback_id = row.id.to_string();
    let (home, db, receipt, row) = (env.home.clone(), env.users.clone(), receipt.clone(), row.clone());
    let issued = tokio::task::spawn_blocking(move || -> Option<(Option<String>, Option<String>)> {
        let db = db.as_deref()?;
        let cfg = config::effective(&home, Some(db))?;
        let user = db.live_user_context(&receipt.user_id).ok()??;
        if user.role != UserRole::Admin || user.must_change_password
            || !has_totp(db, &receipt.user_id).ok()? || receipt.status != "live"
            || receipt.purpose != "rm143" || receipt.approval_id != row.id.as_str()
            || receipt.digest != crate::approval::decision_digest_at(&home, &row)
            || !chrono::DateTime::parse_from_rfc3339(&receipt.valid_until).is_ok_and(|t| t > chrono::Utc::now()) {
            return None;
        }
        let mut store = store::Store::open(&home).ok()?;
        // Google Chat has no user reply-to-message binding. The research does
        // not establish a dedicated DM thread as equivalent proof of intent.
        // Never solicit TOTP there (HTTP or Pub/Sub). Apps also cannot delete a
        // user's code from Google Chat history; use web/dashboard instead.
        // Reserve the default web option first when only one slot remains.
        let web = if cfg.step_up_methods.contains(&config::StepMethod::Web) {
            landing_base(&home).and_then(|base| store.issue(&receipt).ok().map(|raw| format!("{base}#ticket={raw}")))
        } else { None };
        let totp = if receipt.channel == "telegram" && cfg.step_up_methods.contains(&config::StepMethod::DmTotp) {
            store.issue_totp(&receipt).ok()
        } else { None };
        Some((web, totp))
    }).await.ok().flatten();
    let (web, totp) = issued.unwrap_or_default();
    let mut prompted = false;
    if let Some(id) = totp {
        let message = send(original, dm_totp::prompt_text(lang)).await;
        let home = env.home.clone();
        prompted = tokio::task::spawn_blocking(move || {
            let Ok(store) = store::Store::open(&home) else { return false; };
            if let Some(message) = message {
                if store.attach_prompt(&id, &message).is_ok() { return true; }
            }
            let _ = store.void_totp(&id);
            false
        }).await.unwrap_or(false);
    }
    if web.is_some() || prompted {
        let text = if prompted {
            words(lang, "請回覆剛送出的提示訊息，輸入驗證碼。", "Reply to the new prompt with your verification code.", "送信された案内メッセージに確認コードを返信してください。")
        } else { card::step_up_needed_text(lang).to_owned() };
        return PressOutcome { reply: text.clone(), card_text: Some(text),
            card_link: web.map(|url| CardLink { label: words(lang, "前往確認", "Review request", "確認する"), url }),
            refused_reason: Some("step_up_required") };
    }
    let (home, id) = (env.home.clone(), fallback_id);
    let link = tokio::task::spawn_blocking(move || crate::deep_link::public_deep_link(
        &home, crate::deep_link::DeepLinkKind::Approval, &id)).await.ok().flatten();
    let mut text = words(lang, "這類審批請到儀表板核准", "Please approve this request on the dashboard.", "この依頼はダッシュボードで承認してください。");
    if let Some(link) = link { text.push(' '); text.push_str(&link); }
    PressOutcome::refused(text, "step_up_required", true)
}
