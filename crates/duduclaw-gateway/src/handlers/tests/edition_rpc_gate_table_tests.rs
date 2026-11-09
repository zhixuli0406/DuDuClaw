//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

/// G2 — the Enterprise-only RPC table, tested as a pure function (no env, no
/// tempdir, no races with the tests that mutate `DUDUCLAW_EDITION`). The
/// end-to-end behaviour through `dispatch` lives in
/// `tests/edition_rpc_gate_test.rs`, which runs in its own process.
use super::*;

#[test]
fn odoo_permission_metadata_has_the_schema_scan_edition_gate() {
    for method in ["odoo.schema_scan", "odoo.permission_options"] {
        assert!(!is_enterprise_only_method(method));
    }
}

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
        "users.my_team",
        "users.my_history_views",
        "users.audit_log",
        // 部門
        "departments.list",
        "departments.create",
        "departments.remove",
        // 總經銷授權交付與管理
        "distributor.status",
        "distributor.list",
        "distributor.add",
        "distributor.update",
        "distributor.remove",
        "distributor.issue",
        "distributor.revoke",
        "distributor.upgrade",
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
        "compliance.generate",
        // 隔離連線帳號（受保護模式＝Linux 團隊版）
        "isolation.connector_list",
        "isolation.connector_step_up",
        "isolation.connector_provision",
        "isolation.connector_revoke",
        "isolation.connector_import_legacy",
        // 受保護員工配置提案（A4）
        "isolation.employee_list",
        "isolation.employee_propose",
        "isolation.employee_tombstone",
        "isolation.employee_purge_requests",
        "me.isolation.request_personal_employee",
        "me.isolation.list_personal_employee_requests",
        // SR2: Personal → Shared delegation owner cards
        "me.isolation.list_personal_delegations",
        "me.isolation.decide_personal_delegation",
        // F6d restore RPCs
        "me.isolation.list_restorable",
        "me.isolation.restore_challenge",
        "me.isolation.request_restore",
        "me.isolation.cancel_restore",
        "isolation.employee_restore_decide",
        "isolation.employee_restore_cancel",
        "isolation.employee_restore_regrant",
        "isolation.employee_retire_propose",
        // F6g adoption request RPCs (F6d surface)
        "me.isolation.request_adoption",
        "isolation.shared_adoption_request",
        // F6fa2 Admin filing for an owner without a second factor
        "isolation.employee_restore_request_for_owner",
        "isolation.adoption_request_for_owner",
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
        // 設定檔編輯 (agent_files.*) — a Personal owner edits their own employees too
        "agent_files.list",
        "agent_files.get",
        "agent_files.runtime_files",
        "agent_files.diff",
        "agent_files.history",
        "agent_files.save",
        "agent_files.rollback",
        "agent_files.withdraw",
        "agent_files.pending_get",
        "contract.get",
        "contract.update",
        // Self-service account management (§1.6 帳號與密碼 ✅)
        "users.me",
        "users.change_password",
        "users.change_email",
        // RM-064 外部查詢憑證 — 個人版擁有者也要能讓 Odoo 查自己的員工
        "me.status_tokens.list",
        "me.status_tokens.create",
        "me.status_tokens.revoke",
        "status_tokens.admin_list",
        "status_tokens.admin_revoke",
        // 委派權限 — 跨 agent 不是跨人 (§1.6)
        "delegation.get",
        "delegation.set",
        // 授權：升級路徑本身，擋掉就再也升不了級
        "license.status",
        "license.fingerprint",
        "license.activate",
        // 審批 / 稽核 / 日誌 / kill switch (D9)
        "approvals.list",
        "approvals.decide",
        "audit.unified_log",
        "audit.evolution_query",
        "security.status",
        "security.egress_shadow_report",
        "security.audit_log",
        "security.credential_hygiene",
        "security.credential_inventory",
        "security.credential_cleanup",
        "security.credential_protection",
        "security.credential_protection_enable",
        "security.credential_protection_disable",
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
        // 組織圖編輯 (RM-027) — 跨 agent，不是跨人
        "org.propose_change",
        "org.apply_change",
        "org.pending_changes",
        "org.placement",
        // 日常主軌
        "agents.list",
        "agents.create",
        "agents.update",
        "agents.permission_history",
        "tasks.list",
        "plans.list",
        "evolution.status",
        "autopilot.list",
        "autopilot.breakers",
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

#[test]
fn mfa_recovery_edition_exceptions_are_exact() {
    for method in ["users.mfa_reset", "users.mfa_enrollment_code"] {
        assert!(!is_enterprise_only_method(method));
        assert!(is_enterprise_only_method(&format!("{method}_extra")));
    }
    assert!(is_enterprise_only_method("users.update"));
}
