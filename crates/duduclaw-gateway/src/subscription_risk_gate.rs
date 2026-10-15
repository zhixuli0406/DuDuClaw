//! Server-side acknowledgment gate for subscription-credential RPCs.
//!
//! Owner decision 2026-10-16: the subscription-login features stay as they
//! are, but nothing may start or save a consumer-subscription credential
//! unless the caller explicitly accepted the risk disclosure. The dashboard's
//! `SubscriptionRiskDisclosure` checkbox was the only guard before; a
//! hand-written WebSocket client (or a future UI that forgets the component)
//! could skip it. This gate moves the requirement to the RPC boundary.
//!
//! Gated methods (wired in `handlers/dispatch_knowledge.rs`):
//!
//! - `auth.cli_login.start` — every runtime with a scriptable login. All of
//!   them sign in a consumer account (Claude setup-token, ChatGPT login for
//!   Codex, Google for Gemini, SuperGrok for Grok).
//! - `accounts.setup_token_start` — the Claude subscription wizard.
//! - `accounts.add` with `type = "oauth"`. API-key accounts are not gated.
//!
//! The gate runs at the dispatch arm, not inside the handlers, so internal
//! callers (`auth.cli_login.finalize` and `accounts.setup_token_submit` reuse
//! `handle_accounts_add`) are not asked twice: the session they finish was
//! already opened through an acknowledged `start`.
//!
//! Every accepted call writes one `subscription_risk_accepted` row to
//! `security_audit.jsonl`. The row records the acknowledgment, not a
//! successful login: a later step (CLI missing, bad token) can still fail.
//!
//! Policy wording lives in `duduclaw_core::subscription_risk`.

use std::path::Path;

use duduclaw_auth::UserContext;
use duduclaw_security::audit::{AuditEvent, Severity};
use serde_json::{Value, json};

use crate::protocol::WsFrame;

/// Request parameter the caller must set to JSON `true`.
pub const ACCEPT_PARAM: &str = "accept_subscription_risk";

/// Stable error code for a refused call. The dashboard maps it to
/// `subscriptionRisk.error.notAccepted` in every locale.
pub const NOT_ACCEPTED_CODE: &str = "subscription_risk_not_accepted";

/// `event_type` of the audit row written for each acknowledgment.
pub const AUDIT_EVENT_TYPE: &str = "subscription_risk_accepted";

/// Which RPC the acknowledgment was given for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    CliLoginStart,
    SetupTokenStart,
    AccountsAddOauth,
}

impl Surface {
    pub fn method(self) -> &'static str {
        match self {
            Self::CliLoginStart => "auth.cli_login.start",
            Self::SetupTokenStart => "accounts.setup_token_start",
            Self::AccountsAddOauth => "accounts.add",
        }
    }
}

/// `true` only for an explicit JSON boolean `true`. A string `"true"`, `1`
/// or a missing key all count as not accepted (fail closed).
pub fn accepted(params: &Value) -> bool {
    params.get(ACCEPT_PARAM).and_then(Value::as_bool) == Some(true)
}

/// Structured refusal: `{code, message, param}`.
pub fn refusal_frame() -> WsFrame {
    WsFrame::Response {
        id: String::new(),
        ok: false,
        payload: None,
        error: Some(json!({
            "code": NOT_ACCEPTED_CODE,
            "param": ACCEPT_PARAM,
            "message": format!(
                "需要先閱讀並勾選訂閱帳號登入的風險說明才能繼續（請求缺少 {ACCEPT_PARAM}: true）。{}",
                duduclaw_core::subscription_risk::subscription_risk_line_zh()
            ),
        })),
    }
}

/// Append the acknowledgment to the security audit log.
pub fn record_acceptance(
    home_dir: &Path,
    surface: Surface,
    provider: &str,
    account_id: Option<&str>,
    ctx: &UserContext,
) {
    let event = AuditEvent::new(
        AUDIT_EVENT_TYPE,
        ctx.user_id.as_str(),
        Severity::Info,
        json!({
            "method": surface.method(),
            "provider": provider,
            "account_id": account_id,
            "actor_email": ctx.email,
            "actor_role": ctx.role.to_string(),
            "policy_url": duduclaw_core::subscription_risk::ANTHROPIC_LEGAL_URL,
        }),
    );
    crate::security_autopilot::audit_and_emit(home_dir, &event);
}

/// Refuse unless `params` carries the acknowledgment; on acceptance, write
/// the audit row and return `Ok(())`.
pub fn require_acceptance(
    home_dir: &Path,
    params: &Value,
    ctx: &UserContext,
    surface: Surface,
    provider: &str,
    account_id: Option<&str>,
) -> Result<(), WsFrame> {
    if !accepted(params) {
        tracing::info!(
            method = surface.method(),
            provider,
            actor = %ctx.user_id,
            "subscription credential call refused: risk disclosure not accepted"
        );
        return Err(refusal_frame());
    }
    record_acceptance(home_dir, surface, provider, account_id, ctx);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_json_true_counts() {
        assert!(accepted(&json!({ ACCEPT_PARAM: true })));
        for v in [
            json!({}),
            json!({ ACCEPT_PARAM: false }),
            json!({ ACCEPT_PARAM: "true" }),
            json!({ ACCEPT_PARAM: 1 }),
            json!({ ACCEPT_PARAM: null }),
        ] {
            assert!(!accepted(&v), "{v}");
        }
    }

    #[test]
    fn refusal_is_coded_and_names_the_param() {
        match refusal_frame() {
            WsFrame::Response {
                ok: false,
                error: Some(err),
                ..
            } => {
                assert_eq!(err["code"], NOT_ACCEPTED_CODE);
                assert_eq!(err["param"], ACCEPT_PARAM);
                let msg = err["message"].as_str().unwrap();
                assert!(msg.contains(duduclaw_core::subscription_risk::ANTHROPIC_LEGAL_URL));
            }
            other => panic!("expected a coded error frame, got {other:?}"),
        }
    }

    #[test]
    fn acceptance_writes_one_audit_row() {
        let home = tempfile::tempdir().unwrap();
        let ctx = UserContext::admin_fallback();
        require_acceptance(
            home.path(),
            &json!({ ACCEPT_PARAM: true }),
            &ctx,
            Surface::AccountsAddOauth,
            "anthropic",
            Some("acct-1"),
        )
        .expect("accepted");
        let rows = duduclaw_security::audit::read_recent_events(home.path(), 10);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.event_type, AUDIT_EVENT_TYPE);
        assert_eq!(row.agent_id, ctx.user_id);
        assert_eq!(row.details["method"], "accounts.add");
        assert_eq!(row.details["provider"], "anthropic");
        assert_eq!(row.details["account_id"], "acct-1");
        assert_eq!(row.details["actor_email"], ctx.email);
        assert!(!row.timestamp.is_empty());
    }

    #[test]
    fn refusal_writes_no_audit_row() {
        let home = tempfile::tempdir().unwrap();
        let err = require_acceptance(
            home.path(),
            &json!({}),
            &UserContext::admin_fallback(),
            Surface::SetupTokenStart,
            "anthropic",
            None,
        );
        assert!(err.is_err());
        assert!(duduclaw_security::audit::read_recent_events(home.path(), 10).is_empty());
    }
}
