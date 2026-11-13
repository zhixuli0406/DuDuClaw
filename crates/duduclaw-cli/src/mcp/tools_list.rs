//! `tools/list` — authorized declarations, optionally narrowed by usage.
//!
//! ## Authorization filters precede optional discovery pruning
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
    CODRIVE_TOOLS, COMPUTER_USE_TOOLS, COMPUTER_WORKSPACE_TOOLS, DB_SOURCE_TOOLS, FORK_TOOLS,
    OS_NATIVE_TOOLS, RECORDING_TOOLS, SYSTEM_OPERATOR_TOOLS,
};

/// Tools hidden while the Google Workspace integration gate is off
/// (`config.toml [integrations] google_workspace`, default false).
pub(crate) const GOOGLE_WORKSPACE_TOOLS: &[&str] =
    duduclaw_core::google_identity::NATIVE_GOOGLE_TOOLS;

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
/// `code_map` (v1.81, S1) scans a directory tree contained in the acting
/// employee's own directory (`mcp_files::vet_dir`); a caller that is not an
/// employee has no such directory, so it is listed here too.
///
/// Hidden from such callers by [`visible_tools`] and refused to them by the
/// dispatch gate (`mcp_dispatch.rs`, error class `process_agent_tool`), both
/// through [`process_agent_tool_refused`], so discoverable ⇔ callable.
pub(crate) const PROCESS_AGENT_TOOLS: &[&str] = &[
    "code_map",
    "meeting_upload_link",
    "meeting_transcribe",
    "meeting_transcribe_status",
    "meeting_transcribe_cancel",
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

/// Why the process lane refuses `name`, or `None` when it may be listed and
/// called. The dispatch gate (error class `explore_lane`) and
/// [`visible_tools_with`] both read this, so discoverable ⇔ callable.
pub(crate) fn lane_refusal(lane: &duduclaw_core::ProcessLane, name: &str) -> Option<String> {
    use duduclaw_core::ProcessLane;
    let effect = duduclaw_core::effect_of(name);
    match lane {
        ProcessLane::Normal => None,
        ProcessLane::Explore if lane.permits(effect) => None,
        ProcessLane::Explore => Some(format!(
            "Tool '{name}' ({effect}) is not available in the read-only explore lane: \
             this run may only read and draft. Leave the action for a normal run."
        )),
        ProcessLane::Invalid => Some(format!(
            "{} is set to a value other than \"{}\", so this MCP server refuses every \
             tool call (fail closed). Unset it or set it to \"{}\".",
            duduclaw_core::ENV_LANE,
            duduclaw_core::LANE_EXPLORE,
            duduclaw_core::LANE_EXPLORE
        )),
    }
}

/// Whether this process was spawned by the gateway for an employee.
pub(crate) fn employee_process_from_env() -> bool {
    if crate::mcp_isolation::current().is_some() { return true; }
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
        "[agent]\nname = \"Test\"\nstatus = \"active\"\n[capabilities]\n\
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
    visible_tools_in_lane(
        principal,
        home_dir,
        default_agent,
        employee_process,
        &crate::mcp_isolation::process_lane(),
    )
    .await
}

/// [`visible_tools_with`] with the process lane passed in, so tests do not
/// depend on this process's environment.
pub(crate) async fn visible_tools_in_lane(
    principal: &crate::mcp_auth::Principal,
    home_dir: &Path,
    default_agent: &str,
    employee_process: bool,
    lane: &duduclaw_core::ProcessLane,
) -> Vec<&'static ToolDef> {
    // Scoped non-employee callers (standalone `duduclaw mcp init` clients,
    // external keys): list only what the scope gate lets through and what does
    // not act for the process's agent. Discovery filter only.
    let client_is_agent = crate::mcp_namespace::client_is_agent(home_dir, &principal.client_id);
    let scope_listing = scope_listing_applies(principal, client_is_agent, employee_process);
    let dedicated_google = crate::mcp_auth::dedicated_google_call(home_dir, principal).is_some();

    // Per-employee Google identity: an effective `none` hides the group —
    // the same predicate the dispatch gate refuses with.
    // Listing has no accepted execution marker. Select only a current,
    // verified employee for display; execution acceptance stays in dispatch.
    let google_listed = if crate::mcp_auth::dedicated_google_call(home_dir, principal).is_some() {
        duduclaw_core::google_identity::effective_for(home_dir, Some(&principal.client_id))
            .effective
            .tools_listed()
    } else {
        super::google_identity_client::GoogleCaller::for_process(home_dir, &principal.client_id)
            .tools_listed(home_dir)
    };
    let google_enabled =
        duduclaw_gateway::google_workspace::integration_enabled(home_dir) && google_listed;
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
    let caps = if (principal.is_external && !dedicated_google) || effective_agent.is_empty() {
        duduclaw_core::types::CapabilitiesConfig::default()
    } else if is_member {
        member_caps.unwrap_or_default()
    } else {
        let agent_dir = caller_agent_dir(home_dir, effective_agent);
        duduclaw_core::agent_toml::load(&agent_dir).capabilities
    };
    let cap_gate: Option<(Vec<String>, Vec<String>)> =
        if (!principal.is_external || dedicated_google) && !effective_agent.is_empty() {
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
    // Document knowledge base: listed while the feature is on and at least
    // one collection is on the employee side (granted to it or its
    // department, narrowed by `[knowledge] collections`) — the same predicate
    // the call path and the gateway route check
    // (`knowledge_client::available_for`). A grant change moves the set, so
    // the watcher sends `list_changed`.
    let allow_group_history = gated_caller
        && !member_invalid
        && super::group_history::available_for(home_dir, &principal.client_id, default_agent).await;
    let allow_knowledge = gated_caller
        && !member_invalid
        && super::knowledge_client::available_for(home_dir, effective_agent);
    let allow_responsibilities = gated_caller
        && !member_invalid
        && duduclaw_gateway::responsibility::ResponsibilityConfig::from_home(home_dir).enabled;
    // RM-120: generic `odoo_create` / `odoo_write` need an explicit grant and
    // the separate per-table opt-in in the employee's own Odoo settings, and
    // the table must clear the never-writable list, the 功能模組 switch and
    // the block list. The Odoo handler acts for `default_agent`, so the same
    // agent and the same predicate (`generic_write_model_gate` over
    // `allowed_models`) decide the listing.
    let allow_odoo_create =
        !member_invalid && super::generic_write_listed(home_dir, default_agent, "odoo_create");
    let allow_odoo_write =
        !member_invalid && super::generic_write_listed(home_dir, default_agent, "odoo_write");

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
    if gated_caller || dedicated_google {
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
                // RM-103 S3: mirror of the dispatch §3.65 exemption — only the
                // exact intrinsic (`team_handoff`) of a verified role member;
                // every other tool a wildcard entry covers stays hidden.
                if gated_caller
                    && duduclaw_gateway::ephemeral::is_role_member_intrinsic_call(
                        home_dir,
                        effective_agent,
                        tool.name,
                    )
                {
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

    // 2026-10: `[capabilities] action_rules` — a `block`ed tool is hidden
    // (mirror of dispatch §3.455, same `action_rule_verdict`); `ask` stays
    // listed because it is callable after an approval.
    let action_rules = if (gated_caller || dedicated_google) && !member_invalid {
        let agent_dir =
            match duduclaw_gateway::ephemeral::resolve_agent_dir(home_dir, effective_agent) {
                Some(dir) => dir,
                None => home_dir.join("agents").join(effective_agent),
            };
        duduclaw_core::agent_toml::load_action_rules(&agent_dir)
    } else {
        duduclaw_core::ActionRules::default()
    };

    // RM-137: `[delegation] policy = "none"` (or a value that resolves to it)
    // — mirror of dispatch §3.458, same predicate with no arguments, so only
    // tools every call to which is refused are hidden. External callers are
    // outside that gate in dispatch, so they are outside it here too.
    let delegation_none = !principal.is_external
        && duduclaw_core::delegation_policy_from_home(home_dir)
            == duduclaw_core::DelegationPolicy::Disabled;
    // RM-137 (soft identity mode): the handler and the `none` exemption also
    // require a token-verified identity (`caller_token_verified`); a soft-mode
    // `Unverified` process declaring an `eph-*` id gets neither. Only a role
    // member can need it, so the identity check runs for `eph-*` callers only.
    let member_token_verified =
        is_member && super::caller_token_verified(home_dir, effective_agent);
    // RM-137 H1: the `team_handoff` handler refuses every caller that is not a
    // verified role member (`.ephemeral/eph-*` scaffold, `[team_member]`
    // record agreeing with `ephemeral.toml`) with a token-verified identity,
    // so listing follows the same predicate.
    let team_handoff_listed = !principal.is_external
        && member_token_verified
        && duduclaw_gateway::ephemeral::is_role_member_intrinsic_call(
            home_dir,
            effective_agent,
            "team_handoff",
        );

    let tool_allowed_by_capability = |name: &str| -> bool {
        if principal.is_external && !GOOGLE_WORKSPACE_TOOLS.contains(&name) {
            return true;
        }
        let Some((denied, allowed)) = cap_gate.as_ref() else {
            return true; // external / unresolved caller → not gated here
        };
        // The dispatch gate's own predicate (v1.68.1: shared wildcard-aware
        // matcher), so discoverable ⇔ callable.
        duduclaw_core::tool_catalog::tool_list_verdict(name, denied, allowed)
            == duduclaw_core::tool_catalog::ToolListVerdict::Allowed
    };

    // Match dispatch's verified caller classification and fail-closed predicate.
    // Argument-dependent changes (e.g. agent_update) remain discoverable.
    let actor = super::record_actor_for(home_dir, &principal.client_id, default_agent);
    let background_allowed = super::check_background_caller(home_dir, actor, "tools/list").is_ok();

    let agent_dir = caller_agent_dir(home_dir, effective_agent);
    let pruning_mode = if !principal.is_external && !effective_agent.is_empty() {
        duduclaw_core::tool_pruning::read_mode(&agent_dir)
    } else {
        duduclaw_core::tool_pruning::Mode::Off
    };
    let pruning_names: Vec<_> = tools().map(|t| t.name).collect();
    let prunable = if pruning_mode != duduclaw_core::tool_pruning::Mode::Off {
        duduclaw_core::tool_pruning::candidates_from_files(home_dir, &agent_dir, effective_agent, &pruning_names)
    } else {
        std::collections::BTreeSet::new()
    };
    let scoped = duduclaw_gateway::capability_grants::scoped_tools(&agent_dir);

    // v1.81 (S2): `code_map` refuses every caller whose identity does not
    // resolve to an employee directory (role members included). Listed by
    // the handler's own predicate — `DUDUCLAW_AGENT_ID` being set is not
    // enough — so listed ⇒ callable.
    let code_map_listed = crate::mcp_memory_handlers::code_map_caller(
        home_dir,
        &principal.client_id,
        std::env::var(duduclaw_core::ENV_AGENT_ID).ok().as_deref(),
    )
    .is_some();

    tools()
        .filter(|t| crate::mcp_isolation::tool_allowed(t.name, principal))
        // SR2 1a: refused on every transport where the isolation architecture
        // applies, so it is not listed there either (discoverable ⇔ callable).
        .filter(|t| t.name != "create_agent" || !duduclaw_gateway::runtime_readiness::isolation_active())
        .filter(|t| !matches!(t.name, "meeting_transcribe" | "meeting_transcribe_status" | "meeting_transcribe_cancel" | "meeting_upload_link") || meeting_listed(home_dir, principal))
        .filter(|t| background_allowed || !super::creates_background_work(t.name))
        .filter(|t| {
            // C4: discoverable ⇔ callable — same predicate as the dispatch
            // gate (legacy whitelist ∪ explicitly-granted grantable scopes).
            !principal.is_external
                || crate::mcp_auth::external_tool_allowed(t.name, principal)
                || crate::mcp_auth::dedicated_google_tool_allowed(home_dir, principal, t.name)
        })
        .filter(|t| {
            !dedicated_google
                || !GOOGLE_WORKSPACE_TOOLS.contains(&t.name)
                || crate::mcp_auth::dedicated_google_tool_allowed(home_dir, principal, t.name)
        })
        .filter(|t| google_enabled || !GOOGLE_WORKSPACE_TOOLS.contains(&t.name))
        .filter(|t| {
            !matches!(t.name, "google_chat_spaces" | "google_chat_messages")
                || duduclaw_gateway::google_workspace::chat_read_tool_available(home_dir, t.name)
        })
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
        .filter(|t| t.name != "group_history_read" || allow_group_history)
        .filter(|t| allow_knowledge || !super::KNOWLEDGE_TOOLS.contains(&t.name))
        .filter(|t| t.name != "credential_declare" || (gated_caller && !member_invalid))
        .filter(|t| allow_responsibilities || !super::RESPONSIBILITY_TOOLS.contains(&t.name))
        .filter(|t| t.name != "odoo_create" || allow_odoo_create)
        .filter(|t| t.name != "odoo_write" || allow_odoo_write)
        // O7: PORTICO scoped tools without an active grant.
        .filter(|t| {
            (principal.is_external && !GOOGLE_WORKSPACE_TOOLS.contains(&t.name))
                || !scoped_without_grant.contains(t.name)
        })
        // WP-7A bug2: internal per-agent capability filter (mirror of §3.45).
        .filter(|t| tool_allowed_by_capability(t.name))
        // 2026-10: action rules `block` (mirror of §3.455).
        .filter(|t| {
            (principal.is_external && !GOOGLE_WORKSPACE_TOOLS.contains(&t.name))
                || crate::mcp_dispatch::action_rule_verdict(&action_rules, t.name)
                    != Some(duduclaw_core::ActionVerdict::Block)
                // RM-103 S3: mirror of the dispatch exemption for a team role
                // member's intrinsic (`team_handoff`).
                || (gated_caller
                    && duduclaw_gateway::ephemeral::is_role_member_intrinsic_call(
                        home_dir,
                        effective_agent,
                        t.name,
                    ))
        })
        // RM-137: delegation policy `none` (mirror of dispatch §3.458).
        .filter(|t| {
            !delegation_none
                || !crate::mcp_dispatch::delegation_none_refuses_for(
                    home_dir,
                    t.name,
                    &serde_json::Value::Null,
                    effective_agent,
                    || member_token_verified,
                )
        })
        // RM-137 H1: `team_handoff` is callable only by a verified team role
        // member (the handler's own check), so only a verified role member
        // sees it — under every policy.
        .filter(|t| t.name != "team_handoff" || team_handoff_listed)
        // 2026-10: read-only explore lane (mirror of dispatch §0b).
        .filter(|t| lane_refusal(lane, t.name).is_none())
        // Standalone profile (2026-10-07): scope gate + process-agent tools.
        .filter(|t| !scope_listing || scoped_caller_can_call(t.name, principal))
        .filter(|t| {
            !process_agent_tool_refused(t.name, principal, client_is_agent, employee_process)
        })
        .filter(|t| t.name != "code_map" || code_map_listed)
        // Discovery only, LAST: neither grants nor authorization are changed.
        // An active scoped task grant always remains discoverable.
        .filter(|t| pruning_mode != duduclaw_core::tool_pruning::Mode::On
            || !prunable.contains(t.name)
            || (duduclaw_gateway::capability_grants::set_contains_tool(&scoped, t.name)
                && !scoped_without_grant.contains(t.name)))
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
