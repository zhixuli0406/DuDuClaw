//! G2 — server-side edition gate on the dashboard WebSocket RPC surface.
//!
//! The frontend `EditionGuard` / `nav-visibility` flags are fail-open UX only:
//! a Personal-edition user who edits the URL, follows a legacy route alias, or
//! speaks the WebSocket JSON-RPC protocol directly still reached every
//! Enterprise management surface. `MethodHandler::dispatch` now refuses those
//! methods before the method match.
//!
//! These live in an integration test (own process) rather than in
//! `handlers.rs`'s unit tests on purpose: edition resolution reads the
//! `DUDUCLAW_EDITION` process env, and several unit tests in that crate mutate
//! it. A separate process removes that race entirely.

use duduclaw_auth::UserContext;
use duduclaw_core::EditionProfile;
use duduclaw_gateway::handlers::MethodHandler;
use duduclaw_gateway::protocol::WsFrame;
use serde_json::json;

/// Stable code the dashboard keys its upgrade copy off.
const ENTERPRISE_ONLY_CODE: &str = "enterprise_edition_required";

/// Every Enterprise-only method that owns a `➖` row in
/// `09-edition-split-features.md` §1.
const ENTERPRISE_METHODS: &[&str] = &[
    "users.list",
    "users.create",
    "users.update",
    "users.remove",
    "users.bind_agent",
    "users.unbind_agent",
    "users.offboard",
    "users.subordinates",
    "users.audit_log",
    "departments.list",
    "departments.create",
    "departments.remove",
    "governance.list",
    "governance.upsert",
    "governance.remove",
    "distributor.status",
    "distributor.list",
    "distributor.add",
    "distributor.update",
    "distributor.remove",
    "distributor.issue",
    "distributor.revoke",
    "distributor.upgrade",
    "distributor.bundle.sign",
    "partner.profile",
    "partner.profile.update",
    "partner.stats",
    "partner.customers",
    "partner.customer.add",
    "partner.customer.update",
    "partner.customer.delete",
    "identity.config_get",
    "identity.config_set",
    "identity.resolve",
    "wiki.trust_audit",
    "wiki.trust_history",
    "wiki.trust_override",
    "audit.reliability_summary",
];

fn error_code(frame: &WsFrame) -> Option<String> {
    match frame {
        WsFrame::Response { error: Some(e), .. } => {
            e.get("code").and_then(|c| c.as_str()).map(String::from)
        }
        _ => None,
    }
}

async fn handler_with_edition(
    home: &std::path::Path,
    edition: EditionProfile,
) -> MethodHandler {
    let handler = MethodHandler::new(home.to_path_buf()).await;
    handler.set_edition_override(Some(edition)).await;
    handler
}

/// Personal edition + Enterprise method ⇒ refused with the shared code, and
/// the caller being a full admin does not help (this is a form-factor gate,
/// not a role gate — the single Personal user always *is* the admin).
#[tokio::test]
async fn personal_edition_refuses_every_enterprise_method() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_edition(home.path(), EditionProfile::Personal).await;
    let ctx = UserContext::admin_fallback();

    for method in ENTERPRISE_METHODS {
        let frame = handler.handle(method, json!({}), &ctx).await;
        assert_eq!(
            error_code(&frame).as_deref(),
            Some(ENTERPRISE_ONLY_CODE),
            "{method} must be refused in the Personal edition, got {frame:?}"
        );
    }
}

/// Enterprise edition + the same methods ⇒ the gate is transparent. The calls
/// may still fail for their own reasons (no user DB, no license runtime, empty
/// params) — what must never appear is the edition refusal.
#[tokio::test]
async fn enterprise_edition_passes_the_gate() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_edition(home.path(), EditionProfile::Enterprise).await;
    let ctx = UserContext::admin_fallback();

    for method in ENTERPRISE_METHODS {
        let frame = handler.handle(method, json!({}), &ctx).await;
        assert_ne!(
            error_code(&frame).as_deref(),
            Some(ENTERPRISE_ONLY_CODE),
            "{method} must reach its handler in the Enterprise edition"
        );
    }
}

/// The gate must not touch anything a single-person install uses. `users.me` /
/// `users.change_password` are the interesting pair: they sit inside a gated
/// family but only ever resolve their target from `ctx.user_id`, so they are
/// self-service and stay open.
#[tokio::test]
async fn personal_edition_keeps_ordinary_methods_working() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_edition(home.path(), EditionProfile::Personal).await;
    let ctx = UserContext::admin_fallback();

    for method in [
        // self-service inside a gated family
        "users.me",
        "users.change_password",
        // 委派權限 / 授權升級路徑 / 審批 / 稽核 / kill switch / 帳務 / 共享 wiki
        "delegation.get",
        "license.status",
        "approvals.list",
        "audit.unified_log",
        "security.status",
        "security.credential_hygiene",
        "security.credential_cleanup",
        "killswitch.get",
        "billing.usage",
        "wiki.pages",
        "wiki_scope.get",
        "topology.list",
        // daily rail
        "agents.list",
        "tasks.list",
        "plans.list",
        "evolution.status",
        "autopilot.list",
        "system.status",
        "ping",
    ] {
        let frame = handler.handle(method, json!({}), &ctx).await;
        assert_ne!(
            error_code(&frame).as_deref(),
            Some(ENTERPRISE_ONLY_CODE),
            "{method} must stay reachable in the Personal edition"
        );
    }
}

/// An unknown method is not silently swept into the Enterprise bucket — it
/// still falls through to the dispatcher's own unknown-method handling, so the
/// gate cannot become a catch-all that masks routing bugs.
#[tokio::test]
async fn unknown_method_is_not_captured_by_the_gate() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_edition(home.path(), EditionProfile::Personal).await;
    let ctx = UserContext::admin_fallback();

    let frame = handler
        .handle("definitely.not.a.method", json!({}), &ctx)
        .await;
    assert_ne!(error_code(&frame).as_deref(), Some(ENTERPRISE_ONLY_CODE));
}
