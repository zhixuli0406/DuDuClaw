//! HTTP-independent operations; server.rs supplies only a verified dashboard JWT.
use super::*;
use super::store::{Store, Ticket};
use crate::{approval::{ApprovalId, ApprovalStatus, decision_service::{self, ChatAdminProof, Decider}},
    handlers::MethodHandler, protocol::WsFrame};
use super::super::{ledger, receipts};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Decision {
    pub decision: Choice,
    pub expected_digest: String,
    pub step_up_csrf: Option<[u8; 32]>,
}
#[derive(Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Choice { Approve, Reject }
impl Choice {
    fn approve(self) -> bool { matches!(self, Self::Approve) }
    fn as_str(self) -> &'static str { if self.approve() { "approve" } else { "reject" } }
}

struct Loaded { ticket: Ticket, receipt: Receipt, row: ApprovalRecord, live: UserContext, totp: bool }

async fn load(env: &DecisionEnv, ctx: &UserContext, session: &str, raw: &str) -> Result<Loaded, Error> {
    let (home, db, ctx, session, raw) = (env.home.clone(), env.users.clone(), ctx.clone(), session.to_owned(), raw.to_owned());
    let (ticket, live, totp) = tokio::task::spawn_blocking(move || {
        let db = db.as_deref().ok_or_else(Error::forbidden)?;
        let live = active_admin(db, &ctx, &session)?;
        let ticket = Store::open(&home)?.preview(&raw, &live.user_id)?;
        let totp = has_totp(db, &live.user_id)?;
        Ok::<_, Error>((ticket, live, totp))
    }).await.map_err(Error::unavailable)??;
    let receipt = receipts::by_card_id(&env.broker.store, &ticket.card_id).await
        .map_err(Error::unavailable)?.ok_or_else(Error::missing)?;
    if receipt.status != "live" || receipt.purpose != "rm143" || receipt.user_id != ticket.user_id
        || receipt.approval_id != ticket.approval_id || receipt.digest != ticket.digest {
        return Err(Error::forbidden());
    }
    if !chrono::DateTime::parse_from_rfc3339(&receipt.valid_until).is_ok_and(|t| t > chrono::Utc::now()) {
        return Err(Error::expired());
    }
    let row = env.broker.get(&ApprovalId::from(ticket.approval_id.clone())).await
        .map_err(Error::unavailable)?.ok_or_else(Error::missing)?;
    let proof = proof(&ticket, &receipt)?;
    decision_service::validate_chat_identity(env, &row, false, &proof).await
        .map_err(|e| Error::decision(e, Lang::ZhTw))?;
    if row.is_stale(chrono::Utc::now()) { return Err(Error::expired()); }
    if row.status != ApprovalStatus::Pending { return Err(Error::new(StatusCode::CONFLICT,
        "already_decided", "這筆審核已有決定，請到儀表板查看結果。")); }
    let (home, current) = (env.home.clone(), row.clone());
    let digest = tokio::task::spawn_blocking(move || crate::approval::decision_digest_at(&home, &current))
        .await.map_err(Error::unavailable)?;
    if digest != ticket.digest { return Err(Error::digest()); }
    Ok(Loaded { ticket, receipt, row, live, totp })
}

fn proof(ticket: &Ticket, receipt: &Receipt) -> Result<ChatAdminProof, Error> {
    Ok(ChatAdminProof { user_id: ticket.user_id.clone(),
        channel: ledger::parse_channel(&receipt.channel).map_err(|_| Error::forbidden())?,
        account: receipt.account.clone(), principal: receipt.principal.clone(), dm: receipt.dm.clone(),
        card_id: receipt.card_id.clone(), digest: ticket.digest.clone(), step: ledger::Step::Web })
}

pub(crate) async fn preview(handler: &MethodHandler, env: &DecisionEnv, ctx: &UserContext,
    session: &str, raw: &str) -> Result<Value, Error> {
    let loaded = load(env, ctx, session, raw).await?;
    // Reuse the actual dashboard implementation, including visibility, personal
    // data gates and bound-workflow projection. Never synthesize a payload here.
    let frame = crate::google_personal::DASHBOARD_SESSION.scope(
        Some((loaded.live.user_id.clone(), session.to_owned())),
        handler.handle_approvals_list(json!({"id":loaded.ticket.approval_id}), &loaded.live),
    ).await;
    let WsFrame::Response { ok: true, payload: Some(payload), .. } = frame else { return Err(Error::forbidden()); };
    let approval = payload.get("approvals").and_then(Value::as_array)
        .and_then(|items| items.first()).ok_or_else(Error::missing)?;
    // A concurrent content change must not pair old ticket authority with a new
    // review projection; redacted/ineligible projections are refused as well.
    if approval.get("digest").and_then(Value::as_str) != Some(loaded.ticket.digest.as_str()) {
        return Err(Error::digest());
    }
    // The approval center fetches this existing Admin detail projection for
    // file changes: the proposal is a sidecar, not part of approvals.payload.
    let agent_file_change = if loaded.row.action_kind == crate::agent_files::approval::ACTION_KIND {
        let frame = handler.handle_agent_files_pending_get(
            json!({"approval_id":loaded.ticket.approval_id}), &loaded.live).await;
        let WsFrame::Response { ok: true, payload: Some(detail), .. } = frame else { return Err(Error::forbidden()); };
        if detail.get("stale").and_then(Value::as_bool) != Some(false)
            || detail.get("content_available").and_then(Value::as_bool) != Some(true) {
            return Err(Error::digest());
        }
        Some(detail)
    } else { None };
    let lang = super::super::press::language(&env.home).await;
    Ok(json!({"status":"live", "kind_label":card::kind_label(&loaded.row, lang),
        "approval":approval, "digest":loaded.ticket.digest, "has_totp":loaded.totp,
        "expires":loaded.ticket.expires, "agent_file_change":agent_file_change}))
}

pub(crate) async fn confirm(env: &DecisionEnv, ctx: &UserContext, session: &str,
    raw: &str, input: Decision) -> Result<Value, Error> {
    let loaded = load(env, ctx, session, raw).await?;
    if input.expected_digest != loaded.ticket.digest { return Err(Error::digest()); }
    let approve = input.decision.approve();
    let proof = proof(&loaded.ticket, &loaded.receipt)?;
    let (home, db, ctx, session, ticket) = (env.home.clone(), env.users.clone(), ctx.clone(),
        session.to_owned(), loaded.ticket.clone());
    tokio::task::spawn_blocking(move || {
        let db = db.as_deref().ok_or_else(Error::forbidden)?;
        let live = active_admin(db, &ctx, &session)?;
        let cfg = config::effective(&home, Some(db)).ok_or_else(Error::forbidden)?;
        if !cfg.step_up_methods.contains(&config::StepMethod::Web) { return Err(Error::forbidden()); }
        if approve {
            let csrf = input.step_up_csrf.ok_or_else(Error::step_up)?;
            let fresh = db.migration_confirmation_session(&live.user_id, &session,
                live.credential_epoch, chrono::Utc::now().timestamp()).map_err(|_| Error::step_up())?
                .ok_or_else(Error::step_up)?;
            ring::constant_time::verify_slices_are_equal(&fresh.csrf, &csrf).map_err(|_| Error::step_up())?;
        }
        Store::open(&home)?.consume(&ticket, &session)
    }).await.map_err(Error::unavailable)??;
    let lang = super::super::press::language(&env.home).await;
    let result = decision_service::decide(env, &loaded.row.id, approve, "", &Decider::ChatAdmin(proof)).await;
    // Service sync retires OTHER live cards. This particular receipt is consumed
    // by the ledger, so explicitly render its durable result using its own bot.
    super::super::sync::collapse_step_up_card(&env.home, &env.broker.store, &loaded.receipt).await;
    let result = result.map_err(|e| Error::decision(e, lang))?;
    Ok(json!({"status":"done", "decision":input.decision.as_str(), "decided_by":result.decided_by}))
}
