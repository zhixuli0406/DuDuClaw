//! Shared auth computation, with gateway presentation and fail-closed reads.

use std::path::Path;
use serde_json::{json, Value};
pub(crate) use duduclaw_auth::dept_grants::{reach_change, reach_digest, reach_sentence_zh, ReachLine};

pub(crate) const UNAVAILABLE: &str = "授權影響無法計算";
pub(crate) const CHANGED: &str = "授權範圍在你檢視後有變動，請重新確認。";

/// Callers pass the same authoritative entry used to compute the org base.
/// A supervisor-only change never needs the grant store.
pub(crate) fn for_move(home: &Path, from: &str, to: &str) -> Result<Vec<ReachLine>, String> {
    if from == to { return Ok(Vec::new()); }
    let db = duduclaw_auth::UserDb::open_read_only(&home.join("users.db"))?;
    reach_change(&db, Some(from), Some(to))
}

pub(crate) fn preview(lines: &[ReachLine]) -> Value {
    json!({"reach": lines, "reach_digest": reach_digest(lines), "sentence": reach_sentence_zh(lines)})
}

pub(crate) fn proposal_preview(home: &Path, from: &str, to: &str) -> Value {
    match for_move(home, from, to) {
        Ok(lines) => preview(&lines),
        Err(_) => json!({"reach": null, "reach_digest": null, "sentence": UNAVAILABLE}),
    }
}
