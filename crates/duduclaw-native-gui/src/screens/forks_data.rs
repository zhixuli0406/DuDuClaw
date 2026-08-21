// WP-S5b3-H (S5b 第三波, 2026-08-21) — data model + pure parsing for the
// "分支決戰" page (`screens/forks.rs`), RFC-26 Live Run Forking.
//
// ── Data source (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed) ────────────────────────────────────────────
//   `fork.list {"limit"?}` (dispatch ~L6657, handler `handle_fork_list`
//   ~L31134, NO admin/manager gate — any authenticated caller may list) →
//   `{"forks":[{"fork_id","agent_id","merge_mode","resolved","winner",
//   "promoted","aggregate_spent_usd","created_at"}]}`. Empty when
//   `fork_store.db` doesn't exist yet (no fork ever created) — an honest
//   empty list, not an error.
//   `fork.inspect {"fork_id"}` (handler `handle_fork_inspect` ~L31173) →
//   `{"fork_id","agent_id","prompt","merge_mode","resolved","winner",
//   "promoted","branches":[{"branch_id","steering","state","budget_usd",
//   "spent_usd","test_exit_code","output"}]}`. `prompt`/`output` are
//   server-truncated (4000/8000 bytes) but never fabricated.
//   `fork.resolve {"fork_id","branch_id"}` (manager-gated) promotes a
//   winner — this page renders the "選為勝者" affordance but does NOT wire
//   it (task brief: "決策類組裝不真按", same scope line this task draws
//   for every other page's irreversible-decision button).
//
// ── Honest deviation from the canvas ─────────────────────────────────────
// `Forks.dc.html` (B13) draws the left "分支任務" list with human-readable
// task titles ("整理 8 月客服月報") and a branch count per row — but
// `ForkSummary` (the real `fork.list` row shape) carries NEITHER a title
// NOR a branch count; only `fork.inspect` (one fork at a time) returns
// `prompt`/`branches`. Fetching every fork's detail just to populate the
// list would multiply RPC calls per render and still race real usage —
// `web/src/pages/ForkPage.tsx`'s own left list has the identical gap and
// renders `fork_id.slice(0, 14)` instead of a title (verified by reading
// that file directly, not assumed). This page follows that same, already-
// shipped precedent: the LIST shows fork_id/agent/merge_mode/spend/resolved
// state; the real `prompt` appears once a fork is selected and its detail
// loads (`fork.inspect`'s own field).

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct ForkSummary {
    pub fork_id: String,
    pub agent_id: String,
    pub merge_mode: String,
    pub resolved: bool,
    pub winner: Option<String>,
    pub promoted: bool,
    pub aggregate_spent_usd: f64,
    pub created_at: String,
}

pub fn parse_fork_list(v: &Value) -> Vec<ForkSummary> {
    v.get("forks")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|f| {
                    let fork_id = f.get("fork_id")?.as_str()?.to_string();
                    let agent_id = f.get("agent_id").and_then(Value::as_str).unwrap_or("").to_string();
                    let merge_mode = f.get("merge_mode").and_then(Value::as_str).unwrap_or("").to_string();
                    let resolved = f.get("resolved").and_then(Value::as_bool).unwrap_or(false);
                    let winner = f.get("winner").and_then(Value::as_str).map(str::to_string);
                    let promoted = f.get("promoted").and_then(Value::as_bool).unwrap_or(false);
                    let aggregate_spent_usd = f.get("aggregate_spent_usd").and_then(Value::as_f64).unwrap_or(0.0);
                    let created_at = f.get("created_at").and_then(Value::as_str).unwrap_or("").to_string();
                    Some(ForkSummary { fork_id, agent_id, merge_mode, resolved, winner, promoted, aggregate_spent_usd, created_at })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq)]
pub struct ForkBranch {
    pub branch_id: String,
    pub steering: Option<String>,
    pub state: String,
    pub budget_usd: f64,
    pub spent_usd: f64,
    pub test_exit_code: Option<i64>,
    pub output: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ForkDetail {
    pub fork_id: String,
    pub agent_id: String,
    pub prompt: String,
    pub merge_mode: String,
    pub resolved: bool,
    pub winner: Option<String>,
    pub promoted: bool,
    pub branches: Vec<ForkBranch>,
}

pub fn parse_fork_detail(v: &Value) -> ForkDetail {
    let branches = v
        .get("branches")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|b| ForkBranch {
                    branch_id: b.get("branch_id").and_then(Value::as_str).unwrap_or("").to_string(),
                    steering: b.get("steering").and_then(Value::as_str).map(str::to_string),
                    state: b.get("state").and_then(Value::as_str).unwrap_or("pending").to_string(),
                    budget_usd: b.get("budget_usd").and_then(Value::as_f64).unwrap_or(0.0),
                    spent_usd: b.get("spent_usd").and_then(Value::as_f64).unwrap_or(0.0),
                    test_exit_code: b.get("test_exit_code").and_then(Value::as_i64),
                    output: b.get("output").and_then(Value::as_str).unwrap_or("").to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    ForkDetail {
        fork_id: v.get("fork_id").and_then(Value::as_str).unwrap_or("").to_string(),
        agent_id: v.get("agent_id").and_then(Value::as_str).unwrap_or("").to_string(),
        prompt: v.get("prompt").and_then(Value::as_str).unwrap_or("").to_string(),
        merge_mode: v.get("merge_mode").and_then(Value::as_str).unwrap_or("").to_string(),
        resolved: v.get("resolved").and_then(Value::as_bool).unwrap_or(false),
        winner: v.get("winner").and_then(Value::as_str).map(str::to_string),
        promoted: v.get("promoted").and_then(Value::as_bool).unwrap_or(false),
        branches,
    }
}

/// First 8 chars of a branch/fork id — matches `ForkPage.tsx`'s own
/// `branch.branch_id.slice(0, 8)` truncation convention (CJK-safe: fork/
/// branch ids are ASCII UUID-shaped, so a char-count slice never lands
/// mid-codepoint, but `chars().take(n)` is used anyway per this crate's
/// "never slice strings by raw byte index" coding convention).
pub fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_fork_list_reads_every_field() {
        let v = json!({ "forks": [{"fork_id":"f1","agent_id":"a1","merge_mode":"best_of","resolved":true,
                                     "winner":"b1","promoted":false,"aggregate_spent_usd":0.42,"created_at":"t"}]});
        let rows = parse_fork_list(&v);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].winner.as_deref(), Some("b1"));
        assert!((rows[0].aggregate_spent_usd - 0.42).abs() < 1e-9);
    }

    #[test]
    fn parse_fork_list_missing_array_is_empty() {
        assert!(parse_fork_list(&json!({})).is_empty());
    }

    #[test]
    fn parse_fork_detail_reads_branches() {
        let v = json!({
            "fork_id":"f1","agent_id":"a1","prompt":"do x","merge_mode":"best_of",
            "resolved":false,"winner":null,"promoted":false,
            "branches":[{"branch_id":"b1","steering":null,"state":"running","budget_usd":1.5,
                          "spent_usd":0.4,"test_exit_code":null,"output":"working..."}],
        });
        let d = parse_fork_detail(&v);
        assert_eq!(d.branches.len(), 1);
        assert_eq!(d.branches[0].state, "running");
        assert_eq!(d.branches[0].test_exit_code, None);
    }

    #[test]
    fn short_id_takes_first_eight_chars() {
        assert_eq!(short_id("a1f92c8bdeadbeef"), "a1f92c8b");
        assert_eq!(short_id("abc"), "abc");
    }
}
