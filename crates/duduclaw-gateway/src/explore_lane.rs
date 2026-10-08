//! Running a dispatched turn in the read-only explore lane (2026-10-08).
//!
//! The heartbeat proactive check has run in the lane since 2026-10-07
//! (`duduclaw-agent/src/heartbeat.rs`). Work woken by an external event (an
//! MCP Events delivery, [`crate::mcp_events`]) runs there too, unless the
//! operator opted the subscription into normal mode: the bus message carries
//! `lane = "explore"` (`message_queue.lane`; any other non-empty value is
//! read as explore too), the dispatcher scopes
//! [`EXPLORE`] around the run, and the Claude CLI spawn
//! (`claude_runner::prepare_claude_cmd`) then
//!
//! - sets `DUDUCLAW_LANE=explore`, which the DuDuClaw MCP server, `duduclaw
//!   mcp-proxy` and `duduclaw mcp-remote-bridge` inherit (read-only tools
//!   only, see `duduclaw_core::ProcessLane`);
//! - narrows the built-in tools to `EXPLORE_LANE_BUILTIN_TOOLS`;
//! - routes third-party stdio MCP servers through the gated proxy.
//!
//! Only the Claude CLI path is wired. A run in the lane whose employee uses
//! another runtime, a MoA model or local-only inference is refused (fail
//! closed), and the hybrid local offload is skipped.

tokio::task_local! {
    /// `true` while a dispatched run must stay in the explore lane.
    pub static EXPLORE: bool;
}

/// Is the current task in the explore lane?
pub fn in_explore() -> bool {
    EXPLORE.try_with(|e| *e).unwrap_or(false)
}

/// Does a bus message's `lane` put the run in the explore lane? `None` ⇒
/// normal; `explore` ⇒ explore; any other value also ⇒ explore, the most
/// restrictive lane there is (an unknown value is never read as normal).
pub fn is_explore(raw: Option<&str>) -> bool {
    raw.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scope_and_lane_parsing() {
        assert!(!in_explore());
        assert!(EXPLORE.scope(true, async { in_explore() }).await);
        assert!(!is_explore(None));
        assert!(is_explore(Some("explore")));
        assert!(is_explore(Some("")));
        assert!(is_explore(Some("normal")));
    }
}
