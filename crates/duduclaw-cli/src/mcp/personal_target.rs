//! Cross-agent MCP tools never act on another person's Personal employee
//! (native Linux isolation v1.81, task P1, SR2 finding 5).
//!
//! In protected mode an AI caller (any MCP principal: the internal key of a
//! running employee, a per-agent key or an external key) that names ANOTHER
//! employee in one of [`PERSONAL_TARGET_TOOLS`] is refused when that employee
//! is a Personal employee (users.db `isolation_personal_agents` mirror, read
//! fresh per call). Only the Personal employee itself may still use these
//! tools on itself (subject to every existing gate); its human owner manages
//! it from the dashboard. Shared targets and non-protected installs are
//! unchanged. The decision is `duduclaw_gateway::personal_visibility::
//! management_refusal`, shared with the gateway's dashboard gates.
use serde_json::Value;
use std::path::Path;

/// Tools whose argument names another employee whose configuration, files,
/// wiki, skills, mailbox or identity the call reads or changes, with the
/// argument key. An absent/blank argument means "the caller itself".
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
];

/// The employee `arguments` name for `tool`, if the tool is listed and the
/// argument is a non-blank string.
pub(crate) fn named_target<'a>(tool: &str, arguments: &'a Value) -> Option<&'a str> {
    let (_, key) = PERSONAL_TARGET_TOOLS.iter().find(|(name, _)| *name == tool)?;
    arguments.get(*key).and_then(Value::as_str).map(str::trim).filter(|target| !target.is_empty())
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
    let target = named_target(tool, arguments)?;
    if target == acting_agent {
        // Self-targeting needs no ownership read (the mirror check below
        // would allow it anyway).
        return None;
    }
    duduclaw_gateway::personal_visibility::management_refusal(home_dir, true, acting_agent, target)
}

#[cfg(all(test, unix))]
#[path = "personal_target_tests.rs"]
mod tests;
