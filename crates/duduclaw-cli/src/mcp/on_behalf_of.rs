//! The person this MCP server's employee works for (`on_behalf_of`).
//!
//! The only source is the `DUDUCLAW_ON_BEHALF_OF` variable the gateway set
//! when it spawned the employee (see `duduclaw_gateway::on_behalf_of`); no
//! tool argument is ever read. The value is written onto what this process
//! hands on (`message_queue.on_behalf_of`, the `bus_queue.jsonl` field, the
//! task row) and, with `[delegation] human_principal = "enforce"`, narrows
//! delegations the employee tree already allowed.

use super::*;

/// The principal from the host env, validated. A malformed value is audited
/// once per process as `on_behalf_of_rejected` and treated as absent.
pub(crate) fn host_on_behalf_of(home_dir: &Path, caller: &str) -> Option<String> {
    match duduclaw_core::host_principal_checked() {
        Ok(p) => p.map(|p| p.to_string()),
        Err(rejection) => {
            audit_rejection_once(home_dir, caller, &rejection);
            None
        }
    }
}

fn audit_rejection_once(home_dir: &Path, caller: &str, rejection: &duduclaw_core::PrincipalRejection) {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if ONCE.set(()).is_ok() {
        duduclaw_gateway::on_behalf_of::audit_rejected(home_dir, caller, "mcp_env", rejection);
    }
}

/// Enforce-mode check for `sender → target` on `path_kind` (call it after
/// the employee tree allowed the delegation). Record mode always passes.
/// A refusal writes a `delegation_denied` row (which carries the principal)
/// and returns the zh-TW reason.
pub(crate) fn check_human_principal(
    home_dir: &Path,
    sender: &str,
    target: &str,
    path_kind: &str,
    policy: duduclaw_core::DelegationPolicy,
) -> std::result::Result<(), String> {
    let mode = duduclaw_core::HumanPrincipalMode::from_home(home_dir);
    let principal = duduclaw_core::host_principal_checked();
    if let Err(rejection) = &principal {
        audit_rejection_once(home_dir, sender, rejection);
    }
    match duduclaw_gateway::on_behalf_of::access::check(
        home_dir, mode, &principal, sender, target, path_kind, policy,
    ) {
        Ok(()) => Ok(()),
        Err(denied) => {
            duduclaw_security::audit::append_tool_call_with_extras(
                home_dir,
                sender,
                "delegation_denied",
                &format!(
                    "{path_kind}: '{sender}' -> '{target}' denied ({})",
                    denied.reason.as_str()
                ),
                false,
                &[
                    ("target", serde_json::json!(target)),
                    ("reason", serde_json::json!(denied.reason.as_str())),
                    ("policy", serde_json::json!(denied.policy.as_str())),
                    ("path_kind", serde_json::json!(path_kind)),
                    ("human_principal", serde_json::json!(mode.as_str())),
                ],
            );
            Err(denied.message_zh())
        }
    }
}
