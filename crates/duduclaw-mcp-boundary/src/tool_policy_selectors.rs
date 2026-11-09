//! Record-side selectors of `tool_policy` (H8 `create_task`, H9 NewTask,
//! H12a own-skill tools). Moved verbatim out of `tool_policy.rs` (H13, file
//! size); every item is re-exported there, so paths are unchanged.
use duduclaw_isolation::capability::{Principal, ResourceGrant};
use serde_json::Value;
use crate::{RequestContext, VerificationError};
use super::{own_employee, validate_resource};

/// Top-level task selector argument keys. `create_task` never accepts them.
pub const TASK_SELECTOR_KEYS: [&str; 5] = ["task_id", "parent_task_id", "goal_id", "parent_goal_id", "depends_on"];

/// H8 live task selector for `create_task`: the task is the run's own task
/// from the verified capability record (`record.task`), never an argument, and
/// the record must also carry the exact signed `Task` grant for it. Live
/// ownership (the task store says the calling employee owns it now) is the
/// resolver's job at use time; this is only the record-side half, shared by
/// discovery (`eligible`) and `authorize`.
pub fn create_task_selector(context: &RequestContext) -> Result<ResourceGrant, VerificationError> {
    let record = context.record();
    let task = record.task.as_ref().ok_or(VerificationError::Invalid)?;
    let selector = ResourceGrant::Task { id: task.clone() };
    validate_resource(&selector)?;
    if !record.io_resources().any(|grant| grant == &selector) { return Err(VerificationError::Invalid); }
    Ok(selector)
}

/// H12a own-skill tools. They act only on the record's own employee: its
/// host-owned skill files (`agents/<label>/SKILLS`, read by the trusted
/// producer and parsed as data by the E worker) and its own wiki/SKILLS
/// publication. Identity is the verified record (EmployeeScope + its own
/// `Employee` grant); the agent label is bound by the host producer, never an
/// argument. Task-limited records (no EmployeeScope) never list them.
pub const OWN_SKILL_TOOLS: &[&str] = &["skill_extract", "shared_skill_adopt", "skill_security_scan",
    "skill_graduate", "skill_curator_status"];

/// The exact argument schema of [`OWN_SKILL_TOOLS`] in protected mode. No
/// selector names another employee: `target_agent`, `scope`, `namespace`,
/// paths and execution identities refuse. `agent_id` is accepted only as the
/// legacy schema's self-reference; the producer requires it to equal the bound
/// employee. Values are short slugs, so skill content never travels here.
pub fn check_own_skill_arguments(name: &str, arguments: &Value) -> Result<(), VerificationError> {
    let object = arguments.as_object().ok_or(VerificationError::Invalid)?;
    let allowed: &[&str] = match name {
        "skill_curator_status" => &["run"],
        "shared_skill_adopt" => &["skill_name"],
        "skill_extract" | "skill_security_scan" | "skill_graduate" => &["skill_name", "agent_id"],
        _ => return Err(VerificationError::Invalid),
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) { return Err(VerificationError::Invalid); }
    if object.get("run").is_some_and(|value| !value.is_boolean()) { return Err(VerificationError::Invalid); }
    let slug = |value: &Value| value.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 64
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    if name != "skill_curator_status" && !object.get("skill_name").is_some_and(slug) {
        return Err(VerificationError::Invalid);
    }
    if object.get("agent_id").is_some_and(|value| !slug(value)) { return Err(VerificationError::Invalid); }
    Ok(())
}

/// Record-side selection for [`OWN_SKILL_TOOLS`], shared by discovery and
/// calls: the employee's own `Employee` grant under its own EmployeeScope,
/// plus (skill_extract only) exactly one own-namespace wiki output grant.
/// Which `agent:<label>` namespace is "own" is the host resolver's and the
/// producer's decision (`verify_wiki_root`); here only the shape is pinned.
pub fn own_skill_selector(context: &RequestContext, name: &str) -> Result<Vec<ResourceGrant>, VerificationError> {
    if !OWN_SKILL_TOOLS.contains(&name) { return Err(VerificationError::Invalid); }
    let record = context.record();
    let own = own_employee(context);
    if !record.employee_scope() || !record.io_resources().any(|grant| grant == &own) {
        return Err(VerificationError::Invalid);
    }
    let mut selection = vec![own];
    if name == "skill_extract" {
        let wikis: Vec<_> = record.io_resources().filter(|grant| matches!(grant, ResourceGrant::Wiki { namespace }
            if namespace.as_str().strip_prefix("agent:").is_some_and(|label| !label.is_empty()))
            && validate_resource(grant).is_ok()).collect();
        let [wiki] = wikis.as_slice() else { return Err(VerificationError::Invalid); };
        selection.push((*wiki).clone());
    }
    Ok(selection)
}

/// H9 owner decision 3: the interactive-run `create_task` authority. The record
/// must be an employee-scope record without a task, its principal a `Human`,
/// and it must carry the plain (never `Confined`) signed
/// `NewTask { requester }` grant for exactly that human. The requester is the
/// authenticated person of the run; it is never read from arguments. A
/// Background principal never qualifies (the capability decoder refuses such
/// a grant too).
pub fn new_task_grant(context: &RequestContext) -> Result<ResourceGrant, VerificationError> {
    let record = context.record();
    let Principal::Human { user_id, .. } = &record.principal else { return Err(VerificationError::Invalid); };
    if record.task.is_some() || !record.employee_scope() { return Err(VerificationError::Invalid); }
    let grant = ResourceGrant::NewTask { requester: user_id.clone() };
    validate_resource(&grant)?;
    if !record.resources.as_slice().contains(&grant) { return Err(VerificationError::Invalid); }
    Ok(grant)
}

/// Top-level `create_task` arguments a `NewTask` call may carry. Everything
/// else (task/goal/dependency selectors, assignee or creator fields, identity
/// keys) is refused instead of being ignored.
pub const NEW_TASK_KEYS: [&str; 2] = ["goal", "steps"];
/// Keys a `NewTask` plan step may carry (the existing step schema). The step
/// `agent` must name the calling employee or be empty; the resolver, which
/// knows the employee's agent label, enforces that.
pub const NEW_TASK_STEP_KEYS: [&str; 4] = ["description", "agent", "depends_on", "acceptance_criteria"];
/// Most steps in one interactive plan.
pub const NEW_TASK_MAX_STEPS: usize = 64;

/// The plan steps of a `create_task` call, parsed exactly as the handler parses
/// them (a JSON array, or a JSON string holding one), each a JSON object.
pub fn new_task_steps(arguments: &Value) -> Result<Vec<Value>, VerificationError> {
    let steps = match arguments.get("steps").ok_or(VerificationError::Invalid)? {
        Value::Array(steps) => steps.clone(),
        Value::String(raw) => serde_json::from_str::<Vec<Value>>(raw).map_err(|_| VerificationError::Invalid)?,
        _ => return Err(VerificationError::Invalid),
    };
    if steps.is_empty() || steps.len() > NEW_TASK_MAX_STEPS || steps.iter().any(|step| !step.is_object()) {
        return Err(VerificationError::Invalid);
    }
    Ok(steps)
}
