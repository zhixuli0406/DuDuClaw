//! G6 / Hindsight #7 — consolidation-failure telemetry.
//!
//! Hindsight's Memories view lets an operator drill into *failed* consolidations
//! — "these raw memories were NOT merged; here is why". DuDuClaw's reflexion
//! consolidation (`crate::reflexion`) has several rejection points that until
//! now returned `Ok(None)` **silently**, so a user asking "why didn't the agent
//! learn from these repeated mistakes?" had no answer. This module records the
//! *interesting* rejections — the ones where a group actually reached the count
//! threshold but was then blocked by a downstream gate — into an append-only
//! JSONL telemetry log the dashboard can query.
//!
//! Deliberately NOT recorded: the ordinary "still accumulating, below
//! threshold" case (`total < threshold`), which fires on nearly every mistake
//! and is normal progress, not a failure. Only gate-reached rejections are
//! logged, so the file stays small and every row is a genuine "why not merged".
//!
//! Storage: `<home>/consolidation_failures.jsonl`, one JSON object per line,
//! appended under a cross-process advisory lock (`duduclaw_core::with_file_lock`)
//! — same discipline as the other shared JSONL logs. Bounded by
//! [`MAX_RECORDS`]: on overflow the oldest rows are dropped (tail kept). All
//! failures here are best-effort telemetry — a write error only logs, never
//! propagates into the reflexion path.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name under the agent home directory.
pub const FILE_NAME: &str = "consolidation_failures.jsonl";

/// Max rows retained; on overflow the oldest are dropped. Telemetry only fires
/// on gate-reached rejections (bounded), so this is generous.
pub const MAX_RECORDS: usize = 2000;

/// Why a would-be consolidation did not produce a rule. Serialized as a stable
/// snake_case `reason` string so the dashboard can group without a schema bump.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    /// The raw group reached the count threshold, but after the B2 evidence
    /// filter (unverified self-reports dropped) too few verified mistakes
    /// remained. `detail`: `{ "raw": N, "verified": M, "threshold": T }`.
    InsufficientVerifiedEvidence,
    /// Enough verified mistakes, but the GovMem independence gate
    /// (`reflexion::assess_promotion`) judged them correlated — too few
    /// distinct sessions and/or distinct lessons. `detail`:
    /// `{ "verified": M, "distinct_sessions": S, "distinct_lessons": L }`.
    NeedsMoreEvidence,
    /// The synthesized rule was a near-duplicate of an already-known rule and
    /// the B1 novelty gate rejected the write. `detail`:
    /// `{ "matched_id": "...", "similarity": 0.93, "threshold": 0.92 }`.
    NoveltyRejected,
}

impl FailureReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InsufficientVerifiedEvidence => "insufficient_verified_evidence",
            Self::NeedsMoreEvidence => "needs_more_evidence",
            Self::NoveltyRejected => "novelty_rejected",
        }
    }
}

/// One recorded consolidation failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConsolidationFailure {
    /// RFC3339 timestamp.
    pub at: String,
    pub agent_id: String,
    /// `MistakeCategory::as_str()`.
    pub category: String,
    /// `source_kind` of the group (may be empty for unattributed/legacy rows).
    #[serde(default)]
    pub source_kind: String,
    pub reason: FailureReason,
    /// Reason-specific structured detail (counts, ids, similarity...).
    #[serde(default)]
    pub detail: serde_json::Value,
}

impl ConsolidationFailure {
    pub fn new(
        agent_id: &str,
        category: &str,
        source_kind: &str,
        reason: FailureReason,
        detail: serde_json::Value,
    ) -> Self {
        Self {
            at: chrono::Utc::now().to_rfc3339(),
            agent_id: agent_id.to_string(),
            category: category.to_string(),
            source_kind: source_kind.to_string(),
            reason,
            detail,
        }
    }
}

fn log_path(home_dir: &Path) -> PathBuf {
    home_dir.join(FILE_NAME)
}

/// Append one failure record. Best-effort: a filesystem error is logged and
/// swallowed — telemetry must never break the consolidation path.
pub fn record_failure(home_dir: &Path, failure: &ConsolidationFailure) {
    let path = log_path(home_dir);
    let line = match serde_json::to_string(failure) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "consolidation-failure telemetry: serialize failed");
            return;
        }
    };
    let res = duduclaw_core::with_file_lock(&path, || {
        {
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
            writeln!(f, "{line}")?;
        }
        // Bounded retention: if the file grew past MAX_RECORDS lines, keep the
        // newest tail. Cheap because this only fires on gate-reached rejections.
        if let Ok(contents) = std::fs::read_to_string(&path) {
            let lines: Vec<&str> = contents.lines().collect();
            if lines.len() > MAX_RECORDS {
                let keep = &lines[lines.len() - MAX_RECORDS..];
                std::fs::write(&path, format!("{}\n", keep.join("\n")))?;
            }
        }
        Ok(())
    });
    if let Err(e) = res {
        tracing::warn!(error = %e, "consolidation-failure telemetry: write failed");
    }
}

/// Read recorded failures, newest-first, optionally filtered to one agent,
/// capped at `limit`. Malformed lines are skipped (fail-open). Missing file ⇒
/// empty. This is the query surface a dashboard RPC or CLI wraps (#7 drill-down).
pub fn list_failures(
    home_dir: &Path,
    agent_id: Option<&str>,
    limit: usize,
) -> Vec<ConsolidationFailure> {
    let path = log_path(home_dir);
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut out: Vec<ConsolidationFailure> = contents
        .lines()
        .filter_map(|l| serde_json::from_str::<ConsolidationFailure>(l).ok())
        .filter(|f| agent_id.is_none_or(|a| f.agent_id == a))
        .collect();
    out.reverse(); // newest-first
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn record_and_list_roundtrip_newest_first_and_agent_filtered() {
        let dir = TempDir::new().unwrap();
        record_failure(
            dir.path(),
            &ConsolidationFailure::new(
                "alice",
                "capability",
                "task_failure",
                FailureReason::NeedsMoreEvidence,
                serde_json::json!({ "verified": 3, "distinct_sessions": 1, "distinct_lessons": 3 }),
            ),
        );
        record_failure(
            dir.path(),
            &ConsolidationFailure::new(
                "bob",
                "factual",
                "",
                FailureReason::NoveltyRejected,
                serde_json::json!({ "matched_id": "m1", "similarity": 0.95, "threshold": 0.92 }),
            ),
        );
        record_failure(
            dir.path(),
            &ConsolidationFailure::new(
                "alice",
                "capability",
                "task_failure",
                FailureReason::InsufficientVerifiedEvidence,
                serde_json::json!({ "raw": 4, "verified": 2, "threshold": 3 }),
            ),
        );

        // Newest-first, all agents.
        let all = list_failures(dir.path(), None, 10);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].reason, FailureReason::InsufficientVerifiedEvidence);
        assert_eq!(all[2].reason, FailureReason::NeedsMoreEvidence);

        // Agent filter.
        let alice = list_failures(dir.path(), Some("alice"), 10);
        assert_eq!(alice.len(), 2);
        assert!(alice.iter().all(|f| f.agent_id == "alice"));

        // Limit.
        assert_eq!(list_failures(dir.path(), None, 1).len(), 1);
    }

    #[test]
    fn missing_file_is_empty_not_error() {
        let dir = TempDir::new().unwrap();
        assert!(list_failures(dir.path(), None, 10).is_empty());
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let dir = TempDir::new().unwrap();
        let path = log_path(dir.path());
        std::fs::write(&path, "not json\n{\"garbage\":true}\n").unwrap();
        record_failure(
            dir.path(),
            &ConsolidationFailure::new("z", "capability", "", FailureReason::NoveltyRejected, serde_json::json!({})),
        );
        // Only the one valid record survives the parse filter.
        let got = list_failures(dir.path(), None, 10);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].agent_id, "z");
    }

    #[test]
    fn retention_keeps_newest_tail() {
        let dir = TempDir::new().unwrap();
        // Write MAX_RECORDS + 5 rows; only the newest MAX_RECORDS survive.
        for i in 0..(MAX_RECORDS + 5) {
            record_failure(
                dir.path(),
                &ConsolidationFailure::new(
                    "a",
                    "capability",
                    "",
                    FailureReason::NoveltyRejected,
                    serde_json::json!({ "i": i }),
                ),
            );
        }
        let all = list_failures(dir.path(), None, MAX_RECORDS + 100);
        assert_eq!(all.len(), MAX_RECORDS, "retention caps at MAX_RECORDS");
        // Newest-first: the very newest row is i = MAX_RECORDS+4.
        assert_eq!(all[0].detail["i"], serde_json::json!(MAX_RECORDS + 4));
        // The oldest surviving row is i = 5 (0..4 dropped).
        assert_eq!(all[all.len() - 1].detail["i"], serde_json::json!(5));
    }
}
