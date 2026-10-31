//! "Is this process running inside an AI employee's turn?" for operator-only
//! commands: `egress-leases`, `memory forget-source`, `memory migrate-namespace`,
//! `responsibility …` and `agent pause|resume|freeze|unfreeze` (present
//! even when empty counts), and
//! `channel-ingress` / `computer-workspaces` (same list, non-empty
//! values count there). One list for all of them.
//!
//! The gateway sets one or more of these variables on every process it
//! spawns for an employee (identity, turn, session, delegation chain, run).
//! Any one of them present, even empty, counts: an operator's own terminal
//! carries none, and operator-only commands fail closed.
//! This is a first line only — an employee with Bash can unset variables —
//! so the Bash lane also refuses these commands for employees. Destructive
//! memory operations additionally need dashboard approval; employee status
//! and evolution switches remain directly available in an operator's terminal.

/// Variables that mark a DuDuClaw-spawned employee process.
pub(crate) const AI_SESSION_ENV_VARS: &[&str] = &[
    duduclaw_core::ENV_AGENT_ID,
    duduclaw_core::ENV_AGENT_TOKEN,
    duduclaw_core::ENV_TRUST_TURN_ID,
    duduclaw_core::ENV_TRUST_SESSION_ID,
    duduclaw_core::ENV_TURN_USER_MESSAGE_SEQ,
    duduclaw_core::ENV_TURN_USER_MESSAGE_AT,
    duduclaw_core::ENV_DISPATCH_SESSION,
    duduclaw_core::ENV_DISPATCH_RUN_ID,
    duduclaw_core::ENV_UPSTREAM_UNKNOWN,
    duduclaw_core::ENV_DELEGATION_SENDER,
    duduclaw_core::ENV_DELEGATION_ORIGIN,
    duduclaw_core::ENV_DELEGATION_DEPTH,
    duduclaw_core::ENV_HOP_DEPTH,
    duduclaw_core::ENV_REPLY_CHANNEL,
    duduclaw_core::ENV_REPLY_SENDER,
    // The task of a goal round (set per call by the gateway's runtimes).
    duduclaw_core::ENV_TASK_ID,
    // The read-only explore lane (heartbeat proactive check, 2026-10).
    duduclaw_core::ENV_LANE,
    // The person the run works for (`on_behalf_of`).
    duduclaw_core::ENV_ON_BEHALF_OF,
    duduclaw_gateway::on_behalf_of::context::ENV_CONTEXT,
    // Also refused by the LINE inbox and computer-workspace commands before
    // the lists were merged into this one.
    "DUDUCLAW_MCP_API_KEY",
    "DUDUCLAW_DATA_FILE_GUARD",
];

/// The marker variables present according to `is_set`.
pub(crate) fn markers_with(is_set: &dyn Fn(&str) -> bool) -> Vec<&'static str> {
    AI_SESSION_ENV_VARS
        .iter()
        .copied()
        .filter(|k| is_set(k))
        .collect()
}

/// The marker variables present in this process.
pub(crate) fn markers() -> Vec<&'static str> {
    markers_with(&|k| std::env::var_os(k).is_some())
}

// Explicit utility classification shared by operator commands. An incomplete
// employee environment must never turn a command into a human interaction.
fn utility_employee_identity(marked: bool, agent: Option<&str>) -> Option<String> {
    if !marked {
        return None;
    }
    Some(match agent {
        Some(id) if duduclaw_core::is_valid_agent_id(id)
            && !duduclaw_core::is_system_sender(id)
            && !duduclaw_core::is_reserved_queue_sender(id) => id.to_string(),
        _ => "invalid:utility-caller".to_string(),
    })
}

fn utility_employee() -> Option<String> {
    utility_employee_identity(!markers().is_empty(), std::env::var(duduclaw_core::ENV_AGENT_ID).ok().as_deref())
}

pub(crate) async fn with_cli_utility_context<F: std::future::Future>(
    home: &std::path::Path,
    target: Option<&str>,
    future: F,
) -> F::Output {
    use duduclaw_gateway::runtime_dispatch::{UtilityContext, with_utility_context};
    match utility_employee() {
        Some(caller) => {
            let target = target.unwrap_or(&caller);
            let callers = vec![caller.clone()];
            with_utility_context(home, UtilityContext::Autonomous { target, callers: &callers }, future).await
        }
        None => with_utility_context(home, UtilityContext::HumanInteractive, future).await,
    }
}

/// MCP and detached work is autonomous even without an employee environment.
/// The caller is additional authority; it never replaces the real target.
pub(crate) async fn with_autonomous_utility_context<F: std::future::Future>(
    home: &std::path::Path,
    target: &str,
    future: F,
) -> F::Output {
    use duduclaw_gateway::runtime_dispatch::{UtilityContext, with_utility_context};
    let callers = utility_employee().into_iter().collect::<Vec<_>>();
    with_utility_context(home, UtilityContext::Autonomous { target, callers: &callers }, future).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utility_context_requires_an_unmarked_operator_or_an_employee_identity() {
        assert_eq!(utility_employee_identity(false, None), None);
        assert_eq!(utility_employee_identity(true, Some("alice")), Some("alice".into()));
        for agent in [None, Some(""), Some("../alice"), Some("heartbeat-scheduler")] {
            assert_eq!(utility_employee_identity(true, agent), Some("invalid:utility-caller".into()));
        }
    }

    #[test]
    fn any_single_marker_counts_and_none_means_operator() {
        assert!(markers_with(&|_| false).is_empty());
        for k in AI_SESSION_ENV_VARS {
            let found = markers_with(&|v| v == *k);
            assert_eq!(found, vec![*k]);
        }
        assert!(AI_SESSION_ENV_VARS.contains(&"DUDUCLAW_TURN_ID"));
        assert!(AI_SESSION_ENV_VARS.contains(&"DUDUCLAW_SESSION_ID"));
        assert!(AI_SESSION_ENV_VARS.contains(&"DUDUCLAW_TASK_ID"));
        assert!(AI_SESSION_ENV_VARS.contains(&"DUDUCLAW_ON_BEHALF_OF"));
        assert!(AI_SESSION_ENV_VARS.contains(&"DUDUCLAW_LANE"));
    }
}
