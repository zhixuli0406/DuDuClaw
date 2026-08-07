//! GVU rejection telemetry (WP0.6, R6 diagnostics half of ABC §3.3 P2).
//!
//! ## Why this exists
//!
//! R6 diagnosis: the Verifier's 8-layer fail-closed chain (L-Safety →
//! L1 → L2 → L2.5 → L3 judge → L3.5 → L-Canary → L4) and the Updater's
//! cap/ASI skip-gates can veto a proposal for many different reasons, but
//! until now NOTHING recorded *which* layer vetoed *what* — the code comment
//! in `verifier.rs` documents a real production incident ("3 generations all
//! rejected with the same error") purely from re-reading log lines by hand.
//! Without a queryable trail there is no way to tell "over-restrictive rule
//! that should be relaxed" (ABC §3.3 P2) from "correctly blocking a genuinely
//! bad proposal" — it's a diagnostic blind spot, not just a UX gap.
//!
//! ## What this does
//!
//! Every Verifier layer rejection and every Updater apply-skip appends one
//! JSONL line to `~/.duduclaw/evolution_telemetry.jsonl` (cross-process safe
//! via [`duduclaw_core::with_file_lock`]). [`telemetry_summary`] aggregates
//! that file into per-(stage, layer) rejection counts for a trailing window,
//! feeding a future dashboard chart and the (already-decided, always-human-
//! reviewed) rulebase relaxation proposals in WP2.6.
//!
//! **Zero behavioral impact.** This module never changes what the
//! Verifier/Updater decide — it only observes. Write failures are logged and
//! swallowed; telemetry must never be able to break the GVU loop it watches.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::version_store::VersionStore;

/// Reason strings are capped at this many bytes (CJK-safe) before being
/// written — a verbose LLM-judge feedback string must not make a single
/// telemetry line unbounded.
const REASON_MAX_BYTES: usize = 500;

/// One Verifier-rejection or Updater-skip record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectionRecord {
    /// RFC-3339 timestamp of the rejection.
    pub ts: String,
    pub agent_id: String,
    /// Which stage produced this rejection: `"verify"` (Verifier layer) or
    /// `"apply"` (Updater skip-gate).
    pub stage: String,
    /// Layer/gate tag — for `stage="verify"` this is the `TextGradient
    /// .source_layer` string (e.g. `"L1-Deterministic"`, `"L3-LLMJudge"`);
    /// for `stage="apply"` it's an Updater gate name (e.g. `"cap_lines"`,
    /// `"asi_critical"`).
    pub layer: String,
    /// Human-readable reason, truncated to [`REASON_MAX_BYTES`] bytes.
    pub reason: String,
    /// SHA-256 hex digest of the proposal content. Lets repeated rejections
    /// of the literal-same text be correlated without persisting the raw
    /// (potentially sensitive) proposal body in the telemetry log.
    pub proposal_hash: String,
    /// Proposal generation number (1-based).
    pub generation: u32,
}

/// Aggregate rejection counts over a trailing window, grouped by stage then
/// layer. Feeds a future dashboard chart (deferred to a later wave per the
/// task brief — this module only exposes the query function).
#[derive(Debug, Clone, Default, Serialize)]
pub struct TelemetrySummary {
    pub agent_id: String,
    pub days: i64,
    pub total: u64,
    /// stage -> layer -> count
    pub by_stage_layer: std::collections::BTreeMap<String, std::collections::BTreeMap<String, u64>>,
}

fn telemetry_path(home_dir: &Path) -> PathBuf {
    home_dir.join("evolution_telemetry.jsonl")
}

/// Derive the DuDuClaw home directory from an already-open `VersionStore`.
///
/// `VersionStore`'s db_path is always `<home>/evolution.db` in production
/// (see `server.rs` / `duduclaw-cli evolution finalize`), so this avoids
/// threading a new `home_dir` parameter through `verify_all_with_mistakes`
/// and `Updater::apply` — both already carry a `&VersionStore`.
fn home_dir_from_store(version_store: &VersionStore) -> Option<PathBuf> {
    version_store.db_path_ref().parent().map(|p| p.to_path_buf())
}

fn sha256_hex(bytes: &[u8]) -> String {
    use ring::digest;
    let d = digest::digest(&digest::SHA256, bytes);
    d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Append one rejection/skip record, deriving the telemetry file location
/// from the given `VersionStore`. Best-effort: any failure (missing home
/// dir, I/O error, lock contention) is logged via `tracing::warn!` and
/// swallowed — telemetry must never be able to break the GVU loop.
pub fn record_rejection_from_store(
    version_store: &VersionStore,
    agent_id: &str,
    stage: &str,
    layer: &str,
    reason: &str,
    proposal_content: &str,
    generation: u32,
) {
    let Some(home_dir) = home_dir_from_store(version_store) else {
        warn!(
            agent = %agent_id,
            "GVU telemetry: could not derive home_dir from VersionStore db_path — skipping"
        );
        return;
    };
    record_rejection(&home_dir, agent_id, stage, layer, reason, proposal_content, generation);
}

/// Append one rejection/skip record to `<home_dir>/evolution_telemetry.jsonl`.
///
/// Best-effort: write failures are logged and swallowed, never propagated.
pub fn record_rejection(
    home_dir: &Path,
    agent_id: &str,
    stage: &str,
    layer: &str,
    reason: &str,
    proposal_content: &str,
    generation: u32,
) {
    let record = RejectionRecord {
        ts: Utc::now().to_rfc3339(),
        agent_id: agent_id.to_string(),
        stage: stage.to_string(),
        layer: layer.to_string(),
        reason: duduclaw_core::truncate_bytes(reason, REASON_MAX_BYTES).to_string(),
        proposal_hash: sha256_hex(proposal_content.as_bytes()),
        generation,
    };
    if let Err(e) = append_record(&telemetry_path(home_dir), &record) {
        warn!(
            agent = %agent_id,
            stage,
            layer,
            error = %e,
            "Failed to write GVU rejection telemetry (non-fatal, GVU loop unaffected)"
        );
    }
}

fn append_record(path: &Path, record: &RejectionRecord) -> Result<(), String> {
    let line = serde_json::to_string(record).map_err(|e| e.to_string())?;
    duduclaw_core::with_file_lock(path, || {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        writeln!(f, "{line}")
    })
    .map_err(|e| e.to_string())
}

/// Aggregate `<home_dir>/evolution_telemetry.jsonl` rejection counts for
/// `agent_id` over the trailing `days` days, grouped by stage then layer.
///
/// Missing file (telemetry never written, or a fresh install) returns an
/// empty summary rather than an error — this is a query surface, not a
/// correctness-critical path.
pub fn telemetry_summary(home_dir: &Path, agent_id: &str, days: i64) -> TelemetrySummary {
    let mut summary = TelemetrySummary {
        agent_id: agent_id.to_string(),
        days,
        ..Default::default()
    };

    let path = telemetry_path(home_dir);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return summary,
    };

    let cutoff = Utc::now() - chrono::Duration::days(days.max(0));

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let record: RejectionRecord = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(_) => continue, // Corrupt line — skip, don't fail the whole summary.
        };
        if record.agent_id != agent_id {
            continue;
        }
        let ts: DateTime<Utc> = match DateTime::parse_from_rfc3339(&record.ts) {
            Ok(t) => t.with_timezone(&Utc),
            Err(_) => continue,
        };
        if ts < cutoff {
            continue;
        }

        summary.total += 1;
        summary
            .by_stage_layer
            .entry(record.stage.clone())
            .or_default()
            .entry(record.layer.clone())
            .and_modify(|c| *c += 1)
            .or_insert(1);
    }

    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn record_and_summarize_round_trip() {
        let tmp = TempDir::new().unwrap();
        record_rejection(tmp.path(), "agent-a", "verify", "L1-Deterministic", "too long", "content-1", 1);
        record_rejection(tmp.path(), "agent-a", "verify", "L1-Deterministic", "too long again", "content-2", 2);
        record_rejection(tmp.path(), "agent-a", "verify", "L3-LLMJudge", "low score", "content-3", 1);
        record_rejection(tmp.path(), "agent-a", "apply", "cap_lines", "over cap", "content-4", 1);
        // Different agent — must not pollute agent-a's summary.
        record_rejection(tmp.path(), "agent-b", "verify", "L1-Deterministic", "unrelated", "content-5", 1);

        let summary = telemetry_summary(tmp.path(), "agent-a", 7);
        assert_eq!(summary.total, 4);
        assert_eq!(
            summary.by_stage_layer.get("verify").and_then(|m| m.get("L1-Deterministic")),
            Some(&2)
        );
        assert_eq!(
            summary.by_stage_layer.get("verify").and_then(|m| m.get("L3-LLMJudge")),
            Some(&1)
        );
        assert_eq!(
            summary.by_stage_layer.get("apply").and_then(|m| m.get("cap_lines")),
            Some(&1)
        );
    }

    #[test]
    fn missing_file_returns_empty_summary() {
        let tmp = TempDir::new().unwrap();
        let summary = telemetry_summary(tmp.path(), "agent-a", 7);
        assert_eq!(summary.total, 0);
        assert!(summary.by_stage_layer.is_empty());
    }

    #[test]
    fn long_reason_is_truncated_to_byte_budget() {
        let tmp = TempDir::new().unwrap();
        // All multi-byte CJK so a naive byte-slice would panic mid-character.
        let long_reason: String = "拒絕原因".repeat(200); // way over 500 bytes
        record_rejection(tmp.path(), "agent-a", "verify", "L3-LLMJudge", &long_reason, "content", 1);

        let path = telemetry_path(tmp.path());
        let content = std::fs::read_to_string(&path).unwrap();
        let record: RejectionRecord = serde_json::from_str(content.lines().next().unwrap()).unwrap();
        assert!(
            record.reason.len() <= REASON_MAX_BYTES,
            "reason must be capped at {REASON_MAX_BYTES} bytes, got {} bytes",
            record.reason.len()
        );
        // Must not have panicked mid-multi-byte-char, and must still be valid UTF-8
        // (guaranteed by type, but assert content is non-empty and well-formed).
        assert!(!record.reason.is_empty());
    }

    #[test]
    fn out_of_window_records_are_excluded() {
        let tmp = TempDir::new().unwrap();
        let path = telemetry_path(tmp.path());
        // Hand-craft an old record (30 days ago) bypassing record_rejection's
        // Utc::now() timestamp.
        let old = RejectionRecord {
            ts: (Utc::now() - chrono::Duration::days(30)).to_rfc3339(),
            agent_id: "agent-a".to_string(),
            stage: "verify".to_string(),
            layer: "L1-Deterministic".to_string(),
            reason: "old".to_string(),
            proposal_hash: "deadbeef".to_string(),
            generation: 1,
        };
        std::fs::write(&path, format!("{}\n", serde_json::to_string(&old).unwrap())).unwrap();
        record_rejection(tmp.path(), "agent-a", "verify", "L1-Deterministic", "recent", "c", 1);

        let summary = telemetry_summary(tmp.path(), "agent-a", 7);
        assert_eq!(summary.total, 1, "only the recent record should count within a 7-day window");
    }

    #[test]
    fn corrupt_line_is_skipped_not_fatal() {
        let tmp = TempDir::new().unwrap();
        let path = telemetry_path(tmp.path());
        std::fs::write(&path, "not-json\n{\"garbage\": true}\n").unwrap();
        record_rejection(tmp.path(), "agent-a", "verify", "L1-Deterministic", "ok", "c", 1);

        let summary = telemetry_summary(tmp.path(), "agent-a", 7);
        assert_eq!(summary.total, 1, "corrupt lines must be skipped, not abort the summary");
    }
}
