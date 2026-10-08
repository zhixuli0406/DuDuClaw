//! P5 (2026-10-08) — opt-in read-only lane for a responsibility's runs.
//!
//! A responsibility created with `lane = "explore"` runs every occurrence
//! round in the read-only explore lane that #69 introduced for the heartbeat
//! proactive check: `DUDUCLAW_LANE=explore` reaches the DuDuClaw MCP server
//! (only `read` / `draft` tools listed and callable) and the Claude CLI's
//! built-in tools are narrowed to `EXPLORE_LANE_BUILTIN_TOOLS`.
//!
//! The lane is stored inside the contract's `scope_json`
//! (`{"event_names":[…],"lane":"explore"}`), so it is part of the contract
//! hash, needs no schema change, and a responsibility without it keeps a
//! byte-identical `scope_json`.
//!
//! Running the round: the dispatcher folds this lane and the external-event
//! lane (`message_queue.lane`) into the one flag [`crate::explore_lane::EXPLORE`];
//! which runtimes carry it, the pre-dispatch refusal
//! ([`crate::explore_lane::dispatch_refusal`]) and the restrictions inside the
//! run are described there and are the same for both sources. Fail closed:
//! never run unrestricted.

use serde_json::Value;

use crate::task_store::ResponsibilityRow;

/// The only lane value a responsibility may carry.
pub const LANE_EXPLORE: &str = duduclaw_core::LANE_EXPLORE;

/// Lane of a responsibility, read from its stored `scope_json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RespLane {
    /// No lane restriction (the default).
    Normal,
    /// Read-only explore lane.
    Explore,
}

/// Validate a lane given at create / update time. `None` and `"normal"` are
/// the default; `"explore"` is the read-only lane; anything else is refused.
pub fn parse_input_lane(lane: Option<&str>) -> Result<RespLane, String> {
    match lane.map(str::trim) {
        None | Some("") | Some("normal") => Ok(RespLane::Normal),
        Some(LANE_EXPLORE) => Ok(RespLane::Explore),
        Some(other) => Err(format!(
            "lane must be \"explore\" or absent (got {:?})",
            duduclaw_core::truncate_chars(other, 40)
        )),
    }
}

/// Lane recorded in a stored `scope_json`. An unparseable value, or a lane
/// key with any value other than `"explore"`, is an error: the caller must
/// not run the round (fail closed).
pub fn lane_of_scope(scope_json: &str) -> Result<RespLane, String> {
    let v: Value = serde_json::from_str(scope_json)
        .map_err(|e| format!("responsibility scope is unreadable: {e}"))?;
    match v.get("lane") {
        None | Some(Value::Null) => Ok(RespLane::Normal),
        Some(Value::String(s)) if s == LANE_EXPLORE => Ok(RespLane::Explore),
        Some(other) => Err(format!("responsibility lane {other} is not recognised")),
    }
}

pub fn lane_of(row: &ResponsibilityRow) -> Result<RespLane, String> {
    lane_of_scope(&row.scope_json)
}

/// The `event_names` part of a stored scope (what decides whether the
/// subscriptions changed; the lane is not a subscription).
pub fn scope_event_names(scope_json: &str) -> Value {
    serde_json::from_str::<Value>(scope_json)
        .ok()
        .and_then(|v| v.get("event_names").cloned())
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod lane_tests {
    use super::*;

    #[test]
    fn input_lane_values() {
        assert_eq!(parse_input_lane(None), Ok(RespLane::Normal));
        assert_eq!(parse_input_lane(Some("normal")), Ok(RespLane::Normal));
        assert_eq!(parse_input_lane(Some("explore")), Ok(RespLane::Explore));
        assert!(parse_input_lane(Some("Explore")).is_err());
        assert!(parse_input_lane(Some("write")).is_err());
    }

    #[test]
    fn stored_lane_fails_closed() {
        assert_eq!(lane_of_scope(r#"{"event_names":[]}"#), Ok(RespLane::Normal));
        assert_eq!(
            lane_of_scope(r#"{"event_names":[],"lane":"explore"}"#),
            Ok(RespLane::Explore)
        );
        assert!(lane_of_scope(r#"{"event_names":[],"lane":"other"}"#).is_err());
        assert!(lane_of_scope(r#"{"event_names":[],"lane":1}"#).is_err());
        assert!(lane_of_scope("not json").is_err());
    }
}
