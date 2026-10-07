//! `tools/list` — the declaration surface, kept equal to the callable surface.
//!
//! ## The rule this file enforces: discoverable ⇔ callable
//!
//! Every schema advertised here is a fixed per-spawn prompt cost, and every
//! schema advertised here that the dispatch gate would then *reject* is a pure
//! loss twice over: the tokens are paid, and the model is invited to plan
//! around a tool it cannot use. So the filter pipeline below mirrors, gate for
//! gate, the deny predicates in [`crate::mcp_dispatch`] — and each gate reads
//! the **same constant** the dispatcher enforces with, so they cannot drift.
//!
//! Hiding a tool here does **not** authorize anything: calling an unlisted tool
//! still hits the real gate and is still refused with the same message. This is
//! a discovery filter, never a security boundary.
//!
//! ## Why hiding is safe now (O7, 2026-09-29)
//!
//! Hiding a tool used to make it permanently unreachable, because the client
//! reads `tools/list` once at session start. The server now declares
//! `tools.listChanged` and emits `notifications/tools/list_changed` when the
//! caller's visible set actually changes (see [`crate::mcp::server`]), so a
//! capability granted mid-session — a PORTICO `capability_request` approval, a
//! dashboard `agent_update`, an operator editing `agent.toml` — brings its
//! tools into view without a restart.

use super::*;

pub(crate) use crate::mcp_dispatch::{
    CODRIVE_TOOLS, COMPUTER_USE_TOOLS, COMPUTER_WORKSPACE_TOOLS, DB_SOURCE_TOOLS, FORK_TOOLS, OS_NATIVE_TOOLS,
    RECORDING_TOOLS, SYSTEM_OPERATOR_TOOLS,
};

/// Tools hidden while the Google Workspace integration gate is off
/// (`config.toml [integrations] google_workspace`, default false).
pub(crate) const GOOGLE_WORKSPACE_TOOLS: &[&str] = &[
    "google_status",
    "gmail_search",
    "gmail_read",
    "gmail_create_draft",
    "calendar_list_events",
    "calendar_create_event",
    "sheets_read",
    "sheets_append",
    "forms_get",
    "forms_list_responses",
    "gtasks_lists",
    "gtasks_list",
    "gtasks_create",
    "gtasks_complete",
    "drive_search",
    "drive_read",
    "docs_read",
    "docs_append",
    "slides_read",
];

/// GitHub native tools, gated by `[integrations] github` exactly the way
/// [`GOOGLE_WORKSPACE_TOOLS`] is gated by `[integrations] google_workspace`
/// (H8, 2026-09 feature audit). `github_issue_comment` posts a publicly
/// visible comment, so "a token exists in the vault" was never a safe proxy
/// for "every agent may use it".
pub(crate) const GITHUB_WORKSPACE_TOOLS: &[&str] = &[
    "github_status",
    "github_search_issues",
    "github_issue_read",
    "github_pr_read",
    "github_issue_comment",
];

/// Tools whose handler acts for the MCP **process's** agent (`default_agent`,
/// i.e. `DUDUCLAW_AGENT_ID` or `[general] default_agent`) instead of the
/// caller's own namespace.
///
/// For an employee that is the same identity, so these work. For a caller that
/// is not an employee (a standalone `duduclaw mcp init` client, an external
/// key) they either fail (`working_state_*` answer `unknown agent: <id>`,
/// `office_script` needs an agent directory, `mail_*` need the gateway's mail
/// worker, `shared_wiki_delete` judges authorship by the process agent) or
/// read and write another agent's data (`memory_search_by_layer` and the
/// three consolidation reads look at the process agent's memories, `canvas_*`
/// draw on the process agent's dashboard canvas, `wiki_namespace_status`
/// reports the process agent's department). Measured 2026-10-07 on an
/// isolated home with the 1.70.1 binary; see `docs/guides/mcp-standalone.md`.
///
/// Hidden from such callers by [`visible_tools`] and refused to them by the
/// dispatch gate (`mcp_dispatch.rs`, error class `process_agent_tool`), both
/// through [`process_agent_tool_refused`], so discoverable ⇔ callable.
pub(crate) const PROCESS_AGENT_TOOLS: &[&str] = &[
    "working_state_get",
    "working_state_set",
    "working_state_clear",
    "working_state_handoff",
    "memory_search_by_layer",
    "memory_successful_conversations",
    "memory_episodic_pressure",
    "memory_consolidation_status",
    "shared_wiki_delete",
    "wiki_namespace_status",
    "canvas_push",
    "canvas_clear",
    "team_handoff",
    "mail_list",
    "mail_read",
    "mail_send",
    "office_script",
];

/// Does `tools/list` for this caller list only the tools its scopes reach?
///
/// True for every principal that holds no `admin` scope and is not an AI
/// employee: not the gateway-internal key, not a per-agent key (whose
/// `client_id` names an existing `agents/<id>/agent.toml`), and, for an
/// internal key, not a process the gateway spawned for an employee
/// (`DUDUCLAW_AGENT_ID` set). External keys are included: the dispatch gate's
/// scope check applies to them too, so the legacy whitelist was listed but
/// not callable without the matching scope.
///
/// An `admin` holder passes every scope check, so its listing is unchanged.
pub(crate) fn scope_listing_applies(
    principal: &crate::mcp_auth::Principal,
    client_is_agent: bool,
    employee_process: bool,
) -> bool {
    if principal.scopes.contains(&crate::mcp_auth::Scope::Admin) {
        return false;
    }
    if principal.is_external {
        return true;
    }
    !principal.client_id.is_empty()
        && principal.client_id != duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID
        && !client_is_agent
        && !employee_process
}

/// For a caller [`scope_listing_applies`] to: is `name` callable?
///
/// The tool's minimum scope must be one the caller holds (the dispatch gate's
/// own table, `tool_requires_scope`, read here so the two cannot drift), and
/// the tool must not act for the process's agent ([`PROCESS_AGENT_TOOLS`]).
pub(crate) fn scoped_caller_can_call(name: &str, principal: &crate::mcp_auth::Principal) -> bool {
    !PROCESS_AGENT_TOOLS.contains(&name)
        && crate::mcp_auth::tool_requires_scope(name)
            .is_some_and(|required| principal.scopes.contains(&required))
}

/// Must this caller be kept away from `name` because the tool acts for the
/// process's agent ([`PROCESS_AGENT_TOOLS`])?
///
/// True for an external key whatever scopes it holds (an external key is
/// never an employee; `admin` on one only widens within the externally
/// grantable set) and for every other caller [`scope_listing_applies`] to.
/// `tools/list` hides and the dispatch gate refuses by this one predicate.
pub(crate) fn process_agent_tool_refused(
    name: &str,
    principal: &crate::mcp_auth::Principal,
    client_is_agent: bool,
    employee_process: bool,
) -> bool {
    PROCESS_AGENT_TOOLS.contains(&name)
        && (principal.is_external
            || scope_listing_applies(principal, client_is_agent, employee_process))
}

/// Whether this process was spawned by the gateway for an employee.
pub(crate) fn employee_process_from_env() -> bool {
    std::env::var(duduclaw_core::ENV_AGENT_ID).is_ok_and(|v| !v.trim().is_empty())
}

/// Test helper: tools/list needs a home_dir (the Google and, since H8, GitHub
/// integration gates). An empty tempdir has no config.toml, so both gates read
/// closed (the fail-closed default) and neither group is listed.
///
/// The internal (`is_external = false`) principal carries `admin`, like the
/// gateway-internal key it stands for; without it the scoped-caller listing
/// rule would hide every tool from a non-agent client id.
#[cfg(test)]
pub(crate) fn test_principal(is_external: bool) -> crate::mcp_auth::Principal {
    let scopes = if is_external {
        std::collections::HashSet::new()
    } else {
        [crate::mcp_auth::Scope::Admin].into_iter().collect()
    };
    crate::mcp_auth::Principal {
        client_id: "test".into(),
        scopes,
        is_external,
        created_at: chrono::Utc::now(),
    }
}

#[cfg(test)]
pub(crate) fn tmp_home_for_tools_list() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

/// Test helper: a home whose caller agent (`test`, the id
/// [`test_principal`] resolves to) has every deny-by-default master switch
/// turned on.
///
/// O7 made those switches hide their tools from `tools/list`, so a test that
/// inspects the *schema shape* of a capability-gated tool must opt the caller
/// in first — exactly as an operator would. Tests about the gate itself keep
/// using the bare [`tmp_home_for_tools_list`].
#[cfg(test)]
pub(crate) fn tmp_home_with_all_capabilities() -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("tempdir");
    let dir = home.path().join("agents").join("test");
    std::fs::create_dir_all(&dir).expect("agent dir");
    std::fs::write(
        dir.join("agent.toml"),
        "[capabilities]\n\
         os_native = true\n\
         recording = true\n\
         system_operator = true\n\
         codrive = true\n\
         computer_use = true\n\
         db_sources = [\"demo\"]\n\
         \n[fork]\nenabled = true\n",
    )
    .expect("agent.toml");
    home
}

#[cfg(test)]
pub(crate) async fn handle_tools_list(
    id: &Value,
    principal: &crate::mcp_auth::Principal,
    home_dir: &Path,
) -> Value {
    handle_tools_list_for_agent(id, principal, home_dir, "").await
}

pub(crate) async fn handle_tools_list_for_agent(
    id: &Value,
    principal: &crate::mcp_auth::Principal,
    home_dir: &Path,
    default_agent: &str,
) -> Value {
    let tools: Vec<Value> = visible_tools(principal, home_dir, default_agent)
        .await
        .into_iter()
        .map(build_tool_schema)
        .collect();
    jsonrpc_response(id, serde_json::json!({ "tools": tools }))
}

/// The exact set of tool names this caller would be shown right now.
///
/// Extracted from [`handle_tools_list_for_agent`] so the stdio server can poll
/// it and tell — by comparing sets, not by guessing at file mtimes — whether a
/// `notifications/tools/list_changed` is owed.
pub(crate) async fn visible_tool_names(
    principal: &crate::mcp_auth::Principal,
    home_dir: &Path,
    default_agent: &str,
) -> Vec<&'static str> {
    visible_tools(principal, home_dir, default_agent)
        .await
        .into_iter()
        .map(|t| t.name)
        .collect()
}

/// The filter pipeline. Each stage mirrors one dispatch-gate deny predicate.
async fn visible_tools(
    principal: &crate::mcp_auth::Principal,
    home_dir: &Path,
    default_agent: &str,
) -> Vec<&'static ToolDef> {
    visible_tools_with(
        principal,
        home_dir,
        default_agent,
        employee_process_from_env(),
    )
    .await
}

/// [`visible_tools`] with the "spawned for an employee" fact passed in, so
/// tests do not depend on this process's environment.
pub(crate) async fn visible_tools_with(
    principal: &crate::mcp_auth::Principal,
    home_dir: &Path,
    default_agent: &str,
    employee_process: bool,
) -> Vec<&'static ToolDef> {
    // Scoped non-employee callers (standalone `duduclaw mcp init` clients,
    // external keys): list only what the scope gate lets through and what does
    // not act for the process's agent. Discovery filter only.
    let client_is_agent = crate::mcp_namespace::client_is_agent(home_dir, &principal.client_id);
    let scope_listing = scope_listing_applies(principal, client_is_agent, employee_process);

    let google_enabled = duduclaw_gateway::google_workspace::integration_enabled(home_dir);
    // H8: same deny-by-default gate for GitHub — discoverable ⇔ callable.
    let github_enabled = duduclaw_gateway::github_workspace::integration_enabled(home_dir);

    // WP-7A bug2: for an INTERNAL agent caller, advertise only the tools it can
    // actually call — mirror the dispatch gate's `denied_tools`/`allowed_tools`
    // predicate (`McpDispatcher` §3.45) so "discoverable ⇔ callable". Before
    // this, a capability-restricted agent still received all ~200 MCP tool
    // schemas even though the gate would reject the calls. Keyed on
    // the effective agent id resolved by the dispatch gate (including an
    // internal key's default agent and ephemeral role members).
    //
    // Read once (not per tool). Registered agents use the preset-aware reader;
    // ephemeral members use the exact policy-only parse the dispatch gate
    // uses, so a malformed capability cannot make a tool visible but blocked.
    // An invalid ephemeral scaffold is denied all tools in both paths.
    // External
    // clients are filtered separately by `external_tool_allowed` and carry no
    // per-agent capability config.
    let effective_agent = acting_agent_id(&principal.client_id, default_agent);
    let is_member =
        !principal.is_external && duduclaw_gateway::ephemeral::is_ephemeral_id(effective_agent);
    let member_caps = is_member
        .then(|| duduclaw_gateway::ephemeral::resolve_agent_dir(home_dir, effective_agent))
        .flatten()
        .and_then(|dir| std::fs::read_to_string(dir.join("agent.toml")).ok())
        .and_then(|raw| toml::from_str::<crate::mcp_dispatch::PolicyOnlyConfig>(&raw).ok())
        .map(|cfg| cfg.capabilities);
    let member_invalid = is_member && member_caps.is_none();
    let caps = if principal.is_external || effective_agent.is_empty() {
        duduclaw_core::types::CapabilitiesConfig::default()
    } else if is_member {
        member_caps.unwrap_or_default()
    } else {
        let agent_dir = caller_agent_dir(home_dir, effective_agent);
        duduclaw_core::agent_toml::load(&agent_dir).capabilities
    };
    let cap_gate: Option<(Vec<String>, Vec<String>)> =
        if !principal.is_external && !effective_agent.is_empty() {
            // Raw config fields (NOT the computed accessors) to match the gate.
            // A vanished/invalid ephemeral scaffold must advertise no tools.
            Some((
                caps.denied_tools.clone(),
                if member_invalid {
                    vec!["__invalid_ephemeral__".into()]
                } else {
                    caps.allowed_tools.clone()
                },
            ))
        } else {
            None
        };

    // ── Per-agent master switches (O7) ──────────────────────────────────
    // Five deny-by-default capability switches the dispatcher enforces
    // (`mcp_dispatch` §3.62 / §3.625 / §3.626 / §3.627 and the Computer Use
    // arm in `mcp::dispatch`). Each is `false` for a freshly-scaffolded agent
    // and for any caller whose config could not be read, so the default answer
    // is "not listed" — matching the gate's fail-closed answer exactly.
    //
    // `member_invalid` already forces an empty allowlist above, which hides
    // everything; these flags are additionally forced false so an invalid
    // scaffold can never surface a capability-gated tool through some other
    // path.
    let gated_caller = !principal.is_external && !effective_agent.is_empty();
    let allow_os_native = gated_caller && !member_invalid && caps.os_native;
    let allow_recording = gated_caller && !member_invalid && caps.recording;
    let allow_system_operator = gated_caller && !member_invalid && caps.system_operator;
    let allow_codrive = gated_caller && !member_invalid && caps.codrive;
    let allow_computer_use = gated_caller && !member_invalid && caps.computer_use;
    // P2-C: the workspace tools also need the employee's own switch.
    let allow_cu_workspace = allow_computer_use && caps.computer_use_config.workspace;
    // WP-D §13.7: the four `db_*` tools are deny-by-default per agent, so an
    // agent with no `[capabilities] db_sources` grant must not even see them
    // (discoverable ⊆ callable, same rule the `os_*` family follows). Read
    // from the same canonical preset-aware reader as the gate.
    //
    // External callers can never hold `db:read` (not externally grantable) and
    // an unresolved caller has no agent config to consult — in both cases the
    // dispatch gate denies, so hide the tools here too.
    let allow_db_sources = gated_caller && !member_invalid && !caps.db_sources.is_empty();
    // P2-A: the responsibility tools refuse every call while the feature is
    // off, and only an employee identity can use them — hidden otherwise.
    let allow_responsibilities = gated_caller
        && !member_invalid
        && duduclaw_gateway::responsibility::ResponsibilityConfig::from_home(home_dir).enabled;

    // RFC-26: `[fork] enabled` is opt-in per agent and `mcp_fork`'s handlers
    // refuse every fork tool without it. Read through the same shared typed
    // parse point the handlers use so the two answers are the same answer.
    let allow_fork = gated_caller
        && !member_invalid
        && crate::mcp_fork::load_fork_settings(home_dir, effective_agent).enabled;

    // ── PORTICO task-scoped grants (O7) ─────────────────────────────────
    // A tool named in `[capabilities] scoped_tools` is denied until the agent
    // holds an ACTIVE grant for it (`mcp_dispatch` §3.65). Those grants are
    // minted and revoked mid-session, which is exactly why this file now has a
    // `list_changed` companion: the tool appears the moment the grant lands and
    // disappears when the task phase ends.
    //
    // Zero work for the overwhelming majority of agents — an empty
    // `scoped_tools` set short-circuits before the store is even opened.
    // Fail-closed on a store error, matching `has_active_grant`'s own posture.
    let mut scoped_without_grant: std::collections::HashSet<&'static str> =
        std::collections::HashSet::new();
    if gated_caller {
        let agent_dir =
            match duduclaw_gateway::ephemeral::resolve_agent_dir(home_dir, effective_agent) {
                Some(dir) => dir,
                None => home_dir.join("agents").join(effective_agent),
            };
        let scoped = duduclaw_gateway::capability_grants::scoped_tools(&agent_dir);
        if !scoped.is_empty() {
            let store =
                duduclaw_gateway::capability_grants::CapabilityGrantStore::open(home_dir).ok();
            for tool in tools() {
                if !duduclaw_gateway::capability_grants::set_contains_tool(&scoped, tool.name) {
                    continue;
                }
                let granted = match store.as_ref() {
                    Some(s) => s.has_active_grant(effective_agent, tool.name).await,
                    None => false,
                };
                if !granted {
                    scoped_without_grant.insert(tool.name);
                }
            }
        }
    }

    let tool_allowed_by_capability = |name: &str| -> bool {
        let Some((denied, allowed)) = cap_gate.as_ref() else {
            return true; // external / unresolved caller → not gated here
        };
        // The dispatch gate's own predicate (v1.68.1: shared wildcard-aware
        // matcher), so discoverable ⇔ callable.
        duduclaw_core::tool_catalog::tool_list_verdict(name, denied, allowed)
            == duduclaw_core::tool_catalog::ToolListVerdict::Allowed
    };

    tools()
        .filter(|t| {
            // C4: discoverable ⇔ callable — same predicate as the dispatch
            // gate (legacy whitelist ∪ explicitly-granted grantable scopes).
            !principal.is_external || crate::mcp_auth::external_tool_allowed(t.name, principal)
        })
        .filter(|t| google_enabled || !GOOGLE_WORKSPACE_TOOLS.contains(&t.name))
        .filter(|t| github_enabled || !GITHUB_WORKSPACE_TOOLS.contains(&t.name))
        // WP-D §13.7: hide the SQL connector from agents with no grant.
        .filter(|t| allow_db_sources || !DB_SOURCE_TOOLS.contains(&t.name))
        // O7: the five per-agent master switches, deny-by-default.
        .filter(|t| allow_os_native || !OS_NATIVE_TOOLS.contains(&t.name))
        .filter(|t| allow_recording || !RECORDING_TOOLS.contains(&t.name))
        .filter(|t| allow_system_operator || !SYSTEM_OPERATOR_TOOLS.contains(&t.name))
        .filter(|t| allow_codrive || !CODRIVE_TOOLS.contains(&t.name))
        .filter(|t| allow_computer_use || !COMPUTER_USE_TOOLS.contains(&t.name))
        .filter(|t| allow_cu_workspace || !COMPUTER_WORKSPACE_TOOLS.contains(&t.name))
        .filter(|t| allow_fork || !FORK_TOOLS.contains(&t.name))
        .filter(|t| {
            allow_responsibilities || !super::RESPONSIBILITY_TOOLS.contains(&t.name)
        })
        // O7: PORTICO scoped tools without an active grant.
        .filter(|t| !scoped_without_grant.contains(t.name))
        // WP-7A bug2: internal per-agent capability filter (mirror of §3.45).
        .filter(|t| tool_allowed_by_capability(t.name))
        // Standalone profile (2026-10-07): scope gate + process-agent tools.
        .filter(|t| !scope_listing || scoped_caller_can_call(t.name, principal))
        .filter(|t| {
            !process_agent_tool_refused(t.name, principal, client_is_agent, employee_process)
        })
        .collect()
}

/// The tool names carried by a `tools/list` response.
///
/// Reading the answer back is cheaper and more honest than re-deriving the set:
/// the watcher then compares against what was literally sent.
pub(crate) fn advertised_tool_names(response: &Value) -> Vec<String> {
    response["result"]["tools"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| t["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}
