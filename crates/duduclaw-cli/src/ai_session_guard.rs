//! "Is this process running inside an AI employee's turn?" for operator-only
//! memory commands (`memory forget-source`, `memory migrate-namespace`).
//!
//! The gateway sets one or more of these variables on every process it
//! spawns for an employee (identity, turn, session, delegation chain, run).
//! Any one of them present, even empty, counts: an operator's own terminal
//! carries none, and a command that deletes or moves memory fails closed.
//! This is a first line only — an employee with Bash can unset variables —
//! so the destructive step also needs a dashboard approval and the Bash lane
//! refuses the command for employees.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_single_marker_counts_and_none_means_operator() {
        assert!(markers_with(&|_| false).is_empty());
        for k in AI_SESSION_ENV_VARS {
            let found = markers_with(&|v| v == *k);
            assert_eq!(found, vec![*k]);
        }
        assert!(AI_SESSION_ENV_VARS.contains(&"DUDUCLAW_TURN_ID"));
        assert!(AI_SESSION_ENV_VARS.contains(&"DUDUCLAW_SESSION_ID"));
    }
}
