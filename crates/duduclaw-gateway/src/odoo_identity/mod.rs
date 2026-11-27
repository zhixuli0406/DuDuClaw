//! Gateway-only Odoo credential handoff, one decision per tool call.
//!
//! POST /api/internal/odoo-identity has two loopback-only entry modes:
//! - Signed employee: {tool, turn_id?, run_id?, have_rev?}. Authentication is
//!   computer_use_sessions::auth (as google-token), with its own replay/rate
//!   guards. The body cannot assert an employee or a person.
//! - Authorization: Bearer ddc_member_…: {tool, have_rev?}. Live key scopes
//!   intersect the current allow-list; only the holder's personal credential
//!   is eligible. No employee configuration or company credential is read.
//!
//! Person-key permission mapping: acknowledged company follow mode -> follow;
//! otherwise reads -> read_only and writes -> refusal. There is deliberately
//! no employee engine table for a person key. MCP must still enforce Odoo
//! account ACLs, always-refuse rules and F1–F5, as well as the key's scopes.
//!
//! 200: {ok:true,identity,permission,user_id?,odoo_uid?,login?,rev?,secret?}.
//! Only personal responses carry metadata, and secret appears only when the
//! caller's have_rev differs. Refusals: {ok:false,code,message}; 400 malformed
//! body/tool, 401 authentication, 403 policy/not-connected/scope, 429 rate,
//! 503 unavailable storage/crypto/audit. Invalid access policy refuses all tools.
//! Employee decisions use the existing locked tool_calls.jsonl writer, with
//! agent_id attribution; person keys use a fallible security audit append.

pub(crate) mod route;
mod resolve;
pub(crate) mod server;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug)]
pub(super) struct Refusal {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl Refusal {
    fn new(status: StatusCode, code: &'static str, message: &str) -> Self {
        Self { status, code, message: message.to_owned() }
    }
    fn bad_request() -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", "Odoo 身分請求格式不正確或工具不支援。")
    }
    fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "no_verified_caller", "無法驗證這個 Odoo 請求的呼叫者。")
    }
    fn unavailable() -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "odoo_identity_unavailable", "目前無法處理 Odoo 身分請求，請稍後再試。")
    }
    fn access_invalid() -> Self {
        Self::new(StatusCode::FORBIDDEN, "odoo_access_invalid", "Odoo 權限設定讀不到或格式有誤，請管理員檢查設定後再試。")
    }
    fn not_connected(person: bool) -> Self {
        Self::new(StatusCode::FORBIDDEN, "odoo_not_connected",
            duduclaw_odoo::access_policy::not_connected_message(person))
    }
    fn member_credentials_off() -> Self {
        Self::new(StatusCode::FORBIDDEN, "odoo_member_credentials_off", server::MEMBER_CREDENTIALS_OFF)
    }
    fn scope() -> Self {
        Self::new(StatusCode::FORBIDDEN, "person_key_scope", "這把本人金鑰沒有這項 Odoo 操作的範圍。")
    }
    fn authority(error: String) -> Self {
        match error.as_str() {
            "member key invalid" | "member keys disabled" => Self::unauthorized(),
            "member_key_rate_limited" => Self::rate_limited(),
            _ => Self::unavailable(),
        }
    }
    fn rate_limited() -> Self {
        Self::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "Odoo 身分請求太頻繁，請稍後再試。")
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        (self.status, Json(json!({"ok":false,"code":self.code,"message":self.message}))).into_response()
    }
}

#[cfg(test)]
mod tests;
