//! H10 issuer narrowing for the `Workspace { employee-work }` grant.
//!
//! The H9 selector signs `Workspace { employee-work }` (with the brokered H6
//! root) for the File-route tools and — HX5 — for the in-run helper tools
//! (`IN_RUN_HELPER_TOOLS`) when spawnd advertises `helper_calls` and a work
//! broker is installed. The same handle is also the A execution workspace, so
//! the grant alone would make every listed tool that needs a Workspace
//! eligible: other execution tools (discovery, forks) and the
//! workspace-scoped E services. HX6: the browser recording tools are signed
//! only with their own recording selector (`drops_from_signed_with`); SFR:
//! `skill_from_recording` only with its own (`RECORDING_DISTILL_TOOLS`, the
//! same host gate). (H13
//! `spawn_agent` / `spawn_ephemeral` need only the Employee grant and are not
//! affected.) No selector chose a
//! workspace for them, so the issuer must not sign them: they stay hidden and
//! refused (fail closed) until their own selector exists. This only narrows
//! the snapshot.
use crate::tool_policy::{employee_service_scope, policy, ResourceKind, Route, IN_RUN_HELPER_TOOLS, IN_RUN_RECORDING_TOOLS};

/// The reviewed production selector gap: tools that rest on the
/// `employee-work` grant but have no workspace selector yet. The issuer never
/// signs them (`drops_from_signed`), and the CLI parity test
/// (`mcp_isolation_parity_tests::pending_production_selectors`) uses this very
/// list as its tracked gap. HX5 removed `execute_program`, `code_map` and
/// `office_script`; H13 removed `spawn_agent` and `spawn_ephemeral` (routed
/// on the run's own Employee grant, no Workspace); H12b/H12c removed
/// `skill_hub_install` (own Employee + PublicOrigins) and `skill_synthesis_run`
/// (own Employee only); HX6 removed `browser_record_start` /
/// `browser_record_stop` (own recording selector); SFR removed
/// `skill_from_recording` (H12d's `RecordingJobs` seam is installed over HX6's
/// store at boot, every artifact goes through `verify_artifact`, and its own
/// selector [`RECORDING_DISTILL_TOOLS`] signs it only behind the recording
/// gate). None of them may be added back here.
pub const PENDING_WORKSPACE_SELECTOR_TOOLS: &[&str] = &["inspect_branches", "diff_branches", "fork_cost", "discovery_catalog", "discovery_list", "discovery_tree", "discovery_artifact",
    "discovery_cancel"];

/// SFR: distilling a recording needs the brokered workspace that holds the
/// recordings, and is offered only where recording is available (the HX6
/// gate: spawnd `helper_calls` + `helper_recording`, the employee's
/// `[isolation] public_web`, an interactive run) and the profile allows it.
pub const RECORDING_DISTILL_TOOLS: &[&str] = &["skill_from_recording"];

/// Which issuer selectors chose the `Workspace { employee-work }` grant for
/// non-File tools. Each one signs exactly its own tool set.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorkspaceSelectors {
    /// HX5: `IN_RUN_HELPER_TOOLS`.
    pub helpers: bool,
    /// HX6: `IN_RUN_RECORDING_TOOLS`.
    pub recording: bool,
    /// SFR: [`RECORDING_DISTILL_TOOLS`].
    pub distill: bool,
}

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

/// Whether the issuer leaves `name` out of a capability that carries the
/// selected `Workspace { employee-work }` grant. `helpers` is true only when
/// the selector chose that grant for the in-run helper tools (spawnd
/// `helper_calls` advertised and a work broker installed); then exactly the
/// `IN_RUN_HELPER_TOOLS` are signed. Every pending tool, and every other tool
/// whose eligibility would rest on the grant, is dropped (fail closed).
pub fn drops_from_signed(name: &str, employee_scope: bool, helpers: bool) -> bool {
    drops_from_signed_with(name, employee_scope, helpers, false)
}

/// [`drops_from_signed`] plus the HX6 recording selector: `recording` is true
/// only when the issuer chose the workspace for the recording tools (spawnd
/// `helper_calls` + `helper_recording`, the employee's admin-only public-web
/// egress switch, an interactive run). Then exactly `IN_RUN_RECORDING_TOOLS`
/// are signed as well.
pub fn drops_from_signed_with(name: &str, employee_scope: bool, helpers: bool, recording: bool) -> bool {
    drops_from_signed_selected(name, employee_scope, WorkspaceSelectors { helpers, recording, distill: false })
}

/// [`drops_from_signed_with`] plus the SFR distill selector: with `distill`
/// exactly [`RECORDING_DISTILL_TOOLS`] are signed as well.
pub fn drops_from_signed_selected(name: &str, employee_scope: bool, selected: WorkspaceSelectors) -> bool {
    if PENDING_WORKSPACE_SELECTOR_TOOLS.contains(&name) { return true; }
    if selected.helpers && IN_RUN_HELPER_TOOLS.contains(&name) { return false; }
    if selected.recording && IN_RUN_RECORDING_TOOLS.contains(&name) { return false; }
    if selected.distill && RECORDING_DISTILL_TOOLS.contains(&name) { return false; }
    needs_execution_workspace(name, employee_scope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_policy::SPAWN_TOOLS;

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
            assert!(!PENDING_WORKSPACE_SELECTOR_TOOLS.contains(&tool), "{tool}");
            for (scoped, helpers) in [(true, false), (true, true), (false, false), (false, true)] {
                assert!(!drops_from_signed(tool, scoped, helpers), "{tool}");
            }
        }
        // H12b/H12c: routed on the own Employee grant (+ PublicOrigins for the
        // hub), never on a Workspace; H12d's recording tool has its own
        // selector (SFR, `sfr_skill_from_recording_signed_only_with_distill_selector`).
        for tool in ["skill_hub_install", "skill_synthesis_run"] {
            assert!(!policy(tool).expect(tool).required.contains(&ResourceKind::Workspace), "{tool}");
            assert!(!PENDING_WORKSPACE_SELECTOR_TOOLS.contains(&tool), "{tool}");
            for (scoped, helpers) in [(true, false), (true, true), (false, false), (false, true)] {
                assert!(!drops_from_signed(tool, scoped, helpers), "{tool}");
            }
        }
        assert!(!PENDING_WORKSPACE_SELECTOR_TOOLS.contains(&"skill_from_recording"));
        for tool in ["file_read", "csv_read", "xlsx_read", "tasks_update", "computer_session_start",
            "computer_login", "skill_curator_status", "unknown_tool"] {
            assert!(!needs_execution_workspace(tool, true), "{tool}");
            assert!(!drops_from_signed(tool, true, false), "{tool}");
            assert!(!drops_from_signed(tool, true, true), "{tool}");
        }
        // The in-run helper tools rest on the grant: signed only when the
        // selector chose the workspace for them.
        for tool in IN_RUN_HELPER_TOOLS {
            assert!(needs_execution_workspace(tool, true), "{tool}");
            assert!(drops_from_signed(tool, true, false) && drops_from_signed(tool, false, false), "{tool}");
            assert!(!drops_from_signed(tool, true, true) && !drops_from_signed(tool, false, true), "{tool}");
        }
        // A task-limited record has no EmployeeScope: the workspace-scoped
        // computer services would depend on the file grant there, too.
        assert!(needs_execution_workspace("computer_session_start", false));
        assert!(drops_from_signed("computer_session_start", false, true));
        assert!(!needs_execution_workspace("file_read", false));
    }

    /// The shared pending list is exactly reviewed: every entry really rests on
    /// the grant (so the policy-derived rule drops it too), none is an HX5
    /// helper, and the helper bit never signs one.
    #[test]
    fn pending_list_is_policy_backed_and_excludes_helpers() {
        for tool in PENDING_WORKSPACE_SELECTOR_TOOLS {
            assert!(needs_execution_workspace(tool, true) && needs_execution_workspace(tool, false), "{tool}");
            assert!(!IN_RUN_HELPER_TOOLS.contains(tool), "{tool}");
            for (scoped, helpers) in [(true, false), (true, true), (false, false), (false, true)] {
                assert!(drops_from_signed(tool, scoped, helpers), "{tool}");
            }
        }
        let mut sorted = PENDING_WORKSPACE_SELECTOR_TOOLS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), PENDING_WORKSPACE_SELECTOR_TOOLS.len(), "no duplicates");
        // Even with the helper workspace selected, only the three helpers
        // become signable among the tools that need the grant.
        let mut newly: Vec<_> = crate::tool_policy::POLICIES.iter().map(|policy| policy.name)
            .filter(|name| drops_from_signed(name, true, false) && !drops_from_signed(name, true, true)).collect();
        let mut helpers = IN_RUN_HELPER_TOOLS.to_vec();
        helpers.sort_unstable();
        newly.sort_unstable();
        assert_eq!(newly, helpers);
    }

    /// HX6: the recording tools left the pending list; they are signed only
    /// with the recording selector, never by the foreground helper bit.
    #[test]
    fn recording_tools_signed_only_with_recording_selector() {
        for tool in IN_RUN_RECORDING_TOOLS {
            assert!(!PENDING_WORKSPACE_SELECTOR_TOOLS.contains(&tool), "{tool}");
            assert!(needs_execution_workspace(tool, true), "{tool}");
            for scoped in [true, false] {
                assert!(drops_from_signed(tool, scoped, true), "{tool}");
                assert!(drops_from_signed_with(tool, scoped, false, false), "{tool}");
                assert!(!drops_from_signed_with(tool, scoped, false, true), "{tool}");
            }
        }
        let mut newly: Vec<_> = crate::tool_policy::POLICIES.iter().map(|policy| policy.name)
            .filter(|name| drops_from_signed_with(name, true, false, false) && !drops_from_signed_with(name, true, false, true))
            .collect();
        newly.sort_unstable();
        assert_eq!(newly, vec!["browser_record_start", "browser_record_stop"]);
        assert!(drops_from_signed_with("skill_from_recording", true, true, true), "only its own selector signs it");
    }

    /// SFR: `skill_from_recording` left the pending list (the list is now the
    /// fork/discovery readers only) and is signed exactly with its own distill
    /// selector, never by the helper or recording bits.
    #[test]
    fn sfr_skill_from_recording_signed_only_with_distill_selector() {
        assert_eq!(RECORDING_DISTILL_TOOLS, ["skill_from_recording"]);
        let mut pending = PENDING_WORKSPACE_SELECTOR_TOOLS.to_vec();
        pending.sort_unstable();
        assert_eq!(pending, ["diff_branches", "discovery_artifact", "discovery_cancel", "discovery_catalog",
            "discovery_list", "discovery_tree", "fork_cost", "inspect_branches"], "fork/discovery readers only");
        let tool = "skill_from_recording";
        assert!(needs_execution_workspace(tool, true) && needs_execution_workspace(tool, false));
        for scoped in [true, false] {
            for (helpers, recording) in [(false, false), (true, false), (false, true), (true, true)] {
                let without = WorkspaceSelectors { helpers, recording, distill: false };
                assert!(drops_from_signed_selected(tool, scoped, without), "{scoped} {without:?}");
                assert!(!drops_from_signed_selected(tool, scoped, WorkspaceSelectors { distill: true, ..without }));
            }
        }
        // The distill bit signs nothing else.
        let distill = WorkspaceSelectors { distill: true, ..WorkspaceSelectors::default() };
        let newly: Vec<_> = crate::tool_policy::POLICIES.iter().map(|policy| policy.name)
            .filter(|name| drops_from_signed_selected(name, true, WorkspaceSelectors::default())
                && !drops_from_signed_selected(name, true, distill)).collect();
        assert_eq!(newly, vec![tool]);
    }
}
