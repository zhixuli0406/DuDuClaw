//! Dashboard delegation to a Personal employee and hidden-row counts
//! (native Linux isolation v1.81, task P1c). Protected mode only.
//!
//! - **Delegation.** `tasks.create` / `tasks.update` (`assigned_to`),
//!   `tasks.goal_create` / `plans.create` (`agent_id`) naming another
//!   person's Personal employee are refused before the handler: a Personal
//!   employee takes work only from its owner (the users.db mirror owner,
//!   read fresh, compared with the live identity's id by exact equality).
//!   There is no Admin break-glass for delegation. `tasks.update` on a task
//!   that already belongs to one is additionally gated on that task's
//!   employee (`personal_data_gate::task_scoped`).
//! - **Counts.** `tasks.flow_metrics` aggregates over tasks whose employee
//!   is not hidden from the caller ([`MethodHandler::flow_metrics_hidden`]);
//!   `tasks.list_page` / `activity.list` totals use the store's `*_hiding`
//!   queries with `TaskReader::hidden`, so a count never includes a row the
//!   caller cannot list.
use super::*;
use crate::personal_visibility as visibility;

/// The parameter naming the employee a dashboard method hands work to.
pub(super) fn delegation_target_key(method: &str) -> Option<&'static str> {
    Some(match method {
        "tasks.create" | "tasks.update" => "assigned_to",
        "tasks.goal_create" | "plans.create" => "agent_id",
        _ => return None,
    })
}

/// The decision for one delegation by `ctx` to `agent` (exact id).
/// Shared and unknown employees need no identity read; for a Personal
/// employee the caller is re-read from `users.db` (revoked ⇒ refused) and
/// must be its owner. `None` = allowed (the existing checks still apply).
pub(super) fn delegation_refusal(home: &Path, protected: bool, ctx: &UserContext, agent: &str)
    -> Option<&'static str> {
    if !protected {
        return None;
    }
    match visibility::personal_owner(home, agent) {
        Ok(None) => return None,
        Ok(Some(_)) => {}
        Err(_) => return Some(visibility::OWNERSHIP_UNAVAILABLE),
    }
    let Ok(live) = super::task_privacy::live_reader_context(home, ctx) else {
        return Some(super::task_privacy::PERMISSION_DENIED);
    };
    visibility::delegation_refusal_for_user(home, true, &live.user_id, agent)
}

impl MethodHandler {
    /// Dispatch-level delegation gate ([`delegation_target_key`]). Runs for
    /// every method from `personal_gates_for_params`; a no-op for other
    /// methods, for an absent/blank target and outside protected mode.
    pub(super) async fn personal_delegation_gate(
        &self,
        method: &str,
        params: &Value,
        ctx: &UserContext,
    ) -> Result<(), WsFrame> {
        let Some(key) = delegation_target_key(method) else {
            return Ok(());
        };
        if !crate::runtime_readiness::is_protected_linux() {
            return Ok(());
        }
        let Some(agent) = params
            .get(key)
            .and_then(Value::as_str)
            .filter(|agent| !agent.trim().is_empty())
            .map(str::to_owned)
        else {
            return Ok(());
        };
        let (home, session) = (self.home_dir.clone(), ctx.clone());
        let refusal = tokio::task::spawn_blocking(move || delegation_refusal(&home, true, &session, &agent))
            .await
            .unwrap_or(Some(visibility::OWNERSHIP_UNAVAILABLE));
        match refusal {
            Some(message) => Err(WsFrame::error_response("", message)),
            None => Ok(()),
        }
    }

    /// Employees `tasks.flow_metrics` must neither count nor break out for
    /// `live`: Personal employees it does not own, minus the request's own
    /// `agent_id` scope (already admitted by the dispatch gate: the owner, or
    /// an audited Admin step-up). Empty outside protected mode; unreadable ⇒
    /// refused.
    pub(super) async fn flow_metrics_hidden(
        &self,
        live: &UserContext,
        scope: Option<&str>,
    ) -> Result<std::collections::BTreeSet<String>, WsFrame> {
        let mut hidden = self
            .personal_hidden_agents(live)
            .await
            .map_err(|message| WsFrame::error_response("", message))?;
        if let Some(scope) = scope.filter(|scope| !scope.is_empty()) {
            hidden.remove(scope);
        }
        Ok(hidden)
    }
}
