//! Unit tests for [`super`], moved verbatim out of the former `approval.rs`.
//!
//! Shared fixtures live here; the cases are split across the sibling
//! files for size only.

mod broker_cases;
mod gate_cases;
mod guard_cases;
mod simulation_cases;

use super::*;

use serde_json::json;

// ── R5: agent.toml missing-key directions, pinned ────────────────────
//
// These four `[capabilities]` readers moved onto the shared typed parse
// point. Each one's missing-key direction is a deliberate, *asymmetric*
// decision, and the asymmetry is the point:
//
//   approval_required_tools / irreversible_tools /
//   maybe_irreversible_tools  absent ⇒ EMPTY (fail-*safe*). They are
//                 additive friction on top of the deny-list, which is the
//                 real security boundary and independently fails closed.
//                 Defaulting to "everything needs approval" would brick
//                 every agent on one typo.
//   auto_approve_install      absent ⇒ FALSE, i.e. the gate stays ON
//                 (fail-*closed*). Only an explicit `true` opens it.
//
// Two readers in one section pointing opposite ways is exactly the kind
// of thing a schema refactor flattens by accident. Changing either
// direction must be a deliberate decision with its own reasoning.

/// `tmp_agent_dir` + an `agent.toml` in one step. (`tmp_agent_dir` and
/// `write_agent_toml` are defined further down in this same module.)
fn with_toml(body: &str) -> std::path::PathBuf {
    let dir = tmp_agent_dir();
    write_agent_toml(&dir, body);
    dir
}

fn broker() -> ApprovalBroker {
    ApprovalBroker::new(std::sync::Arc::new(
        ApprovalStore::open_in_memory().unwrap(),
    ))
}

// ── decision-source parsers ─────────────────────────────

fn write_agent_toml(dir: &Path, body: &str) {
    std::fs::write(dir.join("agent.toml"), body).unwrap();
}

fn tmp_agent_dir() -> PathBuf {
    let p = std::env::temp_dir().join(format!("duduclaw-approval-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn write_scope_policy(home: &Path, body: &str) {
    let dir = home.join("shared").join("wiki");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".scope.toml"), body).unwrap();
}

// ── WP20: TTL reminder scheduling ───────────────────────

/// Build a pending record created `age_secs` ago with the given TTL.
fn aged(age_secs: i64, ttl: i64, reminded: bool) -> ApprovalRecord {
    ApprovalRecord {
        id: ApprovalId::new(),
        agent_id: "a".into(),
        action_kind: "mcp_install".into(),
        summary: "s".into(),
        payload: json!({}),
        status: ApprovalStatus::Pending,
        created_at: (Utc::now() - chrono::Duration::seconds(age_secs)).to_rfc3339(),
        decided_at: None,
        decided_by: None,
        ttl_seconds: ttl,
        notify_channel: None,
        notify_chat_id: None,
        reminded_at: reminded.then(|| Utc::now().to_rfc3339()),
        simulation: None,
    }
}

