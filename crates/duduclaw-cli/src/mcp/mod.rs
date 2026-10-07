//! MCP (Model Context Protocol) server implementation.
//!
//! Communicates via stdin/stdout using JSON-RPC 2.0.
//! Exposes DuDuClaw tools for Claude Code integration.

use std::path::Path;

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_core::truncate_bytes;
use duduclaw_memory::SqliteMemoryEngine;
use duduclaw_security::secret_ref::{Secret, SecretRef};
use rusqlite::OptionalExtension;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{info, warn};

// ── Tool definitions ─────────────────────────────────────────

pub(crate) struct ToolDef {
    name: &'static str,
    description: &'static str,
    params: &'static [ParamDef],
}

pub(crate) struct ParamDef {
    name: &'static str,
    description: &'static str,
    required: bool,
}

/// Outcome of an install-class approval gate.
///
/// `pub(crate)`: also reused (unchanged) by `mcp_os_ops::require_factory_reset_approval`
/// (O-0) so the `os_factory_reset` MCP tool's ApprovalBroker gate shares the
/// exact same fail-closed polling semantics instead of re-deriving them.
pub(crate) enum InstallApprovalOutcome {
    /// Admin caller, no gate, or an explicit approval was granted.
    Proceed,
    /// Denied / expired / broker-unavailable — carries a zh-TW user message.
    Denied(String),
}

/// Outcome of one ActionGuard judge run, already collapsed to a two-way verdict
/// with a flag distinguishing a real "risky" ruling from a fail-closed error.
pub(crate) struct ActionGuardOutcome {
    verdict: duduclaw_gateway::approval::JudgeVerdict,
    /// True when the verdict is `Risky` because the judge call/parse failed
    /// (fail-closed), not because the model ruled it irreversible.
    errored: bool,
    /// D1 (WebDreamer arXiv:2411.06559): the judge's structured "what will
    /// the world look like after this call runs" simulation. Empty
    /// ([`duduclaw_gateway::approval::SimulationNarrative::is_empty`]) when
    /// the judge call/parse failed, or the reply simply omitted it — the
    /// verdict decision above never depends on this field.
    narrative: duduclaw_gateway::approval::SimulationNarrative,
    /// H21: the closed-enumeration findings the judge prompt was actually
    /// built from (see `duduclaw_gateway::approval` module docs, "H21"
    /// section). Always populated — computed deterministically before the
    /// judge call, so it survives a judge error/timeout too, and rides along
    /// to the dispatch-layer audit record regardless of verdict.
    findings: Vec<duduclaw_gateway::approval::ActionGuardFinding>,
}

/// Delegation context read from environment variables (or injected for testing).
#[derive(Debug, Clone)]
pub(crate) struct DelegationContext {
    depth: u8,
    origin: Option<String>,
}

impl DelegationContext {
    /// Read from env vars set by the dispatcher. This is the ONLY trusted source
    /// in production — tool params are ignored to prevent LLM agents from spoofing.
    fn from_env() -> Self {
        let depth = std::env::var(duduclaw_core::ENV_DELEGATION_DEPTH)
            .ok()
            .and_then(|v| v.parse::<u8>().ok())
            .unwrap_or(0);
        let origin = std::env::var(duduclaw_core::ENV_DELEGATION_ORIGIN)
            .ok()
            .filter(|s| !s.is_empty());
        Self { depth, origin }
    }
}

// RFC-21 §2: per-agent connector pool replaces the v1.10.1 global singleton.
// Defined in `crate::odoo_pool::OdooConnectorPool`.
pub(crate) type OdooState = std::sync::Arc<crate::odoo_pool::OdooConnectorPool>;

/// Per-channel session aggregates computed from sessions.db.
#[derive(Default, serde::Serialize)]
pub(crate) struct ChannelSessionStats {
    total_sessions: u64,
    /// Sessions whose last_active is within the past 24 hours.
    active_24h: u64,
    /// Thread/topic-scoped sessions (Discord threads, Telegram forum topics).
    thread_sessions: u64,
}

/// WP7 precomputed department-visibility context for a single shared-wiki tool
/// call. Built once per call (resolves the caller's department) and then reused
/// as a cheap per-page predicate — the single READ-isolation decision point
/// behind `wiki_ls/read/search/stats/lint` with `scope="shared"`.
///
/// F4: department **read** isolation is ALWAYS in force and is orthogonal to
/// the `.scope.toml` **write** policy. Declaring the `departments` namespace in
/// `.scope.toml` tightens who may *write*; it must never open cross-department
/// *reads* (the previous `explicit_override` short-circuit did exactly the
/// wrong thing — it disabled read isolation entirely). An agent may only ever
/// read its own department's pages plus the open company layer.
pub(crate) struct DeptVisibility {
    caller_department: Option<String>,
    /// WP2.3 — the `.scope.toml` `visible_to_departments` read filter. Combined
    /// (AND) with the built-in `departments/<dept>/` isolation in [`Self::allows`]
    /// so a namespace declared `visible_to_departments = [...]` is invisible to
    /// agents outside those departments (fail-closed). Empty policy = no extra
    /// restriction (behaviour identical to pre-WP2.3).
    vis_policy: duduclaw_core::DepartmentVisibilityPolicy,
}

impl DeptVisibility {
    fn for_agent(home_dir: &Path, caller_agent: &str) -> Self {
        Self {
            caller_department: resolve_agent_department(home_dir, caller_agent),
            vis_policy: duduclaw_core::DepartmentVisibilityPolicy::load_for_home(home_dir),
        }
    }

    /// Whether the caller may see/touch `page_path` (department dimension only;
    /// `.scope.toml` write policy is checked separately on write/delete).
    /// Combines the built-in `departments/<dept>/` isolation with the
    /// `visible_to_departments` namespace read filter — both must permit.
    fn allows(&self, page_path: &str) -> bool {
        self.vis_policy
            .page_visible(page_path, self.caller_department.as_deref())
    }
}

/// Who the caller is inside its team.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TeamMemberIdentity {
    role: duduclaw_core::types::Role,
    /// The goal task the scaffold pinned this member to, when it did.
    task_id: Option<String>,
    /// The goal-loop round the scaffold pinned, when it did.
    round: Option<u32>,
}

/// Typed projection of the `[team_member]` table WP-2's ephemeral scaffold
/// writes into a role member's `agent.toml`.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
pub(crate) struct TeamMemberSection {
    role: Option<String>,
    task_id: Option<String>,
    round: Option<u32>,
}

// ── Submodules (P10a: mcp.rs was split by tool family) ──────
mod action_guard;
mod agent_admin;
mod agent_update;
mod agents;
pub(crate) mod approval;
pub(crate) mod workflow_operation;
mod audit_reliability;
mod autopilot_skills;
mod belief;
mod bus_tasks;
mod channels;
mod codrive_mail;
mod computer_use_client;
mod config_read;
mod cost;
mod cron_tasks;
mod decision_tools;
mod delegation;
mod dispatch;
mod evolution;
mod exec;
mod google;
mod identity;
mod inference;
mod jsonrpc;
mod media_tools;
mod memory_tools;
mod messaging;
mod notion_github;
mod odoo;
mod os_tools;
mod record_authz;
mod responsibilities;
mod reminders;
mod server;
mod skills;
mod skills_lifecycle;
mod spawn;
mod tasks;
mod discovery;
mod tasks_goals;
mod team_handoff;
mod tools_def;
mod tools_list;
mod util;
mod web;
mod wiki_agent;
mod wiki_maint;
mod wiki_shared;
mod wiki_shared_ops;
mod wiki_util;
mod working_state;

pub(crate) use action_guard::*;
pub(crate) use responsibilities::*;
pub(crate) use agent_admin::*;
pub(crate) use agent_update::*;
pub(crate) use agents::*;
pub(crate) use approval::*;
pub(crate) use audit_reliability::*;
pub(crate) use autopilot_skills::*;
pub(crate) use belief::*;
pub(crate) use bus_tasks::*;
pub(crate) use channels::*;
pub(crate) use codrive_mail::*;
pub(crate) use computer_use_client::handle_computer_use_tool;
pub use config_read::*;
pub(crate) use cost::*;
pub(crate) use cron_tasks::*;
pub(crate) use decision_tools::*;
pub(crate) use delegation::*;
pub(crate) use dispatch::*;
pub(crate) use evolution::*;
pub(crate) use exec::*;
pub(crate) use google::*;
pub(crate) use identity::*;
pub(crate) use inference::*;
pub(crate) use jsonrpc::*;
pub(crate) use media_tools::*;
pub(crate) use memory_tools::*;
pub(crate) use messaging::*;
pub(crate) use notion_github::*;
pub(crate) use odoo::*;
pub(crate) use os_tools::*;
pub(crate) use record_authz::*;
pub(crate) use reminders::*;
pub use server::*;
pub(crate) use skills::*;
pub(crate) use skills_lifecycle::*;
pub(crate) use spawn::*;
pub(crate) use tasks::*;
pub(crate) use discovery::*;
pub(crate) use tasks_goals::*;
pub(crate) use team_handoff::*;
pub(crate) use tools_def::*;
pub(crate) use tools_list::*;
pub(crate) use util::*;
pub(crate) use web::*;
pub(crate) use wiki_agent::*;
pub(crate) use wiki_maint::*;
pub(crate) use wiki_shared::*;
pub(crate) use wiki_shared_ops::*;
pub(crate) use wiki_util::*;
pub(crate) use working_state::*;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod agent_identity_tests;
#[cfg(test)]
mod wiki_schema_tests;
#[cfg(test)]
mod task_board_tests;
#[cfg(test)]
mod working_state_mcp_tests;
#[cfg(test)]
mod team_handoff_mcp_tests;
#[cfg(test)]
mod ephemeral_caller_dir_tests;
#[cfg(test)]
mod mcp_scope_dispatch_tests;
#[cfg(test)]
mod wiki_namespace_tests;
#[cfg(test)]
mod odoo_pool_dispatch_tests;
#[cfg(test)]
mod skill_description_parser_tests;
#[cfg(test)]
mod audit_input_tests;
#[cfg(test)]
mod wp5_install_approval_tests;
#[cfg(test)]
mod office_script_tests;
/// R5: `agent.toml` reader directions inside this module, pinned.
///
/// Three readers here moved onto the shared typed parse point
/// (`duduclaw_core::agent_toml`). Their missing-key directions differ on
/// purpose and the differences are the point:
///
/// * `collect_existing_agent_identifiers` — an unreadable `[agent] name`
///   contributes NOTHING to the reserved-id set. Fail-open: a broken config
///   must not permanently block an id from ever being claimed.
/// * `agent_status_of` — absent / unrecognised ⇒ `None` (indeterminate), which
///   `spawn` deliberately treats as operational for pre-WP4 configs. The value
///   stays a raw `String` on the view so an unknown status is indeterminate
///   here rather than a fatal `AgentConfig` parse error.
/// * the `computer_use` capability gate — absent / wrong-typed ⇒ DENY
///   (fail-closed), the opposite of the two above.
#[cfg(test)]
mod agent_toml_reader_direction_tests;
#[cfg(test)]
mod mail_tool_tests;
/// O7: the `tools/list` fixed-cost budget (golden bytes + description caps).
#[cfg(test)]
mod tools_list_budget_tests;
/// Standalone profile: `tools/list` for scoped non-employee callers.
#[cfg(test)]
mod standalone_listing_tests;
// ── T5 merged-entry-point regression tests (O3 / O4 / O13) ─────────────────
#[cfg(test)]
mod merged_entry_tests;
// Caller ↔ record relationship checks (tasks / cron / reminders / agent_update).
#[cfg(test)]
mod record_authz_tests;
// P2-A: employee responsibility tools.
#[cfg(test)]
mod responsibilities_tests;
// P2-A round 4: host-decided parent for tasks created during a round.
#[cfg(test)]
mod round_parent_tests;
/// Pre-`RecordActor` handler signatures for the existing tests.
#[cfg(test)]
mod caller_shims;
// Task audience (U6) in `tasks_list` / `activity_list`.
#[cfg(test)]
mod task_audience_mcp_tests;
