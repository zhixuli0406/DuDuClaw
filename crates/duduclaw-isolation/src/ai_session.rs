//! The environment variables that mark a process DuDuClaw spawned for an AI
//! employee (identity, turn, session, delegation chain, run). ONE list for every
//! binary that refuses operator-only work inside such a session: duduclaw-cli
//! (`ai_session_guard`) and the root installer `duduclaw-bootstrap`
//! (duduclaw-updated). It lives here because this is the lowest crate both
//! already depend on (duduclaw-updated does not link duduclaw-core or
//! duduclaw-gateway). The literals equal duduclaw-core's and duduclaw-gateway's
//! named constants; duduclaw-cli's tests assert that, so the two cannot drift.
//!
//! Pure: the caller supplies the presence check (no environment or other I/O is
//! read here). Any one variable present, even empty, counts: an operator's own
//! terminal carries none. This is a guard against mistakes, not a privilege
//! boundary: a process can unset its environment.

/// Variables that mark a DuDuClaw-spawned employee process.
pub const AI_SESSION_ENV_VARS: &[&str] = &[
    // duduclaw_core::ENV_AGENT_ID
    "DUDUCLAW_AGENT_ID",
    // duduclaw_core::ENV_AGENT_TOKEN
    "DUDUCLAW_AGENT_TOKEN",
    // duduclaw_core::ENV_HOOK_TOKEN (RM-166 round 2: the hook-only proof a
    // Claude employee's CLI env carries).
    "DUDUCLAW_HOOK_TOKEN",
    // duduclaw_core::ENV_TRUST_TURN_ID
    "DUDUCLAW_TURN_ID",
    // duduclaw_core::ENV_TRUST_SESSION_ID
    "DUDUCLAW_SESSION_ID",
    // duduclaw_core::ENV_TURN_USER_MESSAGE_SEQ
    "DUDUCLAW_TURN_USER_MESSAGE_SEQ",
    // duduclaw_core::ENV_TURN_USER_MESSAGE_AT
    "DUDUCLAW_TURN_USER_MESSAGE_AT",
    // duduclaw_core::ENV_DISPATCH_SESSION
    "DUDUCLAW_DISPATCH_SESSION",
    // duduclaw_core::ENV_DISPATCH_RUN_ID
    "DUDUCLAW_DISPATCH_RUN_ID",
    // duduclaw_core::ENV_UPSTREAM_UNKNOWN
    "DUDUCLAW_UPSTREAM_UNKNOWN",
    // duduclaw_core::ENV_DELEGATION_SENDER
    "DUDUCLAW_DELEGATION_SENDER",
    // duduclaw_core::ENV_DELEGATION_ORIGIN
    "DUDUCLAW_DELEGATION_ORIGIN",
    // duduclaw_core::ENV_DELEGATION_DEPTH
    "DUDUCLAW_DELEGATION_DEPTH",
    // duduclaw_core::ENV_HOP_DEPTH
    "DUDUCLAW_HOP_DEPTH",
    // duduclaw_core::ENV_REPLY_CHANNEL
    "DUDUCLAW_REPLY_CHANNEL",
    // duduclaw_core::ENV_REPLY_SENDER
    "DUDUCLAW_REPLY_SENDER",
    // duduclaw_core::ENV_ORIGIN_SESSION: the WebChat conversation a turn answers (RM-124).
    "DUDUCLAW_ORIGIN_SESSION",
    // duduclaw_core::ENV_TASK_ID: the task of a goal round (set per call by the gateway's runtimes).
    "DUDUCLAW_TASK_ID",
    // duduclaw_core::ENV_LANE: the read-only explore lane (heartbeat proactive check, 2026-10).
    "DUDUCLAW_LANE",
    // duduclaw_core::ENV_ON_BEHALF_OF: the person the run works for (`on_behalf_of`).
    "DUDUCLAW_ON_BEHALF_OF",
    // duduclaw_gateway::on_behalf_of::context::ENV_CONTEXT
    "DUDUCLAW_PRINCIPAL_CONTEXT",
    // Also refused by the LINE inbox and computer-workspace commands before
    // the lists were merged into this one.
    "DUDUCLAW_MCP_API_KEY",
    "DUDUCLAW_DATA_FILE_GUARD",
];

/// The marker variables present according to `is_set`, in list order.
pub fn markers_with(is_set: &dyn Fn(&str) -> bool) -> Vec<&'static str> {
    AI_SESSION_ENV_VARS.iter().copied().filter(|key| is_set(key)).collect()
}

/// Whether any marker is present according to `is_set`.
pub fn in_ai_session(is_set: &dyn Fn(&str) -> bool) -> bool {
    AI_SESSION_ENV_VARS.iter().any(|key| is_set(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_single_marker_counts_and_the_list_has_no_duplicates() {
        assert!(markers_with(&|_| false).is_empty());
        assert!(!in_ai_session(&|_| false));
        for key in AI_SESSION_ENV_VARS {
            assert_eq!(markers_with(&|k| k == *key), vec![*key]);
            assert!(in_ai_session(&|k| k == *key));
        }
        let mut sorted = AI_SESSION_ENV_VARS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), AI_SESSION_ENV_VARS.len());
        assert!(AI_SESSION_ENV_VARS.iter().all(|key| key.starts_with("DUDUCLAW_")));
    }
}
