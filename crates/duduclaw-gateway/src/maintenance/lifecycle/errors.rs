//! Refusals with stable codes; the dashboard branches on `code` and shows
//! `message` (zh-TW).

use serde_json::{Map, Value, json};
use tracing::error;

// ── Errors with stable codes (the dashboard branches on `code`) ──

/// Stable `code` values of a refused `maintenance.enable` / `disable`.
pub mod error_code {
    pub const CONFIRM_TEXT_MISMATCH: &str = "confirm_text_mismatch";
    pub const USER_SYSTEM_UNAVAILABLE: &str = "user_system_unavailable";
    pub const GATEWAY_NOT_OWNER: &str = "gateway_not_owner";
    pub const INVALID_REQUEST: &str = "invalid_request";
    pub const SSH_KEY_REQUIRED: &str = "ssh_key_required";
    pub const SSH_KEY_UNEXPECTED: &str = "ssh_key_unexpected";
    /// The key failed `ssh_key::parse`; `reason` carries its code.
    pub const SSH_KEY_INVALID: &str = "ssh_key_invalid";
    /// Wrong password; `remaining_attempts` before the lock.
    pub const REAUTH_FAILED: &str = "reauth_failed";
    /// Locked; `retry_after_seconds`.
    pub const REAUTH_LOCKED: &str = "reauth_locked";
    pub const ALREADY_ACTIVE: &str = "already_active";
    /// This device cannot open a keyed SSH window (old OS image, old sysd,
    /// partial OS update, not an appliance). Never answered with a port-only
    /// open.
    pub const SSH_UNSUPPORTED: &str = "ssh_unsupported";
    pub const SSH_UNREACHABLE: &str = "ssh_unreachable";
    /// sysd still holds a key, is finishing a cleanup, or still owes its
    /// audit file a record; `reason` is `key_installed`, `cleanup_pending`,
    /// `audit_pending` or `unknown`.
    pub const SSH_BUSY: &str = "ssh_busy";
    /// sysd's own open limit (at least 5 s apart, at most 12 per hour).
    pub const SSH_RATE_LIMITED: &str = "ssh_rate_limited";
    pub const SSH_OPEN_FAILED: &str = "ssh_open_failed";
    pub const INTERNAL: &str = "internal";
}

/// A refusal with a stable code, a zh-TW sentence, and optional fields.
#[derive(Debug, Clone, PartialEq)]
pub struct MaintError {
    pub code: &'static str,
    pub message: String,
    pub extra: Map<String, Value>,
}

impl MaintError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            extra: Map::new(),
        }
    }

    pub(super) fn with(mut self, key: &str, value: Value) -> Self {
        self.extra.insert(key.to_string(), value);
        self
    }

    pub(super) fn internal(detail: impl Into<String>) -> Self {
        let detail = detail.into();
        // Logged here, not returned: the text can carry paths or SQLite errors.
        error!(error = %detail, "maintenance: internal error");
        Self::new(error_code::INTERNAL, "維修模式暫時無法操作，請稍後再試。")
    }

    /// `{"code", "message", ...extra}` for the RPC error frame.
    pub fn to_json(&self) -> Value {
        let mut obj = self.extra.clone();
        obj.insert("code".to_string(), json!(self.code));
        obj.insert("message".to_string(), json!(self.message));
        Value::Object(obj)
    }
}

impl std::fmt::Display for MaintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

pub(super) fn locked_error(retry_after_seconds: i64) -> MaintError {
    let minutes = (retry_after_seconds.max(1) + 59) / 60;
    MaintError::new(
        error_code::REAUTH_LOCKED,
        format!("密碼錯誤次數過多，{minutes} 分鐘後才能再開啟維修模式。"),
    )
    .with("retry_after_seconds", json!(retry_after_seconds.max(1)))
}

pub(super) fn key_error(code: &str) -> MaintError {
    MaintError::new(
        error_code::SSH_KEY_INVALID,
        "這把公開金鑰無法使用，請確認貼上的是一整行公開金鑰。",
    )
    .with("reason", json!(code))
}

pub(super) fn unsupported_error(reason: &str) -> MaintError {
    MaintError::new(
        error_code::SSH_UNSUPPORTED,
        "這台裝置的系統版本還不支援遠端登入，請先更新系統。",
    )
    .with("reason", json!(reason))
}

pub(super) fn unreachable_error() -> MaintError {
    MaintError::new(
        error_code::SSH_UNREACHABLE,
        "裝置的系統服務沒有回應，無法開啟遠端登入，請稍後再試。",
    )
}
