//! FD-SEL: production selectors for the last pending protected-mode tools of
//! Package H (tools 22–29).
//!
//! - Discovery job tools (`discovery_catalog`, `discovery_list`,
//!   `discovery_tree`, `discovery_artifact`, `discovery_cancel`): host
//!   discovery service reads/cancellation of the caller's OWN discovery runs.
//!   Decision (selector, not a refusal): discovery *execution* is
//!   operator-only (disposable containers, a dedicated account pool, and a
//!   manager decision receipt before any run starts), but a protected employee
//!   already files discovery *requests* through `tasks_create` (kind
//!   `discovery`) as `TrustedCaller::from_isolated_agent`, and every such
//!   request waits for a manager. These five tools only let the employee see,
//!   download the published checkpoint of, or withdraw a request it filed
//!   itself; they never start, approve or widen a run. The host service binds
//!   each protected request to the creator's exact `(employee id,
//!   incarnation)` and answers every other run — foreign, unknown, filed by
//!   another incarnation or before this binding existed — with the same
//!   `discovery not found`.
//! - Fork store reads (`inspect_branches`, `diff_branches`, `fork_cost`):
//!   read-only views of the caller's OWN protected forks in the host fork
//!   store (`forks::ForkStore`, rows keyed by the exact actor = employee id,
//!   principal and binding). A foreign or unknown fork is the same
//!   `Invalid` (CLI: `fork_not_found`). The three fork mutations stay
//!   `ForkNextRelease` refusals (`fork_policy::DEFERRED_TOOLS`).
//!
//! The selection is exactly the record's own `Employee` grant: no Workspace,
//! no Task, no Recipient. Ownership is live host state (the discovery creator
//! binding, the fork actor), checked by the host service on every call, so the
//! resolver is never consulted and cannot widen the selection.
use duduclaw_isolation::capability::ResourceGrant;
use serde_json::Value;
use crate::{RequestContext, VerificationError};
use super::{AuthorizedToolCall, ToolPolicy, own_employee, validate_resource};

/// Discovery job tools (Package H 22–26).
pub const OWN_DISCOVERY_TOOLS: [&str; 5] = ["discovery_catalog", "discovery_list", "discovery_tree",
    "discovery_artifact", "discovery_cancel"];
/// Fork store reads (Package H 27–29); the same list the host fork service supports.
pub const OWN_FORK_READ_TOOLS: [&str; 3] = crate::fork_policy::READ_TOOLS;
/// Every FD-SEL tool.
pub const OWN_READ_TOOLS: [&str; 8] = ["discovery_catalog", "discovery_list", "discovery_tree",
    "discovery_artifact", "discovery_cancel", "inspect_branches", "diff_branches", "fork_cost"];
/// Largest opaque run/fork/branch/file identifier accepted (discovery ids are
/// ≤ 128 bytes; fork ids are 32 hex; artifact file ids are 69 bytes).
pub const OWN_READ_ID_MAX: usize = 128;
/// `discovery_list` limit bounds (the service's own bounds).
pub const OWN_READ_LIST_LIMIT: std::ops::RangeInclusive<u64> = 1..=100;

/// The record-side selection: exactly the record's own `Employee` grant.
pub fn own_read_selector(context: &RequestContext, name: &str) -> Result<Vec<ResourceGrant>, VerificationError> {
    if !OWN_READ_TOOLS.contains(&name) { return Err(VerificationError::Invalid); }
    let own = own_employee(context);
    if !context.record().io_resources().any(|grant| grant == &own) { return Err(VerificationError::Invalid); }
    Ok(vec![own])
}

/// An opaque host identifier: 1..=[`OWN_READ_ID_MAX`] ASCII alphanumerics,
/// `-` or `_` (never a path, never `*`).
fn opaque_id(value: &Value) -> bool {
    value.as_str().is_some_and(|id| !id.is_empty() && id.len() <= OWN_READ_ID_MAX
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')))
}

/// Same rule as `duduclaw_core::is_valid_agent_id` (no core dependency here).
fn agent_label(value: &Value) -> bool {
    value.as_str().is_some_and(|label| !label.is_empty() && label.len() <= 64
        && label.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')))
}

/// Exact protected schema of [`OWN_READ_TOOLS`]. Unknown keys (identity, run,
/// root, workspace, employee selectors) refuse instead of being ignored; an
/// optional key may be `null` (= absent). `agent_id` stays a data filter of
/// the original discovery schema; the host service still applies the
/// creator binding and the delegation policy to it.
pub fn check_own_read_arguments(name: &str, arguments: &Value) -> Result<(), VerificationError> {
    let object = arguments.as_object().ok_or(VerificationError::Invalid)?;
    let (required, optional): (&[&str], &[&str]) = match name {
        "discovery_catalog" => (&[], &["agent_id"]),
        "discovery_list" => (&[], &["agent_id", "limit"]),
        "discovery_tree" | "discovery_cancel" => (&["run_id"], &[]),
        "discovery_artifact" => (&["run_id"], &["file_id"]),
        "inspect_branches" | "fork_cost" => (&["fork_id"], &[]),
        // The protected listing's schema (`mcp_isolation::project_tool_schema`).
        "diff_branches" => (&["fork_id", "left_branch_id", "right_branch_id"], &[]),
        _ => return Err(VerificationError::Invalid),
    };
    if required.iter().any(|key| !object.contains_key(*key)) { return Err(VerificationError::Invalid); }
    for (key, value) in object {
        let key = key.as_str();
        if optional.contains(&key) && value.is_null() { continue; }
        let valid = match key {
            _ if !required.contains(&key) && !optional.contains(&key) => false,
            "agent_id" => agent_label(value),
            "limit" => value.as_u64().is_some_and(|limit| OWN_READ_LIST_LIMIT.contains(&limit)),
            _ => opaque_id(value),
        };
        if !valid { return Err(VerificationError::Invalid); }
    }
    Ok(())
}

/// `tool_policy::authorize` for [`OWN_READ_TOOLS`] (after the exact schema and
/// `eligible`): the selected resources are exactly the record's own Employee
/// grant; no resolver output is merged.
pub(super) fn authorize(context: &RequestContext, policy: &'static ToolPolicy, arguments: &Value)
    -> Result<AuthorizedToolCall, VerificationError> {
    let resources = own_read_selector(context, policy.name)?;
    for resource in &resources {
        crate::resources::require_resource(context, resource)?;
        validate_resource(resource)?;
    }
    Ok(AuthorizedToolCall { context: context.clone(), policy, arguments: arguments.clone(), resources })
}

#[cfg(test)]
#[path = "tool_policy_own_reads_tests.rs"]
mod tests;
