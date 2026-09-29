//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

/// G2 — the Enterprise-only RPC table, tested as a pure function (no env, no
/// tempdir, no races with the tests that mutate `DUDUCLAW_EDITION`). The
/// end-to-end behaviour through `dispatch` lives in
/// `tests/edition_rpc_gate_test.rs`, which runs in its own process.
use super::*;

/// Every ➖ (Personal-hidden) unit in 09-edition-split-features.md §1 that
/// owns a dashboard RPC is gated.
#[test]
fn enterprise_surfaces_are_listed() {
    for method in [
        // 成員（使用者管理）
        "users.list",
        "users.create",
        "users.update",
        "users.remove",
        "users.bind_agent",
        "users.unbind_agent",
        "users.offboard",
        "users.subordinates",
        "users.audit_log",
        // 部門
        "departments.list",
        "departments.create",
        "departments.remove",
        // 經銷商管理／發授權／白牌品牌
        "distributor.status",
        "distributor.list",
        "distributor.add",
        "distributor.update",
        "distributor.remove",
        "distributor.issue",
        "distributor.revoke",
        "distributor.upgrade",
        "distributor.bundle.sign",
        // 夥伴入口（經銷 CRM）
        "partner.profile",
        "partner.profile.update",
        "partner.stats",
        "partner.customers",
        "partner.customer.add",
        "partner.customer.update",
        "partner.customer.delete",
        // 身分解析
        "identity.config_get",
        "identity.config_set",
        "identity.resolve",
        // Wiki Trust 稽核
        "wiki.trust_audit",
        "wiki.trust_history",
        "wiki.trust_override",
        // 可靠性報告
        "audit.reliability_summary",
    ] {
        assert!(
            is_enterprise_only_method(method),
            "{method} must be Enterprise-only"
        );
    }
}

/// The far more dangerous direction: anything a single-person install
/// legitimately uses must NOT be caught. A false positive here is a
/// regression that silently breaks the free edition.
#[test]
fn personal_surfaces_are_untouched() {
    for method in [
        // Self-service account management (§1.6 帳號與密碼 ✅)
        "users.me",
        "users.change_password",
        // 委派權限 — 跨 agent 不是跨人 (§1.6)
        "delegation.get",
        "delegation.set",
        // 授權：升級路徑本身，擋掉就再也升不了級
        "license.status",
        "license.fingerprint",
        "license.activate",
        "license.redeem",
        // 審批 / 稽核 / 日誌 / kill switch (D9)
        "approvals.list",
        "approvals.decide",
        "audit.unified_log",
        "audit.evolution_query",
        "security.status",
        "security.audit_log",
        "security.credential_hygiene",
        "security.credential_inventory",
        "security.credential_cleanup",
        "killswitch.get",
        "killswitch.update",
        // 帳務：同一 RPC 服務個人版預算檢視，分區隱藏在前端
        "billing.usage",
        // 共享知識庫 / namespace 政策 (D5)
        "wiki.pages",
        "wiki.read",
        "wiki.search",
        "wiki.share",
        "wiki.stats",
        "wiki.lint",
        "wiki.promote",
        "wiki.archive",
        "wiki.auto_pages",
        "wiki_scope.get",
        "wiki_scope.update",
        // 組織架構 — D6 改為漸進揭露，兩版皆可讀
        "topology.list",
        // 日常主軌
        "agents.list",
        "agents.create",
        "agents.update",
        "tasks.list",
        "plans.list",
        "evolution.status",
        "autopilot.list",
        "channels.add",
        "system.status",
        "connect",
        "ping",
    ] {
        assert!(
            !is_enterprise_only_method(method),
            "{method} must stay open in the Personal edition"
        );
    }
}

/// Family matching is exact first-segment equality, not `starts_with`
/// (coding convention #2). A method that merely *begins* with the letters
/// of a gated family is a different method and must not be captured.
#[test]
fn family_match_is_anchored_on_the_whole_segment() {
    for method in [
        "users_export.run",
        "identityx.resolve",
        "partnership.list",
        "departmental.list",
        "distributors_legacy.list",
    ] {
        assert!(
            !is_enterprise_only_method(method),
            "{method} shares only a prefix — must not be gated"
        );
    }
    // …while a genuinely new member of a gated family is fail-closed
    // (Enterprise) without anyone editing the table.
    assert!(is_enterprise_only_method("departments.rename"));
    assert!(is_enterprise_only_method("users.reset_password_for"));
}

/// The refusal is machine-readable and its copy is end-user-facing: no
/// method name, route path or other internal term leaks into the UI.
#[test]
fn reject_frame_is_coded_and_leak_free() {
    match enterprise_only_reject_frame() {
        WsFrame::Response {
            ok: false,
            error: Some(err),
            ..
        } => {
            assert_eq!(
                err.get("code").and_then(|v| v.as_str()),
                Some(ENTERPRISE_ONLY_ERROR_CODE)
            );
            let msg = err.get("message").and_then(|v| v.as_str()).unwrap();
            assert!(msg.contains("多人團隊版"), "plain-language copy: {msg}");
            for leak in ["users.", "RPC", "edition", "enterprise", "/manage/"] {
                assert!(!msg.contains(leak), "internal term leaked: {leak}");
            }
        }
        other => panic!("expected structured error response, got {other:?}"),
    }
}
