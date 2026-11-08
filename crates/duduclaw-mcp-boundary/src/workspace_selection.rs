//! H10 issuer narrowing for the file-tool workspace grant.
//!
//! The H9 selector signs `Workspace { employee-work }` (with the brokered H6
//! root) only for the File-route tools. The same handle is also the A
//! execution workspace, so the grant alone would make every listed tool that
//! needs a Workspace eligible: execution tools (`execute_program`, `code_map`,
//! `office_script`, browser recording — HX6, discovery, spawn) and the
//! workspace-scoped E services. No selector chose an execution workspace for
//! them, so the issuer must not sign them: they stay hidden and refused (fail
//! closed) until their own selector exists. This only narrows the snapshot.
use crate::tool_policy::{employee_service_scope, policy, ResourceKind, Route};

/// Whether a signed `Workspace` grant is what would make `name` eligible on a
/// non-File route. `employee_scope` is true when the record keeps its
/// EmployeeScope (an interactive run): scoped Service/Connector tools are
/// eligible through live employee authority and never need the workspace.
/// Unknown tools are never eligible anyway and are left to `eligible`.
pub fn needs_execution_workspace(name: &str, employee_scope: bool) -> bool {
    let Some(policy) = policy(name) else { return false; };
    policy.route != Route::File && policy.required.contains(&ResourceKind::Workspace)
        && !(employee_scope && (employee_service_scope(policy) || policy.route == Route::Connector))
}

#[cfg(test)]
mod tests {
    use super::needs_execution_workspace;
    use crate::tool_policy::{policy, ResourceKind, SPAWN_TOOLS};

    #[test]
    fn file_workspace_never_signs_execution_or_workspace_services() {
        // H13: spawn_agent / spawn_ephemeral are routed in protected mode and
        // need only the run's own Employee grant. This rule neither drops them
        // nor does any Workspace grant get involved for them.
        assert_eq!(SPAWN_TOOLS, ["spawn_agent", "spawn_ephemeral"]);
        for tool in SPAWN_TOOLS {
            let required = policy(tool).expect(tool).required;
            assert!(required.contains(&ResourceKind::Employee) && !required.contains(&ResourceKind::Workspace), "{tool}");
            assert!(!needs_execution_workspace(tool, true), "{tool}");
            assert!(!needs_execution_workspace(tool, false), "{tool}");
        }
        for tool in ["file_read", "csv_read", "xlsx_read", "tasks_update", "computer_session_start",
            "computer_login", "skill_curator_status", "unknown_tool"] {
            assert!(!needs_execution_workspace(tool, true), "{tool}");
        }
        // Still pending a production selector: dropped from the signed set.
        for tool in ["browser_record_start", "browser_record_stop", "execute_program", "code_map", "office_script",
            "inspect_branches", "diff_branches", "fork_cost",
            "discovery_catalog", "discovery_list", "discovery_tree", "discovery_artifact", "discovery_cancel",
            "skill_from_recording", "skill_synthesis_run", "skill_hub_install"] {
            assert!(needs_execution_workspace(tool, true), "{tool}");
            assert!(needs_execution_workspace(tool, false), "{tool}");
        }
        // A task-limited record has no EmployeeScope: the workspace-scoped
        // computer services would depend on the file grant there, too.
        assert!(needs_execution_workspace("computer_session_start", false));
        assert!(!needs_execution_workspace("file_read", false));
    }
}
