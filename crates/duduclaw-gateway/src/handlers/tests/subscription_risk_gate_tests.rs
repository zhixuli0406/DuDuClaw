//! Subscription-risk acknowledgment gate, driven through the real RPC
//! dispatch (`MethodHandler::handle`) so the arm wiring in
//! `dispatch_knowledge.rs` is what gets tested, not only the helper.
//!
//! "Proceeds" is proven without spawning a real CLI or touching the network:
//! the call must get past the gate and fail (or succeed) on a later,
//! handler-owned check, and the acknowledgment must land in the audit log.
use super::*;
use crate::subscription_risk_gate::{ACCEPT_PARAM, AUDIT_EVENT_TYPE, NOT_ACCEPTED_CODE};

fn error_code(frame: &WsFrame) -> Option<String> {
    match frame {
        WsFrame::Response {
            ok: false,
            error: Some(err),
            ..
        } => err.get("code").and_then(|c| c.as_str()).map(str::to_string),
        _ => None,
    }
}

fn error_text(frame: &WsFrame) -> String {
    match frame {
        WsFrame::Response {
            error: Some(err), ..
        } => err.to_string(),
        other => format!("{other:?}"),
    }
}

fn acceptance_rows(home: &std::path::Path) -> Vec<duduclaw_security::audit::AuditEvent> {
    duduclaw_security::audit::read_recent_events(home, 100)
        .into_iter()
        .filter(|e| e.event_type == AUDIT_EVENT_TYPE)
        .collect()
}

fn assert_refused(frame: &WsFrame) {
    assert_eq!(
        error_code(frame).as_deref(),
        Some(NOT_ACCEPTED_CODE),
        "expected the subscription-risk refusal, got {}",
        error_text(frame)
    );
}

// ── accounts.add ─────────────────────────────────────────────

#[tokio::test]
async fn accounts_add_oauth_without_flag_is_refused_and_writes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext::admin_fallback();
    for extra in [json!(null), json!(false), json!("true")] {
        let mut params = json!({
            "id": "sub-1",
            "type": "oauth",
            "provider": "openai",
            "key": "oauth-secret",
            "priority": 1,
            "monthly_budget_cents": 0,
        });
        if !extra.is_null() {
            params[ACCEPT_PARAM] = extra.clone();
        }
        let res = handler.handle("accounts.add", params, &ctx).await;
        assert_refused(&res);
    }
    assert!(
        !home.path().join("config.toml").exists(),
        "a refused call must not write config"
    );
    assert!(acceptance_rows(home.path()).is_empty());
}

#[tokio::test]
async fn accounts_add_oauth_with_flag_saves_and_is_audited() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext::admin_fallback();
    // `openai` has no write-time probe, so this succeeds offline.
    let res = handler
        .handle(
            "accounts.add",
            json!({
                "id": "sub-ok",
                "type": "oauth",
                "provider": "openai",
                "key": "oauth-secret",
                "priority": 1,
                "monthly_budget_cents": 0,
                ACCEPT_PARAM: true,
            }),
            &ctx,
        )
        .await;
    assert!(
        matches!(res, WsFrame::Response { ok: true, .. }),
        "{}",
        error_text(&res)
    );
    let config = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(config.contains("sub-ok"), "{config}");

    let rows = acceptance_rows(home.path());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].details["method"], "accounts.add");
    assert_eq!(rows[0].details["provider"], "openai");
    assert_eq!(rows[0].details["account_id"], "sub-ok");
    assert_eq!(rows[0].details["actor_email"], ctx.email);
    assert_eq!(rows[0].agent_id, ctx.user_id);
}

/// An Anthropic oauth paste gets past the gate and reaches the handler's own
/// short-lived-token check (no network call for an `at01` token).
#[tokio::test]
async fn accounts_add_anthropic_oauth_with_flag_reaches_the_handler() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let res = handler
        .handle(
            "accounts.add",
            json!({
                "id": "sub-at01",
                "type": "oauth",
                "key": "sk-ant-at01-short-lived",
                "priority": 1,
                "monthly_budget_cents": 0,
                ACCEPT_PARAM: true,
            }),
            &UserContext::admin_fallback(),
        )
        .await;
    assert_ne!(error_code(&res).as_deref(), Some(NOT_ACCEPTED_CODE));
    assert!(error_text(&res).contains("setup-token"), "{}", error_text(&res));
    assert_eq!(acceptance_rows(home.path()).len(), 1);
}

/// API-key accounts are not a subscription credential: no flag needed, no
/// acknowledgment row.
#[tokio::test]
async fn accounts_add_api_key_is_not_gated() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let res = handler
        .handle(
            "accounts.add",
            json!({
                "id": "key-1",
                "type": "api_key",
                "provider": "openai",
                "key": "sk-test",
                "priority": 1,
                "monthly_budget_cents": 0,
            }),
            &UserContext::admin_fallback(),
        )
        .await;
    assert!(
        matches!(res, WsFrame::Response { ok: true, .. }),
        "{}",
        error_text(&res)
    );
    assert!(acceptance_rows(home.path()).is_empty());
}

// ── auth.cli_login.start ─────────────────────────────────────

#[tokio::test]
async fn cli_login_start_without_flag_is_refused_for_every_login_runtime() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    for runtime in ["claude", "codex", "gemini", "grok"] {
        let res = handler
            .handle(
                "auth.cli_login.start",
                json!({ "runtime": runtime }),
                &UserContext::admin_fallback(),
            )
            .await;
        assert_refused(&res);
    }
    assert!(handler.cli_auth_sessions.read().await.is_empty());
    assert!(acceptance_rows(home.path()).is_empty());
}

/// A runtime with no scriptable login keeps its own, more useful error.
#[tokio::test]
async fn cli_login_start_unknown_runtime_keeps_its_own_error() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let res = handler
        .handle(
            "auth.cli_login.start",
            json!({ "runtime": "kimi-not-a-runtime" }),
            &UserContext::admin_fallback(),
        )
        .await;
    assert_ne!(error_code(&res).as_deref(), Some(NOT_ACCEPTED_CODE));
    assert!(error_text(&res).contains("unknown runtime"), "{}", error_text(&res));
}

/// With the flag the call gets past the gate. A restricted product profile
/// then refuses `claude` inside the handler, so no CLI is spawned.
#[tokio::test]
async fn cli_login_start_with_flag_reaches_the_handler_and_is_audited() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "[product]\nprofile='restricted'\n",
    )
    .unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let res = handler
        .handle(
            "auth.cli_login.start",
            json!({ "runtime": "claude", ACCEPT_PARAM: true }),
            &UserContext::admin_fallback(),
        )
        .await;
    assert_ne!(error_code(&res).as_deref(), Some(NOT_ACCEPTED_CODE));
    let text = error_text(&res);
    assert!(
        text.contains("Product profile") || text.contains("product.profile"),
        "{text}"
    );
    let rows = acceptance_rows(home.path());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].details["method"], "auth.cli_login.start");
    assert_eq!(rows[0].details["provider"], "claude");
}

// ── accounts.setup_token_start ───────────────────────────────

/// Only the refusal is driven end to end here: with the flag, the handler
/// spawns the real `claude setup-token` on any machine that has Claude Code
/// installed. The accepted path is the same `require_acceptance` call
/// covered in `subscription_risk_gate::tests`.
#[tokio::test]
async fn setup_token_start_without_flag_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let res = handler
        .handle(
            "accounts.setup_token_start",
            json!({}),
            &UserContext::admin_fallback(),
        )
        .await;
    assert_refused(&res);
    assert!(handler.setup_token_session.read().await.is_none());
    assert!(acceptance_rows(home.path()).is_empty());
}

// ── config.raw.set ───────────────────────────────────────────

async fn raw_hash(handler: &MethodHandler) -> Value {
    match handler
        .handle("config.raw.get", json!({ "file": "config" }), &UserContext::admin_fallback())
        .await
    {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => p["hash"].clone(),
        other => panic!("config.raw.get failed: {other:?}"),
    }
}

/// The raw editor is the other way to write an `[[accounts]]` entry the
/// rotator reads. Adding an oauth one needs the same acknowledgment; the
/// refusal writes nothing and names the entry's vendor.
#[tokio::test]
async fn raw_config_adding_an_oauth_account_needs_the_flag() {
    let home = tempfile::tempdir().unwrap();
    let base = "[[accounts]]\nid = \"key\"\ntype = \"api_key\"\nprovider = \"openai\"\n";
    std::fs::write(home.path().join("config.toml"), base).unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext::admin_fallback();
    let edited = format!("{base}\n[[accounts]]\nid = \"sub\"\ntype = \"oauth\"\nprovider = \"openai\"\n");

    let hash = raw_hash(&handler).await;
    let res = handler
        .handle(
            "config.raw.set",
            json!({ "file": "config", "content": edited, "base_hash": hash }),
            &ctx,
        )
        .await;
    assert_refused(&res);
    assert!(
        error_text(&res).contains(duduclaw_core::subscription_risk::OPENAI_SIWC_TERMS_URL),
        "{}",
        error_text(&res)
    );
    assert_eq!(
        std::fs::read_to_string(home.path().join("config.toml")).unwrap(),
        base,
        "a refused raw write must not touch config.toml"
    );
    assert!(acceptance_rows(home.path()).is_empty());

    let res = handler
        .handle(
            "config.raw.set",
            json!({ "file": "config", "content": edited, "base_hash": hash, ACCEPT_PARAM: true }),
            &ctx,
        )
        .await;
    assert!(
        matches!(res, WsFrame::Response { ok: true, .. }),
        "{}",
        error_text(&res)
    );
    assert!(std::fs::read_to_string(home.path().join("config.toml")).unwrap().contains("sub"));
    let rows = acceptance_rows(home.path());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].details["method"], "config.raw.set");
    assert_eq!(rows[0].details["account_id"], "sub");
    assert_eq!(rows[0].details["provider"], "openai");

    // Editing the file again with the oauth account already there is not
    // asked again.
    let hash = raw_hash(&handler).await;
    let current = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    let res = handler
        .handle(
            "config.raw.set",
            json!({ "file": "config", "content": format!("{current}\n[general]\nlog_level = \"info\"\n"), "base_hash": hash }),
            &ctx,
        )
        .await;
    assert!(
        matches!(res, WsFrame::Response { ok: true, .. }),
        "{}",
        error_text(&res)
    );
    assert_eq!(acceptance_rows(home.path()).len(), 1);
}
