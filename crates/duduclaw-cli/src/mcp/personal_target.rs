//! Cross-agent MCP tools never act on another person's Personal employee
//! (native Linux isolation v1.81, tasks P1 + P1b, SR2 finding 5).
//!
//! In protected mode an AI caller (any MCP principal: the internal key of a
//! running employee, a per-agent key or an external key) that names ANOTHER
//! employee in one of [`PERSONAL_TARGET_TOOLS`] is refused when that employee
//! is a Personal employee (users.db `isolation_personal_agents` mirror, read
//! fresh per call). Only the Personal employee itself — or an ephemeral
//! sub-agent it spawned, resolved through the host-recorded parent chain
//! (`personal_visibility::acting_employee`) — may still use these tools on
//! it (subject to every existing gate); its human owner manages it from the
//! dashboard. Shared targets and non-protected installs are unchanged. The
//! decision is `duduclaw_gateway::personal_visibility::management_refusal`,
//! shared with the gateway's dashboard gates.
//!
//! P1b additions ([`pre_dispatch_refusal`], one call in `dispatch.rs`):
//! delegation and read tools in the table (`spawn_agent`, `send_to_agent`,
//! `check_responses`), the process store read by `task_status`
//! ([`PROCESS_STORE_TOOLS`]) and a memory `namespace` argument
//! ([`namespace_refusal`]). `list_agents` hides other Personal employees
//! entirely ([`list_agents_hiding`]; no metadata for other AI employees) and
//! `task_status` hides the output of a step run by one ([`hidden_step_agents`]).
//!
//! P1c: every other tool whose schema names a target employee is in the
//! table (delegation: `tasks_create`/`tasks_update` `assigned_to`,
//! `discovery_catalog`; org placement: `reports_to`; work-record, schedule,
//! feedback, audit, reliability and cost reads), tasks and plans named by id
//! resolve their employee from `tasks.db` ([`id_target_refusal`]), and the
//! delegation tools ([`DELEGATION_TOOLS`]) also let another Personal employee
//! of the same owner hand work over (the owner's own work).
//!
//! A self-target (target == acting employee, own namespace, own store) is
//! decided before any `users.db` read, so it keeps working inside an
//! isolated run whose MCP process cannot open `users.db`; every
//! cross-employee target then refuses `OWNERSHIP_UNAVAILABLE` (fail closed).
use duduclaw_gateway::personal_visibility as visibility;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;

/// Tools whose argument names another employee whose configuration, files,
/// wiki, skills, mailbox, identity, responses or work queue the call reads
/// or changes, with the argument key. A tool may appear once per key it
/// accepts; every listed key is checked. An absent/blank argument means
/// "the caller itself".
pub(crate) const PERSONAL_TARGET_TOOLS: &[(&str, &str)] = &[
    // Management of another employee's config / SOUL / existence.
    ("agent_update", "agent_id"),
    ("agent_update_soul", "agent_id"),
    ("agent_remove", "agent_id"),
    ("agent_status", "agent_id"),
    ("evolution_toggle", "agent_id"),
    ("evolution_status", "agent_id"),
    // Another employee's skills.
    ("skill_security_scan", "agent_id"),
    ("skill_graduate", "agent_id"),
    ("skill_synthesis_status", "agent_id"),
    ("skill_synthesis_run", "agent_id"),
    ("skill_extract", "agent_id"),
    ("skill_gaps", "agent_id"),
    ("skill_list", "agent_id"),
    ("shared_skill_adopt", "target_agent"),
    // Another employee's own wiki.
    ("wiki_ls", "agent_id"),
    ("wiki_read", "agent_id"),
    ("wiki_write", "agent_id"),
    ("wiki_search", "agent_id"),
    ("wiki_lint", "agent_id"),
    ("wiki_stats", "agent_id"),
    ("wiki_export", "agent_id"),
    ("wiki_dedup", "agent_id"),
    ("wiki_graph", "agent_id"),
    ("wiki_rebuild_fts", "agent_id"),
    ("wiki_trust_audit", "agent_id"),
    ("wiki_trust_history", "agent_id"),
    // Another employee's mailbox / acting identity.
    ("mail_list", "agent_id"),
    ("mail_read", "agent_id"),
    ("mail_send", "agent_id"),
    ("codrive_run", "agent"),
    // P1b: work handed to / responses read from another employee. The
    // isolated runtime already refuses background work for a Personal
    // target; this keeps the legacy path and the listing consistent.
    ("spawn_agent", "agent_id"),
    ("send_to_agent", "agent_id"),
    ("send_to_agent", "agent"),
    ("check_responses", "agent_id"),
    // P1c: every remaining tool whose schema (`duduclaw_core::mcp_tool_defs`)
    // names a target employee. Delegation (also [`DELEGATION_TOOLS`]):
    ("tasks_create", "assigned_to"),
    ("tasks_update", "assigned_to"),
    ("discovery_catalog", "agent_id"),
    // Org placement under another employee.
    ("create_agent", "reports_to"),
    ("agent_update", "reports_to"),
    // Reads of / writes to another employee's work record, schedules,
    // feedback, audit trail, reliability and cost.
    ("tasks_list", "assigned_to"),
    ("activity_list", "agent_id"),
    ("discovery_list", "agent_id"),
    ("list_cron_tasks", "agent_id"),
    ("list_reminders", "agent_id"),
    ("submit_feedback", "agent_id"),
    ("audit_trail_query", "agent_id"),
    ("reliability_summary", "agent_id"),
    ("cost_summary", "agent_id"),
];

/// P1c: tools of [`PERSONAL_TARGET_TOOLS`] that hand work to the target.
/// For these the owner's own work may move between the owner's own Personal
/// employees (`personal_visibility::delegation_refusal`); everything else
/// stays "the employee itself only".
pub(crate) const DELEGATION_TOOLS: &[&str] =
    &["spawn_agent", "send_to_agent", "tasks_create", "tasks_update", "discovery_catalog"];

/// P1c: tools naming a work item by id whose employee is read from
/// `tasks.db` (`personal_visibility::work_item_employee`). `goals_create`,
/// `plan_start`, `plan_update_step` (holder rule: own steps only) and
/// `team_handoff` (role to role inside the member's own host-pinned task)
/// name no other employee and are not listed.
pub(crate) const PERSONAL_ID_TARGETS: &[(&str, &str, visibility::WorkItem)] = &[
    ("tasks_update", "task_id", visibility::WorkItem::Task),
    ("activity_list", "task_id", visibility::WorkItem::Task),
    ("plan_get", "plan_id", visibility::WorkItem::Plan),
];

/// The rule for `tool` on `target`: delegation tools allow the same owner's
/// Personal employees, the rest only the employee itself.
fn target_refusal(home_dir: &Path, tool: &str, acting_agent: &str, target: &str) -> Option<&'static str> {
    if DELEGATION_TOOLS.contains(&tool) {
        visibility::delegation_refusal(home_dir, true, acting_agent, target)
    } else {
        visibility::management_refusal(home_dir, true, acting_agent, target)
    }
}

/// [`PERSONAL_ID_TARGETS`]: the employee of the task / plan a call names by
/// id must not be another person's Personal employee. Unknown ids are left
/// to the tool; an unreadable `tasks.db` refuses (fail closed). A work item
/// of the acting employee itself never reads `users.db`.
pub(crate) fn id_target_refusal(
    home_dir: &Path,
    protected: bool,
    tool: &str,
    arguments: &Value,
    acting_agent: &str,
) -> Option<&'static str> {
    if !protected {
        return None;
    }
    PERSONAL_ID_TARGETS
        .iter()
        .filter(|(name, _, _)| *name == tool)
        .filter_map(|(_, key, item)| {
            let id = arguments.get(*key).and_then(Value::as_str).map(str::trim).filter(|id| !id.is_empty())?;
            Some((id, *item))
        })
        .find_map(|(id, item)| match visibility::work_item_employee(home_dir, item, id) {
            Ok(Some(employee)) if employee == acting_agent => None,
            Ok(Some(employee)) => target_refusal(home_dir, tool, acting_agent, &employee),
            Ok(None) => None,
            Err(visibility::Unavailable) => Some(visibility::OWNERSHIP_UNAVAILABLE),
        })
}

/// Tools that read the store of the MCP process's own employee
/// (`default_agent`) rather than a named argument: an external key (whose
/// acting id is its client id) must not read a Personal employee's store.
pub(crate) const PROCESS_STORE_TOOLS: &[&str] = &["task_status"];

/// Every employee `arguments` name for `tool`: one per listed key whose
/// value is a non-blank string.
pub(crate) fn named_targets<'a>(tool: &'a str, arguments: &'a Value) -> impl Iterator<Item = &'a str> + 'a {
    PERSONAL_TARGET_TOOLS
        .iter()
        .filter(move |(name, _)| *name == tool)
        .filter_map(move |(_, key)| arguments.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|target| !target.is_empty())
}

/// Refusal text when `acting_agent` may not use `tool` on the employee named
/// in `arguments`. `protected` is `runtime_readiness::is_protected_linux()`
/// in production (a parameter so tests need no process environment).
pub(crate) fn personal_target_refusal(
    home_dir: &Path,
    protected: bool,
    tool: &str,
    arguments: &Value,
    acting_agent: &str,
) -> Option<&'static str> {
    if !protected {
        return None;
    }
    named_targets(tool, arguments)
        // Self-targeting needs no ownership read (the mirror check would
        // allow it anyway); an ephemeral sub-agent resolves its parent first.
        .filter(|target| *target != acting_agent)
        .find_map(|target| target_refusal(home_dir, tool, acting_agent, target))
}

/// [`PROCESS_STORE_TOOLS`]: refused when the acting caller is not the
/// process employee and that employee is another person's Personal
/// employee. The process employee itself never reads `users.db`.
pub(crate) fn process_store_refusal(
    home_dir: &Path,
    protected: bool,
    tool: &str,
    acting_agent: &str,
    process_agent: &str,
) -> Option<&'static str> {
    if !protected || !PROCESS_STORE_TOOLS.contains(&tool) || acting_agent == process_agent {
        return None;
    }
    visibility::management_refusal(home_dir, true, acting_agent, process_agent)
}

/// A memory `namespace` argument that resolves to another Personal employee
/// (a bare employee id, or `internal/<id>`). The caller's own namespace (or
/// a sub-namespace of it), `shared/…` and `external/…` never read `users.db`.
/// Runs after `mcp_namespace::assert_can_access`, which already confines a
/// caller to its own namespace and `shared/public`; this is the Personal
/// rule on its own, so a future widening there cannot reach one.
pub(crate) fn namespace_refusal(
    home_dir: &Path,
    protected: bool,
    own_namespace: &str,
    requested: &str,
    acting_agent: &str,
) -> Option<&'static str> {
    if !protected {
        return None;
    }
    let requested = requested.trim();
    let within = |base: &str| {
        !base.is_empty()
            && (requested == base || requested.strip_prefix(base).is_some_and(|rest| rest.starts_with('/')))
    };
    if within(own_namespace) || requested.starts_with("shared/") || requested.starts_with("external/") {
        return None;
    }
    let employee = requested.strip_prefix("internal/").unwrap_or(requested).split('/').next().unwrap_or("");
    if employee.is_empty() || !duduclaw_core::is_valid_agent_id(employee) || employee == acting_agent {
        return None;
    }
    visibility::management_refusal(home_dir, true, acting_agent, employee)
}

/// The P1/P1b refusal for one `tools/call`, before any handler runs.
/// `process_agent` is the process's `default_agent`; `own_namespace` the
/// caller's memory write namespace.
pub(crate) fn pre_dispatch_refusal(
    home_dir: &Path,
    protected: bool,
    tool: &str,
    arguments: &Value,
    acting_agent: &str,
    process_agent: &str,
    own_namespace: &str,
) -> Option<&'static str> {
    if !protected {
        return None;
    }
    personal_target_refusal(home_dir, true, tool, arguments, acting_agent)
        // An isolated call names its task/plan through the signed Task grant,
        // which the boundary resolver checks against live ownership; only the
        // legacy path needs the tasks.db lookup.
        .or_else(|| {
            crate::mcp_isolation::current()
                .is_none()
                .then(|| id_target_refusal(home_dir, true, tool, arguments, acting_agent))
                .flatten()
        })
        .or_else(|| process_store_refusal(home_dir, true, tool, acting_agent, process_agent))
        .or_else(|| {
            let requested = arguments.get("namespace").and_then(Value::as_str)?;
            namespace_refusal(home_dir, true, own_namespace, requested, acting_agent)
        })
}

/// Agents of `step_agents` whose step output `acting_agent` must not see in
/// `task_status`: another person's Personal employees. Blank (the task
/// owner itself) and self steps never read `users.db`; an unreadable store
/// hides every other step's output (fail closed).
pub(crate) fn hidden_step_agents<'a>(
    home_dir: &Path,
    protected: bool,
    acting_agent: &str,
    step_agents: impl IntoIterator<Item = &'a str>,
) -> BTreeSet<String> {
    if !protected {
        return BTreeSet::new();
    }
    let candidates: BTreeSet<&str> = step_agents
        .into_iter()
        .map(str::trim)
        .filter(|agent| !agent.is_empty() && *agent != acting_agent)
        .collect();
    candidates
        .into_iter()
        .filter(|agent| visibility::management_refusal(home_dir, true, acting_agent, agent).is_some())
        .map(str::to_owned)
        .collect()
}

/// `list_agents` with another person's Personal employees removed for
/// `acting_agent` (hidden, not redacted). `caller` keeps its existing role
/// (org visibility). An unreadable ownership store refuses the listing.
pub(crate) async fn list_agents_hiding(
    params: &Value,
    home_dir: &Path,
    caller: &str,
    acting_agent: &str,
    protected: bool,
) -> Value {
    let (home, acting) = (home_dir.to_path_buf(), acting_agent.to_owned());
    let hidden = tokio::task::spawn_blocking(move || visibility::hidden_from_employee(&home, protected, &acting))
        .await;
    match hidden {
        Ok(Ok(hidden)) if hidden.is_empty() => super::handle_list_agents(params, home_dir, caller).await,
        Ok(Ok(hidden)) => super::agents::list_agents_filtered(params, home_dir, caller, &hidden).await,
        _ => super::tool_error(visibility::OWNERSHIP_UNAVAILABLE),
    }
}

#[cfg(all(test, unix))]
#[path = "personal_target_tests.rs"]
mod tests;
