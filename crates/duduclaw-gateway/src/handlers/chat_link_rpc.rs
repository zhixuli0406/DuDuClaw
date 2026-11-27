//! RM-143: an identified Admin proves their own chat identity in a JWT session.
//! All SQLite and filesystem work runs in the blocking worker, without a
//! handler lock. Responses explicitly project store rows: never serialize a
//! Handshake wholesale (it also contains the raw principal and DM destination).

use super::{MethodHandler, UserContext, UserDb, UserRole, WsFrame};
use duduclaw_auth::{chat_link, otp};
use duduclaw_security::audit::{AuditEvent, Severity};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;

const UNAVAILABLE: &str = "目前無法處理聊天連結，請重新整理後再試。";
const NOT_FOUND: &str = "找不到這筆連結，或連結已失效，請重新產生。";
const LOGIN_REQUIRED: &str = "請使用管理員帳號登入儀表板後，再連結聊天帳號。";

pub(super) fn minimum_role(method: &str) -> Option<UserRole> {
    match method {
        "me.chat_link.start" | "me.chat_link.status" | "me.chat_link.confirm"
        | "me.chat_link.cancel" | "me.chat_link.reject" | "me.chat_link.list"
        | "me.chat_link.remove" => Some(UserRole::Admin),
        _ => None,
    }
}

fn session_hash(ctx: &UserContext) -> Result<String, &'static str> {
    // server::handle_socket sets DASHBOARD_SESSION only from a verified access
    // JWT (sub, jti). Member keys set member_key_id without a session; legacy
    // tokens/local fallback produce "system". MCP HTTP keys are authenticated
    // separately in cli::mcp_http_server and dispatch through McpDispatcher,
    // not a dashboard JWT. There is no chat-link MCP tool. Even an internal
    // caller with an Admin context cannot enter here without the JWT task-local.
    // HTTP attachment/member-key contexts likewise do not set this task-local.
    if ctx.user_id.is_empty() || ctx.user_id == "system" || ctx.member_key_id.is_some()
        || ctx.role != UserRole::Admin || ctx.must_change_password {
        return Err(LOGIN_REQUIRED);
    }
    let session = crate::google_personal::session_for(&ctx.user_id)
        .map_err(|_| LOGIN_REQUIRED)?;
    Ok(hex::encode(Sha256::digest(session.as_bytes())))
}

enum Request {
    Start(String), Status(String), Confirm(String), Cancel(String), Reject(String),
    List, Remove { channel: String, principal: String },
}

fn fields(params: &Value, allowed: &[&str]) -> Result<(), &'static str> {
    let object = params.as_object().ok_or("請提供正確的連結資料。")?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("連結資料包含不支援的欄位，請重新整理後再試。");
    }
    Ok(())
}

fn text_param(params: &Value, key: &str, max: usize) -> Result<String, &'static str> {
    params.get(key).and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control))
        .map(str::to_owned).ok_or("連結資料格式不正確，請重新整理後再試。")
}

fn request(method: &str, params: &Value) -> Result<Request, &'static str> {
    match method {
        "me.chat_link.start" => {
            fields(params, &["channel"])?;
            let channel = text_param(params, "channel", 32)?;
            if !chat_link::CHANNELS.contains(&channel.as_str()) {
                return Err("請選擇 Telegram 或 Google Chat。");
            }
            Ok(Request::Start(channel))
        }
        "me.chat_link.list" => { fields(params, &[])?; Ok(Request::List) }
        "me.chat_link.remove" => {
            fields(params, &["channel", "channel_user_id"])?;
            let channel = text_param(params, "channel", 32)?;
            if !otp::is_bindable_channel(&channel) { return Err("不支援這個聊天軟體。"); }
            Ok(Request::Remove { channel, principal: text_param(params, "channel_user_id", 512)? })
        }
        "me.chat_link.status" | "me.chat_link.confirm" | "me.chat_link.cancel" | "me.chat_link.reject" => {
            fields(params, &["handshake_id"])?;
            let id = text_param(params, "handshake_id", 36)?;
            let id = uuid::Uuid::parse_str(&id).map_err(|_| "連結編號格式不正確。")?.to_string();
            Ok(match method {
                "me.chat_link.status" => Request::Status(id),
                "me.chat_link.confirm" => Request::Confirm(id),
                "me.chat_link.cancel" => Request::Cancel(id),
                _ => Request::Reject(id),
            })
        }
        _ => Err("不支援此聊天連結操作。"),
    }
}

fn status_value(handshake: chat_link::Handshake) -> Value {
    json!({
        "handshake_id": handshake.handshake_id, "status": handshake.status,
        "channel": handshake.channel, "expires_at": handshake.expires_at,
        // Untrusted display text stays data. The dashboard must escape it.
        "display_name": handshake.matched_display.as_deref().map(|name| duduclaw_core::plain_line(name, 200)),
        "masked_target": handshake.masked_target,
    })
}

fn targets(home: &Path, channel: &str, agents: &[String]) -> Result<Vec<Value>, &'static str> {
    crate::chat_link_intercept::configured_accounts(home, channel, agents).map(|accounts| {
        accounts.into_iter().map(|account| {
            let identity = crate::agent_channels::identity::get(&account.account);
            json!({"channel": account.channel, "account": account.account,
                "agent_id": account.agent_id, "display_name": account.display_name,
                "bot_username": if channel == "telegram" { identity.as_deref() } else { None },
                "service_account_email": if channel == "googlechat" { identity.as_deref() } else { None },
                "project_number": account.project_number})
        }).collect()
    })
}

fn audit(db: &UserDb, home: &Path, user: &str, event: &str,
    channel: &str, masked: &str, account: Option<&str>) -> Result<(), &'static str> {
    let details = json!({"user_id": user, "channel": channel, "masked_principal": masked, "account": account});
    let activity = db.log_action(Some(user), event, Some(user), Some(&details.to_string()), None);
    let severity = if event == "channel_link_rejected" { Severity::Warning } else { Severity::Info };
    let record = AuditEvent::new(event, "chat_link", severity, details);
    let security = duduclaw_security::audit::try_append_audit_event(home, &record);
    if security.is_ok() {
        crate::security_autopilot::emit_security_autopilot_event(
            &record.severity, &record.event_type, Some(&record.agent_id),
            crate::security_autopilot::SOURCE_AUDIT,
        );
    }
    // The auth mutation has already committed. Do not claim it was undone or
    // silently report success when either audit sink fails.
    if activity.is_err() || security.is_err() {
        return Err("操作已完成，但紀錄未能完整儲存，請重新整理並聯絡管理員。");
    }
    Ok(())
}

impl MethodHandler {
    pub(crate) async fn handle_chat_link_rpc(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        let hash = match session_hash(ctx) {
            Ok(hash) => hash, Err(error) => return WsFrame::error_response("", error),
        };
        let request = match request(method, &params) {
            Ok(request) => request, Err(error) => return WsFrame::error_response("", error),
        };
        let db = self.user_db.read().await.clone();
        let Some(db) = db else { return WsFrame::error_response("", UNAVAILABLE); };
        let agents = if matches!(&request, Request::Start(_)) {
            self.registry.read().await.list().iter().map(|a| a.config.agent.name.clone()).collect()
        } else { Vec::new() };
        let home = self.home_dir.clone();
        let ctx = ctx.clone();
        let result = tokio::task::spawn_blocking(move || -> Result<Value, &'static str> {
            let user = &ctx.user_id;
            // Dispatch refreshes authority; repeat it after entering the worker
            // so queueing cannot preserve a demoted/suspended Admin's rights.
            let live = db.live_user_context(user).map_err(|_| UNAVAILABLE)?.ok_or(LOGIN_REQUIRED)?;
            if live.role != UserRole::Admin || live.must_change_password
                || live.credential_epoch != ctx.credential_epoch { return Err(LOGIN_REQUIRED); }
            match request {
                Request::Start(channel) => {
                    let targets = targets(&home, &channel, &agents)?;
                    let started = chat_link::start(&db, user, &channel, &hash).map_err(|e| {
                        if e == "chat link start limit reached" { "產生次數已達上限，請稍後再試。" }
                        else { "目前無法產生連結代碼，請確認登入與雙因素驗證設定後再試。" }
                    })?;
                    // Code is returned once, never included in activity/audit.
                    Ok(json!({"handshake_id": started.handshake_id, "code": started.code,
                        "expires_at": started.expires_at, "targets": targets}))
                }
                Request::Status(id) => {
                    let state = chat_link::status(&db, user, &id).map_err(|_| UNAVAILABLE)?.ok_or(NOT_FOUND)?;
                    Ok(status_value(state))
                }
                Request::Confirm(id) => {
                    let proof = chat_link::confirm(&db, user, &id, &hash)
                        .map_err(|_| "無法確認這筆連結，請回到產生代碼時的登入頁面，或重新產生代碼。")?;
                    audit(&db, &home, user, "channel_identity_proven", &proof.channel,
                        &otp::mask_target(&proof.channel_user_id), Some(&proof.proof_account))?;
                    Ok(json!({"confirmed": true}))
                }
                Request::Cancel(id) => {
                    let changed = chat_link::cancel(&db, user, &id).map_err(|_| UNAVAILABLE)?;
                    if !changed { return Err(NOT_FOUND); }
                    Ok(json!({"cancelled": true}))
                }
                Request::Reject(id) => {
                    if !chat_link::reject(&db, user, &id).map_err(|_| UNAVAILABLE)? { return Err(NOT_FOUND); }
                    // Read AFTER the successful transition: a match could race
                    // a pre-read. Rejected rows retain match data and cannot be
                    // matched again, so this also audits that racing match.
                    let state = chat_link::status(&db, user, &id).map_err(|_| UNAVAILABLE)?.ok_or(NOT_FOUND)?;
                    if let Some(masked) = state.masked_target {
                        audit(&db, &home, user, "channel_link_rejected", &state.channel,
                            &masked, state.matched_account.as_deref())?;
                    }
                    Ok(json!({"rejected": true}))
                }
                Request::List => {
                    let mut identities = Vec::new();
                    for row in db.channel_identities_for_user(user).map_err(|_| UNAVAILABLE)? {
                        let Some(proof) = db.channel_identity_proof_status(user, &row.channel, &row.channel_user_id)
                            .map_err(|_| UNAVAILABLE)? else { continue; };
                        identities.push(json!({"channel": row.channel, "channel_user_id": row.channel_user_id,
                            "verified": row.verified, "proof_status": proof}));
                    }
                    let mut handshakes = Vec::new();
                    for id in chat_link::pending_ids(&db, user).map_err(|_| UNAVAILABLE)? {
                        if let Some(state) = chat_link::status(&db, user, &id).map_err(|_| UNAVAILABLE)? {
                            if matches!(state.status, chat_link::HandshakeStatus::Waiting | chat_link::HandshakeStatus::Matched) {
                                handshakes.push(status_value(state));
                            }
                        }
                    }
                    Ok(json!({"identities": identities, "handshakes": handshakes}))
                }
                Request::Remove { channel, principal } => {
                    let proof = db.approval_proofs(user).map_err(|_| UNAVAILABLE)?.into_iter()
                        .find(|p| p.channel == channel && p.channel_user_id == principal).ok_or(NOT_FOUND)?;
                    if !db.clear_approval_proof(user, &channel, &principal)
                        .map_err(|_| UNAVAILABLE)? { return Err(NOT_FOUND); }
                    audit(&db, &home, user, "channel_identity_proof_revoked", &channel, &otp::mask_target(&principal),
                        Some(&proof.proof_account))?;
                    Ok(json!({"revoked": true}))
                }
            }
        }).await;
        match result {
            Ok(Ok(value)) => WsFrame::ok_response("", value),
            Ok(Err(error)) => WsFrame::error_response("", error),
            Err(_) => WsFrame::error_response("", UNAVAILABLE),
        }
    }
}

#[cfg(test)]
#[path = "chat_link_rpc_tests.rs"]
mod tests;
