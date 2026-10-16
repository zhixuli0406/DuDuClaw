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
//! - `config.raw.set` on `config.toml` when the submitted file adds an
//!   `[[accounts]]` entry with `type = "oauth"` that the stored file does not
//!   have (new id, or an existing id switched to `oauth`). Wired in
//!   `handlers/config_raw_rpc.rs`; editing other sections, or an oauth
//!   account that is already there, is not asked again.
//!
//! The gate runs at the dispatch arm, not inside the handlers, so internal
//! callers (`auth.cli_login.finalize` and `accounts.setup_token_submit` reuse
//! `handle_accounts_add`) are not asked twice: the session they finish was
//! already opened through an acknowledged `start`.
//!
//! Every accepted call writes one `subscription_risk_accepted` row to
//! `security_audit.jsonl`. The row records the acknowledgment, not a
//! successful login: a later step (CLI missing, bad token) can still fail.
//! The row says so in `outcome` / `note`.
//!
//! Policy wording and the vendor terms URL per runtime / provider live in
//! `duduclaw_core::subscription_risk::vendor_policy`; the refusal message
//! and the audit row's `policy_url` use the vendor of the call (Codex gets
//! OpenAI's terms, Gemini gets Google's, and so on).

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
    ConfigRawSet,
}

impl Surface {
    pub fn method(self) -> &'static str {
        match self {
            Self::CliLoginStart => "auth.cli_login.start",
            Self::SetupTokenStart => "accounts.setup_token_start",
            Self::AccountsAddOauth => "accounts.add",
            Self::ConfigRawSet => "config.raw.set",
        }
    }
}

/// `true` only for an explicit JSON boolean `true`. A string `"true"`, `1`
/// or a missing key all count as not accepted (fail closed).
pub fn accepted(params: &Value) -> bool {
    params.get(ACCEPT_PARAM).and_then(Value::as_bool) == Some(true)
}

/// `outcome` value of every acceptance row: the user acknowledged the
/// notice. It does not mean the login or the save succeeded.
pub const AUDIT_OUTCOME: &str = "acknowledged";

/// Structured refusal: `{code, message, param, provider, policy_url}`. The
/// message carries the terms of `provider` (a runtime id or an `[[accounts]]`
/// provider; see `duduclaw_core::subscription_risk::vendor_policy`).
pub fn refusal_frame(provider: &str) -> WsFrame {
    let policy = duduclaw_core::subscription_risk::vendor_policy(provider);
    WsFrame::Response {
        id: String::new(),
        ok: false,
        payload: None,
        error: Some(json!({
            "code": NOT_ACCEPTED_CODE,
            "param": ACCEPT_PARAM,
            "provider": provider,
            "policy_url": policy.url,
            "message": format!(
                "需要先閱讀並勾選訂閱帳號登入的風險說明才能繼續（請求缺少 {ACCEPT_PARAM}: true）。{}",
                policy.line_zh()
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
            "policy_url": duduclaw_core::subscription_risk::vendor_policy(provider).url,
            "outcome": AUDIT_OUTCOME,
            "note": "risk notice acknowledged; not a record of a successful login or save",
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
        return Err(refusal_frame(provider));
    }
    record_acceptance(home_dir, surface, provider, account_id, ctx);
    Ok(())
}

/// One `[[accounts]]` entry with `type = "oauth"` that a proposed
/// `config.toml` adds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewOauthAccount {
    pub id: Option<String>,
    /// `provider` of the entry; an entry without one is Anthropic (the
    /// rotator's default).
    pub provider: String,
}

fn oauth_entries(table: &toml::Table) -> Vec<NewOauthAccount> {
    table
        .get("accounts")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a.as_table())
                .filter(|t| t.get("type").and_then(|v| v.as_str()) == Some("oauth"))
                .map(|t| NewOauthAccount {
                    id: t.get("id").and_then(|v| v.as_str()).map(str::to_string),
                    provider: t
                        .get("provider")
                        .and_then(|v| v.as_str())
                        .unwrap_or("anthropic")
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Subscription (oauth) accounts present in `after` but not in `before`,
/// matched by `id` (an existing api-key id switched to `oauth` counts as new;
/// so does every extra id-less oauth entry). A stored file that did not parse
/// arrives here as an empty `before`, so every oauth entry counts: the gate
/// asks rather than guesses (fail closed).
pub fn new_oauth_accounts(before: &toml::Table, after: &toml::Table) -> Vec<NewOauthAccount> {
    let mut remaining: Vec<Option<String>> = oauth_entries(before).into_iter().map(|e| e.id).collect();
    let mut added = Vec::new();
    for entry in oauth_entries(after) {
        if let Some(pos) = remaining.iter().position(|id| *id == entry.id) {
            remaining.swap_remove(pos);
        } else {
            added.push(entry);
        }
    }
    added
}

/// [`require_acceptance`] for a raw `config.toml` write that adds `added`.
/// Nothing added: no check, no audit row. Otherwise the call must carry the
/// flag, and each new account gets its own acceptance row. The refusal names
/// the first new account's vendor.
pub fn require_acceptance_for_new_accounts(
    home_dir: &Path,
    params: &Value,
    ctx: &UserContext,
    added: &[NewOauthAccount],
) -> Result<(), WsFrame> {
    let Some(first) = added.first() else {
        return Ok(());
    };
    if !accepted(params) {
        tracing::info!(
            method = Surface::ConfigRawSet.method(),
            provider = %first.provider,
            new_accounts = added.len(),
            actor = %ctx.user_id,
            "raw config write refused: adds a subscription account without the risk acknowledgment"
        );
        return Err(refusal_frame(&first.provider));
    }
    for a in added {
        record_acceptance(home_dir, Surface::ConfigRawSet, &a.provider, a.id.as_deref(), ctx);
    }
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
        match refusal_frame("claude") {
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

    /// Non-Anthropic runtimes get their own vendor's terms, not Anthropic's.
    #[test]
    fn refusal_names_the_vendor_of_the_call() {
        use duduclaw_core::subscription_risk as sr;
        for (provider, url) in [
            ("codex", sr::OPENAI_SIWC_TERMS_URL),
            ("gemini", sr::GEMINI_CLI_TERMS_URL),
            ("antigravity", sr::ANTIGRAVITY_TERMS_URL),
            ("grok", sr::XAI_TERMS_URL),
        ] {
            let WsFrame::Response { error: Some(err), .. } = refusal_frame(provider) else {
                panic!("expected an error frame");
            };
            assert_eq!(err["policy_url"], url, "{provider}");
            let msg = err["message"].as_str().unwrap();
            assert!(msg.contains(url), "{provider}: {msg}");
            assert!(!msg.contains(sr::ANTHROPIC_LEGAL_URL), "{provider}: {msg}");
            assert!(!msg.contains("Anthropic"), "{provider}: {msg}");
        }
    }

    #[test]
    fn audit_row_carries_the_vendor_terms_url() {
        let home = tempfile::tempdir().unwrap();
        require_acceptance(
            home.path(),
            &json!({ ACCEPT_PARAM: true }),
            &UserContext::admin_fallback(),
            Surface::CliLoginStart,
            "codex",
            None,
        )
        .unwrap();
        let rows = duduclaw_security::audit::read_recent_events(home.path(), 10);
        assert_eq!(
            rows[0].details["policy_url"],
            duduclaw_core::subscription_risk::OPENAI_SIWC_TERMS_URL
        );
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
        assert_eq!(row.details["outcome"], AUDIT_OUTCOME);
        assert_eq!(
            row.details["policy_url"],
            duduclaw_core::subscription_risk::ANTHROPIC_LEGAL_URL
        );
        assert!(!row.timestamp.is_empty());
    }

    fn t(s: &str) -> toml::Table {
        s.parse().unwrap()
    }

    #[test]
    fn new_oauth_accounts_are_found_by_id() {
        let before = t("[[accounts]]\nid='a'\ntype='oauth'\n[[accounts]]\nid='k'\ntype='api_key'\n");
        // Unchanged file, reordered entries, and edits to other keys: nothing new.
        assert!(new_oauth_accounts(&before, &before).is_empty());
        let reordered = t("[[accounts]]\nid='k'\ntype='api_key'\n[[accounts]]\nid='a'\ntype='oauth'\npriority=9\n");
        assert!(new_oauth_accounts(&before, &reordered).is_empty());
        // Removing an oauth account is not an addition.
        assert!(new_oauth_accounts(&before, &t("")).is_empty());
        // A new oauth id, with and without a provider.
        let added = new_oauth_accounts(
            &before,
            &t("[[accounts]]\nid='a'\ntype='oauth'\n[[accounts]]\nid='b'\ntype='oauth'\nprovider='openai'\n[[accounts]]\nid='c'\ntype='oauth'\n"),
        );
        assert_eq!(
            added,
            vec![
                NewOauthAccount { id: Some("b".into()), provider: "openai".into() },
                NewOauthAccount { id: Some("c".into()), provider: "anthropic".into() },
            ]
        );
        // An api-key account switched to oauth counts.
        let switched = new_oauth_accounts(&before, &t("[[accounts]]\nid='a'\ntype='oauth'\n[[accounts]]\nid='k'\ntype='oauth'\n"));
        assert_eq!(switched.len(), 1);
        assert_eq!(switched[0].id.as_deref(), Some("k"));
        // An extra id-less oauth entry counts; an existing one does not.
        let one = t("[[accounts]]\ntype='oauth'\n");
        assert!(new_oauth_accounts(&one, &one).is_empty());
        assert_eq!(new_oauth_accounts(&one, &t("[[accounts]]\ntype='oauth'\n[[accounts]]\ntype='oauth'\n")).len(), 1);
    }

    #[test]
    fn raw_write_gate_audits_each_new_account() {
        let home = tempfile::tempdir().unwrap();
        let ctx = UserContext::admin_fallback();
        let added = vec![
            NewOauthAccount { id: Some("x".into()), provider: "anthropic".into() },
            NewOauthAccount { id: Some("y".into()), provider: "openai".into() },
        ];
        assert!(require_acceptance_for_new_accounts(home.path(), &json!({}), &ctx, &added).is_err());
        assert!(duduclaw_security::audit::read_recent_events(home.path(), 10).is_empty());
        require_acceptance_for_new_accounts(home.path(), &json!({ ACCEPT_PARAM: true }), &ctx, &added).unwrap();
        let rows = duduclaw_security::audit::read_recent_events(home.path(), 10);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.details["method"] == "config.raw.set"));
        // Nothing added: no flag needed, no row.
        require_acceptance_for_new_accounts(home.path(), &json!({}), &ctx, &[]).unwrap();
        assert_eq!(duduclaw_security::audit::read_recent_events(home.path(), 10).len(), 2);
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
