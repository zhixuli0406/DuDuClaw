//! H13: production selectors for `spawn_agent` and `spawn_ephemeral`
//! (Package H tools 20–21).
//!
//! Both tools are host-queue work of the calling employee, not workspace
//! operations: the authority is the record's own `Employee` grant (plus, for a
//! task/schedule run, the exact signed `Task` grant of its record task, whose
//! live ownership the host resolver re-reads on every call). Neither tool
//! accepts an identity, run, root or delegation-metadata argument; the
//! protected schemas below are exact.
//!
//! - `spawn_agent`: the target (`agent_id`) is data; the host resolver must
//!   find it allowed by the live delegation policy AND by the isolation
//!   bindings (a Personal target is refused; a Personal source's delegation to
//!   a Shared target is filed through the existing owner approval card by the
//!   handler's `record_bus_delegation`).
//! - `spawn_ephemeral` (D-H3): the child runs under the parent employee's own
//!   identity, so its tool set is the requested tools intersected with the
//!   parent run's signed tool snapshot ([`ephemeral_tools`]) — never more than
//!   the parent run. A Personal parent may spawn only on its owner's own
//!   request (the record principal is the Human owner).
use duduclaw_isolation::capability::{EmployeeBinding, Principal, ResourceGrant};
use serde_json::Value;
use crate::{RequestContext, VerificationError};
use super::{AuthorizedToolCall, ResourceResolver, ToolPolicy, own_employee, validate_resource};

/// The two H13 tools.
pub const SPAWN_TOOLS: [&str; 2] = ["spawn_agent", "spawn_ephemeral"];
/// Exact protected `spawn_agent` arguments (the legacy schema; the host
/// stamps sender, origin, depth, hop, turn and principal itself).
pub const SPAWN_AGENT_KEYS: [&str; 3] = ["agent_id", "task", "session_key"];
/// Exact protected `spawn_ephemeral` arguments.
pub const SPAWN_EPHEMERAL_KEYS: [&str; 4] = ["instruction", "context", "tools", "tier"];
/// Largest task/instruction/context text (the dispatcher's bus payload cap).
pub const SPAWN_TEXT_MAX_BYTES: usize = 100_000;
/// Most tools one ephemeral request may name.
pub const EPHEMERAL_MAX_TOOLS: usize = 64;
/// Model tiers an ephemeral request may name (raw model ids are refused).
pub const EPHEMERAL_TIERS: [&str; 3] = ["cheap", "standard", "preferred"];
/// Prefix of a duduclaw MCP tool in `allowed_tools` notation.
pub const MCP_TOOL_PREFIX: &str = "mcp__duduclaw__";

/// Same rule as `duduclaw_core::is_valid_agent_id` (this crate does not
/// depend on core): 1..=64 ASCII alphanumerics, `-` or `_`.
fn agent_label(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64 && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn bounded_text(object: &serde_json::Map<String, Value>, key: &str) -> Result<(), VerificationError> {
    match object.get(key) {
        Some(Value::String(text)) if !text.trim().is_empty() && text.len() <= SPAWN_TEXT_MAX_BYTES => Ok(()),
        _ => Err(VerificationError::Invalid),
    }
}

/// The requested tool names of a `spawn_ephemeral` call, parsed like the
/// handler (a JSON array of strings, or one comma-separated string), trimmed,
/// empty entries dropped. Every entry must be an exact name: no `*`, no
/// control character, ≤ 128 bytes; 1..=[`EPHEMERAL_MAX_TOOLS`] entries.
pub fn requested_tools(arguments: &Value) -> Result<Vec<String>, VerificationError> {
    let raw: Vec<String> = match arguments.get("tools").ok_or(VerificationError::Invalid)? {
        Value::Array(items) => items.iter()
            .map(|item| item.as_str().map(|s| s.trim().to_owned()).ok_or(VerificationError::Invalid))
            .collect::<Result<_, _>>()?,
        Value::String(list) => list.split(',').map(|s| s.trim().to_owned()).collect(),
        _ => return Err(VerificationError::Invalid),
    };
    let tools: Vec<String> = raw.into_iter().filter(|name| !name.is_empty()).collect();
    if tools.is_empty() || tools.len() > EPHEMERAL_MAX_TOOLS
        || tools.iter().any(|name| name.len() > 128 || name.contains('*') || name.chars().any(char::is_control)) {
        return Err(VerificationError::Invalid);
    }
    Ok(tools)
}

/// Exact protected schema of [`SPAWN_TOOLS`]. Unknown keys (identity,
/// delegation metadata, run/root selectors) refuse instead of being ignored.
pub fn check_spawn_arguments(name: &str, arguments: &Value) -> Result<(), VerificationError> {
    let object = arguments.as_object().ok_or(VerificationError::Invalid)?;
    match name {
        "spawn_agent" => {
            if object.keys().any(|key| !SPAWN_AGENT_KEYS.contains(&key.as_str())) { return Err(VerificationError::Invalid); }
            if !object.get("agent_id").and_then(Value::as_str).is_some_and(agent_label) {
                return Err(VerificationError::Invalid);
            }
            bounded_text(object, "task")?;
            match object.get("session_key") {
                None => Ok(()),
                Some(Value::String(key)) if key.len() <= 256 && !key.chars().any(char::is_control) => Ok(()),
                Some(_) => Err(VerificationError::Invalid),
            }
        }
        "spawn_ephemeral" => {
            if object.keys().any(|key| !SPAWN_EPHEMERAL_KEYS.contains(&key.as_str())) { return Err(VerificationError::Invalid); }
            bounded_text(object, "instruction")?;
            bounded_text(object, "context")?;
            if object.get("tier").is_some_and(|tier| !tier.as_str().is_some_and(|tier| EPHEMERAL_TIERS.contains(&tier))) {
                return Err(VerificationError::Invalid);
            }
            requested_tools(arguments).map(|_| ())
        }
        _ => Err(VerificationError::Invalid),
    }
}

/// D-H3: a Personal parent spawns only on its owner's own request — the
/// record principal must be the Human owner of the Personal binding. A Shared
/// parent has no owner restriction beyond its live membership (the host
/// resolver re-validates the binding on every call).
pub fn ephemeral_parent_allowed(context: &RequestContext) -> Result<(), VerificationError> {
    let record = context.record();
    match (&record.employee_binding, &record.principal) {
        (EmployeeBinding::Shared(_), _) => Ok(()),
        (EmployeeBinding::Personal(_), Principal::Human { user_id, .. })
            if record.personal_owner_matches(user_id.as_str()) => Ok(()),
        (EmployeeBinding::Personal(_), _) => Err(VerificationError::BindingMismatch),
    }
}

/// Record-side selection shared by discovery (`eligible`) and calls: the own
/// `Employee` grant, plus the exact signed `Task` grant of the record task
/// when the run has one (live ownership is the resolver's job). For
/// `spawn_ephemeral` additionally [`ephemeral_parent_allowed`].
pub fn spawn_selector(context: &RequestContext, name: &str) -> Result<Vec<ResourceGrant>, VerificationError> {
    if !SPAWN_TOOLS.contains(&name) { return Err(VerificationError::Invalid); }
    let record = context.record();
    let own = own_employee(context);
    if !record.io_resources().any(|grant| grant == &own) { return Err(VerificationError::Invalid); }
    let mut selection = vec![own];
    if let Some(task) = &record.task {
        let selector = ResourceGrant::Task { id: task.clone() };
        validate_resource(&selector)?;
        if !record.io_resources().any(|grant| grant == &selector) { return Err(VerificationError::Invalid); }
        selection.push(selector);
    }
    if name == "spawn_ephemeral" { ephemeral_parent_allowed(context)?; }
    Ok(selection)
}

/// D-H3 tool set of an ephemeral child: the requested names (`name` or
/// `mcp__duduclaw__name`) that exactly equal a tool of the parent run's signed
/// snapshot (`record.tools`), in request order, without duplicates. Anything
/// else (native tools, unknown or unlisted names) is dropped, so the child can
/// never hold a tool the parent run does not; an empty intersection refuses.
pub fn ephemeral_tools(context: &RequestContext, arguments: &Value) -> Result<Vec<String>, VerificationError> {
    let parent = context.record().tools.as_slice();
    let mut granted: Vec<String> = Vec::new();
    for name in requested_tools(arguments)? {
        let bare = name.strip_prefix(MCP_TOOL_PREFIX).unwrap_or(name.as_str());
        if parent.iter().any(|tool| tool.as_str() == bare) && !granted.iter().any(|tool| tool == bare) {
            granted.push(bare.to_owned());
        }
    }
    if granted.is_empty() { return Err(VerificationError::Invalid); }
    Ok(granted)
}

/// Call path of [`SPAWN_TOOLS`]: exact schema (already checked by
/// `authorize`), record-side selection, then the host resolver, which must
/// vouch for exactly that selection after its live checks (delegation policy,
/// target binding, task ownership); it can neither add nor substitute a grant.
pub(super) fn authorize_spawn(context: &RequestContext, policy: &'static ToolPolicy, arguments: &Value,
    resolver: &dyn ResourceResolver) -> Result<AuthorizedToolCall, VerificationError> {
    let selection = spawn_selector(context, policy.name)?;
    if policy.name == "spawn_ephemeral" { ephemeral_tools(context, arguments)?; }
    let resources = resolver.resolve(context, policy, arguments)?;
    if resources.len() != selection.len() || !selection.iter().all(|grant| resources.contains(grant)) {
        return Err(VerificationError::Invalid);
    }
    for resource in &selection {
        crate::resources::require_resource(context, resource)?;
        validate_resource(resource)?;
    }
    Ok(AuthorizedToolCall { context: context.clone(), policy, arguments: arguments.clone(), resources: selection })
}

#[cfg(test)]
#[path = "tool_policy_spawn_tests.rs"]
mod tests;
