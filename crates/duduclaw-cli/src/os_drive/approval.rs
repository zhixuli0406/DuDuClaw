//! A7a — the `requires_approval` gate. See
//! `commercial/docs/DESIGN-os-self-drive-2026-08.md` §5 for the full
//! semantics; short version: an operator terminal (no `DUDUCLAW_AGENT_ID` in
//! the environment) runs a `requires_approval` command directly, an
//! agent-identity caller must clear `ApprovalBroker` first — the exact same
//! fail-closed polling primitive `mcp_os_ops::require_factory_reset_approval`
//! already uses for `os_factory_reset` (`crate::mcp::run_install_approval`),
//! reused unchanged rather than re-derived.

use std::path::Path;

/// TTL for an os-drive approval request — same value `os_factory_reset` uses
/// (`FACTORY_RESET_APPROVAL_TTL_SECONDS` in `mcp_os_ops.rs`): 5 minutes is a
/// realistic window for a human to see the push notification and decide.
const OS_DRIVE_APPROVAL_TTL_SECONDS: i64 = 300;
/// Poll cadence while blocking on the decision — same value as
/// `FACTORY_RESET_APPROVAL_POLL`.
const OS_DRIVE_APPROVAL_POLL: std::time::Duration = std::time::Duration::from_secs(2);

/// True iff the current process is running as an agent identity — the same
/// ambient-env convention `org_field_guard.rs`/`agent_guard.rs`/
/// `delegation_policy.rs` already use (`duduclaw_core::ENV_AGENT_ID`,
/// `DUDUCLAW_AGENT_ID`). An empty value is treated as "not set" (same
/// leniency `require_factory_reset_approval_via`'s `caller_client_id`
/// handling already applies).
pub fn caller_agent_id() -> Option<String> {
    std::env::var(duduclaw_core::ENV_AGENT_ID)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Run the `requires_approval` gate for one os-drive command.
///
/// - Not an agent-identity caller (interactive/operator terminal): always
///   `Ok(())` — no gate at all, matching the design doc's "a person is
///   standing in front of this machine" reasoning.
/// - Agent-identity caller: opens `ApprovalBroker` at `home_dir` and blocks
///   on a human decision. Broker-unavailable / denied / expired all deny,
///   fail-closed — identical semantics to `os_factory_reset`'s gate.
pub async fn gate(home_dir: &Path, description: &str, tool: &str) -> Result<(), String> {
    let Some(agent_id) = caller_agent_id() else {
        return Ok(());
    };

    let broker = match duduclaw_gateway::approval::ApprovalBroker::open(home_dir) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                error = %e,
                tool,
                "os_drive: ApprovalBroker unavailable — denying (fail-closed)"
            );
            return Err(
                "審批系統暫時無法使用，已拒絕（fail-closed）。請稍後再試或由操作者於終端機直接執行。"
                    .to_string(),
            );
        }
    };

    match crate::mcp::run_install_approval(
        &broker,
        &agent_id,
        description,
        serde_json::json!({ "tool": tool }),
        OS_DRIVE_APPROVAL_TTL_SECONDS,
        OS_DRIVE_APPROVAL_POLL,
    )
    .await
    {
        crate::mcp::InstallApprovalOutcome::Proceed => Ok(()),
        crate::mcp::InstallApprovalOutcome::Denied(msg) => Err(msg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_agent_id_treats_unset_and_empty_as_none() {
        // Never mutates process env in-process (same discipline
        // `mcp_os_ops.rs`'s own appliance-gate tests follow, to avoid racing
        // other tests in the same process that read env vars) — this only
        // exercises the pure filtering logic via a stand-in.
        assert_eq!(filter_agent_id(None), None);
        assert_eq!(filter_agent_id(Some("".to_string())), None);
        assert_eq!(filter_agent_id(Some("  ".to_string())), None);
        assert_eq!(filter_agent_id(Some(" ceo ".to_string())), Some("ceo".to_string()));
    }

    /// Pure stand-in for the env-reading half of [`caller_agent_id`], so the
    /// trim/empty-filter logic is testable without touching process env.
    fn filter_agent_id(raw: Option<String>) -> Option<String> {
        raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }

    #[tokio::test]
    async fn operator_terminal_bypasses_the_gate_entirely() {
        // No DUDUCLAW_AGENT_ID in this test process by construction (never
        // set process-wide in this crate's tests) — the gate must short
        // circuit to Ok without ever touching a broker/home_dir, so an
        // intentionally-invalid path is safe to pass.
        assert!(std::env::var(duduclaw_core::ENV_AGENT_ID).is_err());
        let result = gate(Path::new("/nonexistent/path/should/never/be/touched"), "test", "test_tool").await;
        assert!(result.is_ok(), "{result:?}");
    }
}
