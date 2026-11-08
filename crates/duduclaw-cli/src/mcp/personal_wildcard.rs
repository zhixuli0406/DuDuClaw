//! Cross-agent MCP listings and external callers (native Linux isolation
//! v1.81, task P1d). Protected mode only.
//!
//! - **Wildcard listings.** `tasks_list assigned_to:"*"`, `activity_list
//!   agent_id:"*"` and `goals_list` (always company-wide) leave out every row
//!   of a Personal employee other than the acting employee itself
//!   (`personal_visibility::hidden_from_employee`; an ephemeral sub-agent
//!   counts as its parent), and their `total` counts only what is listed. A
//!   goal belongs to the employee that created it
//!   (`personal_visibility::goal_creators`). A self-scoped listing reads no
//!   `users.db` (it never contains another employee's rows); a scope naming
//!   another employee was already decided by the pre-dispatch gate. An
//!   unreadable ownership store refuses the wildcard listing.
//! - **External callers.** An external key's client id is never an
//!   employee: the Personal gates see it as `external:<client id>`
//!   ([`personal_acting_id`]), so a key named like a Personal employee is
//!   not that employee; in protected mode an external key whose client id
//!   names an existing employee is refused outright
//!   ([`external_identity_refusal`]), because other gates resolve such a
//!   client id to that employee.
use duduclaw_gateway::personal_visibility as visibility;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;

use super::RecordActor;

/// Shown to an external key whose client id names an employee.
pub(crate) const EXTERNAL_ID_IS_EMPLOYEE: &str = "這把外部 MCP 金鑰的用戶端代號與某位 AI 員工相同，受保護模式下已拒絕。請以不同的代號重新簽發金鑰。 \
    / This external MCP key's client id equals an AI employee's id; it is refused in protected mode. Issue the key \
    under a different client id.";

/// The id the Personal gates judge a call by: the acting employee for an
/// internal or per-agent key, `external:<client id>` for an external key
/// (never a valid employee id, never an `eph-` sub-agent).
pub(crate) fn personal_acting_id(principal: &crate::mcp_auth::Principal, default_agent: &str) -> String {
    if principal.is_external {
        format!("external:{}", principal.client_id)
    } else {
        super::acting_agent_id(&principal.client_id, default_agent).to_owned()
    }
}

/// Protected mode: refuse an external key whose client id is an existing
/// employee (`agents/<id>/agent.toml`), so no record or namespace gate can
/// resolve it to that employee.
pub(crate) fn external_identity_refusal(home_dir: &Path, protected: bool, principal: &crate::mcp_auth::Principal)
    -> Option<&'static str> {
    (protected && principal.is_external && crate::mcp_namespace::client_is_agent(home_dir, &principal.client_id))
        .then_some(EXTERNAL_ID_IS_EMPLOYEE)
}

/// Personal employees a wildcard listing hides from `acting`. Empty when not
/// protected or when the listing is not a wildcard (no I/O).
pub(crate) fn wildcard_hidden(home_dir: &Path, protected: bool, acting: &str, wildcard: bool)
    -> Result<BTreeSet<String>, &'static str> {
    if !protected || !wildcard {
        return Ok(BTreeSet::new());
    }
    visibility::hidden_from_employee(home_dir, true, acting).map_err(|_| visibility::OWNERSHIP_UNAVAILABLE)
}

fn is_wildcard(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_str) == Some("*")
}

async fn hidden_for(home_dir: &Path, protected: bool, acting: &str, wildcard: bool)
    -> Result<BTreeSet<String>, &'static str> {
    let (home, acting) = (home_dir.to_path_buf(), acting.to_owned());
    tokio::task::spawn_blocking(move || wildcard_hidden(&home, protected, &acting, wildcard))
        .await
        .unwrap_or(Err(visibility::OWNERSHIP_UNAVAILABLE))
}

/// `tasks_list` with [`wildcard_hidden`] applied.
pub(crate) async fn tasks_list(args: &Value, home_dir: &Path, default_agent: &str, actor: RecordActor<'_>,
    acting: &str, protected: bool) -> Value {
    match hidden_for(home_dir, protected, acting, is_wildcard(args, "assigned_to")).await {
        Ok(hidden) => super::handle_tasks_list_hiding(args, home_dir, default_agent, actor, &hidden).await,
        Err(refusal) => super::tool_error(refusal),
    }
}

/// `activity_list` with [`wildcard_hidden`] applied.
pub(crate) async fn activity_list(args: &Value, home_dir: &Path, default_agent: &str, actor: RecordActor<'_>,
    acting: &str, protected: bool) -> Value {
    match hidden_for(home_dir, protected, acting, is_wildcard(args, "agent_id")).await {
        Ok(hidden) => super::handle_activity_list_hiding(args, home_dir, default_agent, actor, &hidden).await,
        Err(refusal) => super::tool_error(refusal),
    }
}

/// `goals_list` (company-wide, so always treated as a wildcard) with the
/// goals created by hidden employees left out.
pub(crate) async fn goals_list(args: &Value, home_dir: &Path, acting: &str, protected: bool) -> Value {
    match hidden_for(home_dir, protected, acting, true).await {
        Ok(hidden) => super::handle_goals_list_hiding(args, home_dir, &hidden).await,
        Err(refusal) => super::tool_error(refusal),
    }
}
