//! CD-2 (task brief item 2): resolves and authorizes the identity a whole
//! `codrive_run` call executes as, given the MCP tool's optional `agent`
//! argument. This is the authoritative implementation of a decision that
//! previously lived only inline in `duduclaw-cli/src/mcp.rs::
//! handle_codrive_run` — moved here so it is owned by the module with
//! codrive's design authority and is directly testable via
//! `cargo test -p duduclaw-gateway codrive` (see `tests_identity.rs`),
//! independent of a live MCP round trip. `handle_codrive_run` calls
//! [`resolve_run_identity`] instead of duplicating this logic.
//!
//! Design authority: `commercial/docs/DESIGN-codrive-desktop-2026-08.md`
//! §9's CD-1 carry-forward note ("agent 參數身分語意...現＝覆寫整個呼叫
//! 身分含 capability 再檢＋approval 歸屬，保守解讀防權限逃逸").

use std::path::Path;

/// Why [`resolve_run_identity`] refused a call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunIdentityError {
    /// The resolved id (override or caller) failed
    /// `duduclaw_core::is_valid_agent_id` — a format problem, not a
    /// capability decision.
    InvalidAgentId,
    /// The resolved id's own `agent.toml [capabilities] codrive` is not
    /// `true`. Carries the id that failed, so the caller can stamp it on
    /// its own audit-denial line without re-deriving it.
    CapabilityMissing(String),
}

impl std::fmt::Display for RunIdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunIdentityError::InvalidAgentId => write!(f, "invalid agent id"),
            RunIdentityError::CapabilityMissing(id) => {
                write!(f, "agent {id:?} does not have [capabilities] codrive = true")
            }
        }
    }
}

/// Resolves the identity a whole `codrive_run` call executes as, and
/// authorizes it against THAT identity's own `codrive` capability.
///
/// ## Semantics — confirmed correct this round, now pinned (CD-2 item 2)
///
/// `requested_agent` (the MCP tool's `agent` argument — pass the raw,
/// possibly-`None`/blank value; trimming and blank-filtering happen here)
/// is deliberately a full **identity override**, not merely a display
/// label. When present and non-blank, it replaces `caller_agent_id` (the
/// MCP session's own identity, already resolved by the caller — e.g.
/// `duduclaw-cli`'s `resolve_audit_agent`) for everything that happens
/// next:
///
/// 1. **Capability re-check** (this function): the RETURNED id's own
///    `agent.toml [capabilities] codrive` gates the call — never the
///    calling session's. An agent with `codrive = false` can never be
///    driven via `codrive_run`, even when named from a caller that itself
///    has `codrive = true`. The override cannot launder a capability grant
///    from one agent onto another — this is the "防權限逃逸" DESIGN §9
///    flags.
/// 2. **Approval attribution** (downstream — `driver::run_script`'s
///    `agent_id` parameter, threaded into `step::gate_consequential` →
///    `ApprovalBroker::request_with_simulation(agent_id, ...)`) and every
///    audit-denial line the caller writes file under the SAME returned id.
///    "Who was capability-gated for this" and "who this action is
///    attributed to" can therefore never diverge — see `driver::run_script`
///    module doc for how that id is threaded onward once resolved here.
///
/// This is the conservative reading DESIGN-codrive-desktop-2026-08.md §9's
/// CD-1 carry-forward note flags: a caller cannot use `agent` to attribute
/// an approval to some other agent's name while still running under ITS
/// OWN capability grant — the override is all-or-nothing, never split.
///
/// ### If a future round wants a narrower split
///
/// — `agent` affecting ONLY approval attribution, while capability stays
/// gated on the calling session — that is a deliberate WIDENING of the
/// trust model (an agent could then attribute an action to a name whose
/// `codrive` capability it does not itself control), not a bug fix. The
/// change is NOT a one-line swap: it means checking `.capabilities.codrive`
/// (below) against `caller_agent_id` while still RETURNING
/// `requested_agent.unwrap_or(caller_agent_id)` for attribution — i.e. two
/// different identities would need to flow out of this function instead of
/// one (`Ok(String)` would need to become e.g. `Ok { authorized_by:
/// String, attributed_to: String }`, with only `attributed_to` reaching
/// `run_script`). Re-confirm against DESIGN §6's red lines (agent-seat
/// trust boundary) before making that change.
///
/// ### Fail-closed
///
/// A missing/malformed `agent.toml` resolves to `capabilities.codrive =
/// false` (`duduclaw_core::agent_toml::load`'s own `Err(_) =>
/// AgentTomlSections::default()` fallback — see `CapabilitiesConfig::
/// codrive`'s `#[serde(default)]`), so a typo'd or not-yet-provisioned
/// agent id is refused, never silently allowed through.
pub fn resolve_run_identity(
    home_dir: &Path,
    caller_agent_id: &str,
    requested_agent: Option<&str>,
) -> Result<String, RunIdentityError> {
    let agent_id = requested_agent
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(caller_agent_id)
        .to_string();

    if !duduclaw_core::is_valid_agent_id(&agent_id) {
        return Err(RunIdentityError::InvalidAgentId);
    }

    if !duduclaw_core::agent_toml::load_for_agent(home_dir, &agent_id)
        .capabilities
        .codrive
    {
        return Err(RunIdentityError::CapabilityMissing(agent_id));
    }

    Ok(agent_id)
}
