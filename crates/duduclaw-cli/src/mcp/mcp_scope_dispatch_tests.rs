//! Validates that the scope enforcement logic in `run_mcp_server` correctly
//! blocks tool calls that require a scope the caller does not hold, and
//! that `admin` is treated as an unconditional pass.
//!
//! Because `run_mcp_server` is an integration boundary (reads stdin), these
//! tests exercise the same building blocks: `tool_requires_scope` from
//! `mcp_auth` and a local `check_scope` helper that mirrors the dispatcher.

use crate::mcp_auth::{Principal, Scope};
use chrono::Utc;
use std::collections::HashSet;

// ── Helpers ──────────────────────────────────────────────────────────────

fn make_principal(scopes: &[Scope]) -> Principal {
    Principal {
        client_id: "test-ext".to_string(),
        scopes: scopes.iter().cloned().collect::<HashSet<_>>(),
        is_external: true,
        created_at: Utc::now(),
    }
}

/// Mirror of the scope check in `run_mcp_server`:
///   if required_scope present AND principal lacks it AND lacks admin → deny.
fn check_scope(principal: &Principal, tool_name: &str) -> bool {
    if let Some(required) = crate::mcp_auth::tool_requires_scope(tool_name) {
        principal.scopes.contains(&required) || principal.scopes.contains(&Scope::Admin)
    } else {
        // No scope required → always allow
        true
    }
}

// ── Test 1: memory_store blocked without memory:write ────────────────────
#[test]
fn memory_store_blocked_without_memory_write_scope() {
    let p = make_principal(&[Scope::MemoryRead, Scope::WikiRead]);
    assert!(
        !check_scope(&p, "memory_store"),
        "memory_store must be blocked when memory:write is absent"
    );
}

// ── Test 2: wiki_write blocked without wiki:write ─────────────────────────
#[test]
fn wiki_write_blocked_without_wiki_write_scope() {
    let p = make_principal(&[Scope::MemoryRead, Scope::WikiRead]);
    assert!(
        !check_scope(&p, "wiki_write"),
        "wiki_write must be blocked when wiki:write is absent"
    );
}

// ── Test 3: admin scope bypasses all restrictions ────────────────────────
#[test]
fn admin_scope_allows_all_restricted_tools() {
    let p = make_principal(&[Scope::Admin]);
    for tool in &["memory_store", "wiki_write", "send_message"] {
        assert!(check_scope(&p, tool), "admin scope must allow '{tool}'");
    }
}

// ── Test 4: unmapped tool is fail-closed (C2) ─────────────────────────────
#[test]
fn unmapped_tool_is_fail_closed_with_empty_scopes() {
    // C2 fail-closed: a tool not in the scope table requires Admin, so a
    // principal with no scopes (and no Admin) must be denied. This replaces
    // the obsolete expectation that unmapped tools were unrestricted.
    let p = make_principal(&[]); // no scopes at all
    assert!(
        !check_scope(&p, "web_search"),
        "unmapped tool must be denied for a scope-less principal (fail-closed)"
    );
    // An Admin principal still passes.
    let admin = make_principal(&[Scope::Admin]);
    assert!(
        check_scope(&admin, "web_search"),
        "Admin must pass any tool"
    );
}

// ── Test 5: memory_store allowed when memory:write present ───────────────
#[test]
fn memory_store_allowed_with_memory_write_scope() {
    let p = make_principal(&[Scope::MemoryWrite]);
    assert!(
        check_scope(&p, "memory_store"),
        "memory_store must succeed when memory:write is present"
    );
}

// ── Test 6: wiki_write allowed when wiki:write present ───────────────────
#[test]
fn wiki_write_allowed_with_wiki_write_scope() {
    let p = make_principal(&[Scope::WikiWrite]);
    assert!(
        check_scope(&p, "wiki_write"),
        "wiki_write must succeed when wiki:write is present"
    );
}

// ── Test 7: tool_requires_scope returns correct scopes ───────────────────
#[test]
fn tool_requires_scope_table_is_correct() {
    use crate::mcp_auth::tool_requires_scope;
    assert_eq!(
        tool_requires_scope("memory_store"),
        Some(Scope::MemoryWrite)
    );
    assert_eq!(tool_requires_scope("wiki_write"), Some(Scope::WikiWrite));
    assert_eq!(
        tool_requires_scope("send_message"),
        Some(Scope::MessagingSend)
    );
    assert_eq!(
        tool_requires_scope("memory_search"),
        Some(Scope::MemoryRead)
    );
    assert_eq!(tool_requires_scope("wiki_read"), Some(Scope::WikiRead));
    // OS-native Phase 1 tools map to the dedicated os:native scope.
    assert_eq!(tool_requires_scope("os_notify"), Some(Scope::OsNative));
    assert_eq!(
        tool_requires_scope("os_watch_status"),
        Some(Scope::OsNative)
    );
    assert_eq!(tool_requires_scope("os_open"), Some(Scope::OsNative));
    // OS-native P2-4 sensing tools map to the same os:native scope.
    assert_eq!(tool_requires_scope("os_frontmost"), Some(Scope::OsNative));
    assert_eq!(
        tool_requires_scope("os_spotlight_search"),
        Some(Scope::OsNative)
    );
    assert_eq!(
        tool_requires_scope("os_calendar_today"),
        Some(Scope::OsNative)
    );
    // C2 fail-closed: unmapped tools require Admin, not None.
    assert_eq!(tool_requires_scope("totally_unknown"), Some(Scope::Admin));
}
