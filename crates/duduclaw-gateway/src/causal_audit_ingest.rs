//! The audit trail as the causal evidence graph's first **real** source
//! (X1 方案 2). Zero LLM, zero egress, deterministic.
//!
//! ## What this is, and what it is emphatically not
//!
//! When a goal task settles as `rejected` or `needs_human`, two things are
//! already on disk: the deterministic fault attribution
//! ([`crate::fault_attribution::classify_fault_with_reason`]'s rule token) and
//! the round's own `tool_calls.jsonl` rows. This module writes those bytes as
//! a [`SourceArtifact`] and files **one candidate claim** against them:
//!
//! * cause  — the rule token that fired (`tool_error:<tool>`,
//!   `capability_blocked`, `r3_tool_claim_without_native_events`, …)
//! * effect — `task_rejected:<goal_kind>` / `task_escalated:<goal_kind>`
//! * evidence — a byte span into the artifact pointing at the exact audit line
//!
//! **This is transcription, not causal inference.** The claim is born
//! `candidate` and stays there until a human accepts it on the Curation page;
//! nothing downstream may treat an unreviewed claim as evidence. The pairing
//! is "these bytes were recorded together", which is a correlation with a
//! timestamp — the spec's whole point is that becoming *evidence* requires a
//! person to look.
//!
//! ## Boundaries
//!
//! * `config.toml [causal] audit_ingest` — **default true** (zero LLM, zero
//!   egress, local-only), one line to turn off.
//! * 50 claims per agent per UTC day, counted through `with_file_lock` exactly
//!   like `auto_wiki_page::try_consume_quota`.
//! * Deduplicated on `(agent, cause, effect, UTC day)` — a task failing the
//!   same way six times in a morning files one claim, not six.
//! * Every failure fails **open to not writing**: a missing audit file, an
//!   unreadable store, a rejected proposal all `debug!` and return.

use std::path::{Path, PathBuf};

use chrono::Utc;
use duduclaw_core::{truncate_bytes, with_file_lock};
use duduclaw_memory::causal::{
    CausalStore, ClaimModality, EvidenceScope, EvidenceStance, ProposedCausalClaim,
};
use tracing::debug;

/// Tenant every audit-derived claim is scoped to.
pub const TENANT_ID: &str = "local";
/// ACL of every audit-derived claim. The Curation page renders this ACL with
/// a "source: audit trail (real)" label.
pub const ACL: &str = "audit";
/// Artifact kind, so an operator can tell these from uploaded sources.
pub const ARTIFACT_KIND: &str = "duduclaw_tool_audit";
/// Extractor version recorded on every evidence span. Bumped when the cause
/// or effect vocabulary changes.
pub const EXTRACTOR_VERSION: &str = "audit-ingest-v1";
/// Claims per agent per UTC day.
pub const DAILY_CLAIM_LIMIT: u32 = 50;
/// How long the artifact bytes are retained. Long enough for a human to review
/// the claim; the causal store's own sweeper removes them afterwards.
const RETENTION_DAYS: i64 = 90;
/// Ceiling on the artifact content built from audit rows.
const MAX_ARTIFACT_BYTES: usize = 64 * 1024;
/// Most recent audit rows considered for one settle.
const MAX_ROWS: usize = 40;

/// `config.toml [causal] audit_ingest` (default **true**).
///
/// Isolated `toml::Table` parse, matching `fault_attribution::enabled_from_home`
/// — an unrelated malformed section can never turn this on or off by accident.
pub fn enabled_from_home(home_dir: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(home_dir.join("config.toml")) else {
        return true;
    };
    let Ok(table) = content.parse::<toml::Table>() else {
        return true;
    };
    table
        .get("causal")
        .and_then(|v| v.as_table())
        .and_then(|s| s.get("audit_ingest"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

/// The settle outcomes this module reacts to. Accepted rounds are deliberately
/// excluded: "it worked" carries no failure mechanism to transcribe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettleOutcome {
    Rejected,
    Escalated,
}

impl SettleOutcome {
    fn effect_prefix(self) -> &'static str {
        match self {
            Self::Rejected => "task_rejected",
            Self::Escalated => "task_escalated",
        }
    }
}

/// Normalize a free-form goal kind into an effect-variable suffix.
///
/// Lower-cased, non-alphanumerics folded to `-`, capped at 40 bytes on a char
/// boundary (`truncate_bytes`, never a raw byte slice — CJK goal kinds are
/// normal here). Empty input becomes `unspecified` so the effect variable is
/// never a bare prefix.
pub fn normalize_goal_kind(kind: &str) -> String {
    let folded: String = kind
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let trimmed = folded.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "unspecified".to_string()
    } else {
        truncate_bytes(&trimmed, 40).to_string()
    }
}

/// The cause variable for one settle: the fault rule token, refined with the
/// failing tool's name when the audit rows name one.
///
/// Returns the token unchanged when no tool error is visible — inventing a
/// tool name would be exactly the narrative this module exists to avoid.
pub fn cause_variable(fault_reason: &str, failing_tool: Option<&str>) -> String {
    match failing_tool {
        Some(tool) if !tool.trim().is_empty() => {
            format!("tool_error:{}", truncate_bytes(tool.trim(), 60))
        }
        _ => {
            let token = fault_reason.trim();
            if token.is_empty() {
                "unattributed".to_string()
            } else {
                truncate_bytes(token, 60).to_string()
            }
        }
    }
}

/// The effect variable for one settle.
pub fn effect_variable(outcome: SettleOutcome, goal_kind: &str) -> String {
    format!(
        "{}:{}",
        outcome.effect_prefix(),
        normalize_goal_kind(goal_kind)
    )
}

/// One audit row rendered as an artifact line, plus where it landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditLine {
    pub text: String,
    pub span_start: usize,
    pub span_end: usize,
}

/// Render audit rows into artifact content and the byte span of each line.
///
/// Spans are computed while building, so they are always exact byte offsets
/// into the returned string and always land on char boundaries (each line is
/// appended whole). Returns `None` when nothing renderable remains.
pub fn render_artifact(rows: &[serde_json::Value]) -> Option<(String, Vec<AuditLine>)> {
    let mut content = String::new();
    let mut lines = Vec::new();
    for row in rows
        .iter()
        .rev()
        .take(MAX_ROWS)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        let Ok(text) = serde_json::to_string(row) else {
            continue;
        };
        if content.len() + text.len() + 1 > MAX_ARTIFACT_BYTES {
            break;
        }
        let start = content.len();
        content.push_str(&text);
        let end = content.len();
        content.push('\n');
        lines.push(AuditLine {
            text,
            span_start: start,
            span_end: end,
        });
    }
    if lines.is_empty() {
        return None;
    }
    Some((content, lines))
}

/// Whether one `tool_calls.jsonl` row records a failure.
///
/// The canonical schema (`audit::append_tool_call_with_extras`) writes
/// `success: bool`; denial rows add `error_class`, and the Odoo attribution
/// path writes its own `ok: bool` extra. All three are read, because all three
/// exist on disk today.
pub fn row_failed(row: &serde_json::Value) -> bool {
    row.get("error_class")
        .is_some_and(|c| !c.is_null() && c.as_str().is_none_or(|s| !s.trim().is_empty()))
        || row.get("success").and_then(serde_json::Value::as_bool) == Some(false)
        || row.get("ok").and_then(serde_json::Value::as_bool) == Some(false)
}

/// Tool name recorded on one row (`tool_name` is canonical; `tool` is the
/// shape some older extras used).
pub fn row_tool(row: &serde_json::Value) -> Option<&str> {
    row.get("tool_name")
        .or_else(|| row.get("tool"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Pick the line the evidence span should point at: the first failing row,
/// else the last row in the window.
///
/// Deliberately positional, not semantic — the point of the span is "here is
/// the byte range a reviewer should read", not a judgement about it.
pub fn select_evidence_line(lines: &[AuditLine]) -> Option<&AuditLine> {
    lines
        .iter()
        .find(|line| {
            serde_json::from_str::<serde_json::Value>(&line.text).is_ok_and(|v| row_failed(&v))
        })
        .or_else(|| lines.last())
}

/// The failing tool name, when an audit row names one.
pub fn failing_tool(rows: &[serde_json::Value]) -> Option<String> {
    rows.iter()
        .rev()
        .find(|row| row_failed(row))
        .and_then(row_tool)
        .map(str::to_owned)
}

fn quota_path(home_dir: &Path, agent_id: &str) -> PathBuf {
    crate::outcome_spec::agent_work_dir(home_dir, agent_id)
        .join("state")
        .join("causal_audit_quota.json")
}

/// Consume one unit of the per-agent daily claim budget **and** enforce the
/// `(cause, effect, day)` dedup in the same locked read-modify-write, so two
/// concurrent settles cannot both decide they are the first.
///
/// Returns `true` when the caller may write a claim. A filesystem failure
/// returns `false` here — unlike the auto-wiki quota this guard also carries
/// the dedup, and failing open would let a hot failure loop file 50 identical
/// claims. Not writing a claim loses nothing: the audit rows are still on disk.
pub fn try_consume(home_dir: &Path, agent_id: &str, cause: &str, effect: &str) -> bool {
    let path = quota_path(home_dir, agent_id);
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let key = format!("{cause}\u{1f}{effect}");
    let result = with_file_lock(&path, || {
        let mut doc: serde_json::Value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        if doc.get("date").and_then(|v| v.as_str()) != Some(today.as_str()) {
            doc = serde_json::json!({ "date": today, "used": 0, "seen": [] });
        }
        let seen: Vec<String> = doc
            .get("seen")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        if seen.iter().any(|s| s == &key) {
            return Ok(false);
        }
        let used = doc.get("used").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        if used >= DAILY_CLAIM_LIMIT {
            return Ok(false);
        }
        let mut seen = seen;
        seen.push(key.clone());
        doc["used"] = serde_json::json!(used + 1);
        doc["seen"] = serde_json::json!(seen);
        std::fs::write(&path, serde_json::to_string(&doc).unwrap_or_default())?;
        Ok(true)
    });
    result.unwrap_or(false)
}

/// Everything one settle needs to hand this module.
#[derive(Debug, Clone)]
pub struct SettleFacts<'a> {
    pub agent_id: &'a str,
    pub task_id: &'a str,
    pub round: u32,
    pub outcome: SettleOutcome,
    pub goal_kind: &'a str,
    /// The `fault_attribution` rule token for this round.
    pub fault_reason: &'a str,
    /// Start of this round's evidence window, RFC3339.
    pub since: &'a str,
}

/// What a successful ingest produced. Returned for the tests and the caller's
/// log line; nothing downstream consumes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestedClaim {
    pub artifact_id: String,
    pub claim_id: String,
    pub cause_variable: String,
    pub effect_variable: String,
    pub span_start: usize,
    pub span_end: usize,
}

/// Transcribe one failed settle into a candidate causal claim. Best-effort:
/// every failure path returns `None` after a `debug!`.
pub fn ingest_settle(home_dir: &Path, facts: &SettleFacts<'_>) -> Option<IngestedClaim> {
    if !enabled_from_home(home_dir) {
        return None;
    }
    if facts.agent_id.trim().is_empty() {
        return None;
    }
    let rows =
        duduclaw_security::audit::read_tool_calls_since(home_dir, facts.agent_id, facts.since);
    if rows.is_empty() {
        debug!(
            agent = facts.agent_id,
            task = facts.task_id,
            "causal audit ingest: no audit rows in this round's window — nothing to transcribe"
        );
        return None;
    }
    let cause = cause_variable(facts.fault_reason, failing_tool(&rows).as_deref());
    let effect = effect_variable(facts.outcome, facts.goal_kind);
    if cause == effect {
        return None;
    }
    if !try_consume(home_dir, facts.agent_id, &cause, &effect) {
        debug!(
            agent = facts.agent_id,
            cause, effect, "causal audit ingest: quota exhausted or already filed today"
        );
        return None;
    }
    let (content, lines) = render_artifact(&rows)?;
    let line = select_evidence_line(&lines)?;

    let store = CausalStore::new(home_dir.join("memory.db"));
    let scope = EvidenceScope {
        tenant_id: TENANT_ID.into(),
        acl: ACL.into(),
    };
    let now = Utc::now().timestamp();
    let version = format!(
        "{:x}",
        <sha2::Sha256 as sha2::Digest>::digest(content.as_bytes())
    );
    let external_id = format!("audit:{}:{}:{}", facts.agent_id, facts.task_id, facts.round);
    let artifact = match store.add_artifact(
        &scope,
        ARTIFACT_KIND,
        &external_id,
        &version,
        &format!("tool_calls.jsonl@{}", facts.agent_id),
        &content,
        now,
        now + RETENTION_DAYS * 86_400,
    ) {
        Ok(artifact) => artifact,
        Err(e) => {
            debug!(error = %e, "causal audit ingest: artifact write refused");
            return None;
        }
    };

    let proposal = ProposedCausalClaim {
        cause_variable: cause.clone(),
        effect_variable: effect.clone(),
        lag_min_seconds: 0,
        lag_max_seconds: 86_400,
        modality: ClaimModality::Asserted,
        stance: EvidenceStance::Supports,
        span_start: line.span_start,
        span_end: line.span_end,
        excerpt: line.text.clone(),
        speaker_id: None,
        context: serde_json::json!({
            "source": "tool_calls.jsonl",
            "agent_id": facts.agent_id,
            "task_id": facts.task_id,
            "round": facts.round,
            "fault_reason": facts.fault_reason,
            "deterministic": true,
            "note": "Transcribed from the audit trail. Co-occurrence, not an inferred causal effect.",
        }),
    };
    match store.ingest_extracted_claim(
        &scope,
        &artifact.id,
        "Which recorded failure accompanied this task settle?",
        EXTRACTOR_VERSION,
        &proposal,
    ) {
        Ok((claim, span)) => Some(IngestedClaim {
            artifact_id: artifact.id,
            claim_id: claim.id,
            cause_variable: cause,
            effect_variable: effect,
            span_start: span.span_start,
            span_end: span.span_end,
        }),
        Err(e) => {
            debug!(error = %e, "causal audit ingest: claim refused");
            None
        }
    }
}

/// Fraction of a playbook entry's `signals_match` tokens that are backed by a
/// **human-accepted** audit-derived causal claim (0.0–1.0).
///
/// Matching is exact, case-insensitive, after trimming — never a substring
/// (CLAUDE.md coding convention 2; `db` must not "support" `db_select`). A
/// cause token of the `tool_error:<tool>` shape also matches a signal naming
/// the bare tool, because that is the same fact written two ways.
///
/// `None` when there are no accepted claims at all — that is "not measured",
/// not "measured zero", and the commit gate must be able to tell them apart.
pub fn causal_support_score(signals: &[String], accepted_causes: &[String]) -> Option<f64> {
    if accepted_causes.is_empty() {
        return None;
    }
    if signals.is_empty() {
        return Some(0.0);
    }
    let normalized: Vec<String> = accepted_causes
        .iter()
        .flat_map(|cause| {
            let c = cause.trim().to_lowercase();
            match c.strip_prefix("tool_error:") {
                Some(tool) => vec![c.clone(), tool.to_string()],
                None => vec![c],
            }
        })
        .collect();
    let hits = signals
        .iter()
        .filter(|signal| {
            let s = signal.trim().to_lowercase();
            !s.is_empty() && normalized.iter().any(|c| *c == s)
        })
        .count();
    Some(hits as f64 / signals.len() as f64)
}

/// `config.toml [evolution] require_causal_evidence` (default **false**).
///
/// When on, a playbook `Add` whose signals no accepted causal claim supports
/// is written as a **shadow candidate** — stored, scored, never injected —
/// instead of going straight into the injection pool.
pub fn require_causal_evidence(home_dir: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(home_dir.join("config.toml")) else {
        return false;
    };
    let Ok(table) = content.parse::<toml::Table>() else {
        return false;
    };
    table
        .get("evolution")
        .and_then(|v| v.as_table())
        .and_then(|s| s.get("require_causal_evidence"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Cause tokens of this agent's **human-accepted** audit-derived claims.
///
/// The AEE's `causal_support` measure reads exactly this — `review_state =
/// "accepted"` only. A candidate nobody has looked at contributes nothing,
/// which is the whole reason the review state exists.
pub fn accepted_cause_tokens(home_dir: &Path, limit: usize) -> Vec<String> {
    let store = CausalStore::new(home_dir.join("memory.db"));
    let scope = EvidenceScope {
        tenant_id: TENANT_ID.into(),
        acl: ACL.into(),
    };
    let Ok(ids) = store.list_claim_ids(&scope, Some("accepted"), limit.min(500)) else {
        return Vec::new();
    };
    ids.into_iter()
        .filter_map(|id| store.read_claim(&scope, &id).ok())
        .map(|claim| claim.cause_variable)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rows() -> Vec<serde_json::Value> {
        vec![
            json!({"tool_name": "db_select", "success": true, "agent_id": "a"}),
            json!({"tool_name": "web_fetch_cached", "success": false, "error_class": "timeout"}),
            json!({"tool_name": "tasks_complete", "success": true}),
        ]
    }

    #[test]
    fn goal_kind_normalization_is_cjk_safe_and_never_empty() {
        assert_eq!(normalize_goal_kind("Code Review"), "code-review");
        assert_eq!(normalize_goal_kind("  "), "unspecified");
        assert_eq!(normalize_goal_kind("---"), "unspecified");
        // A long CJK kind is truncated on a char boundary, not mid-codepoint.
        let long = "報表產生".repeat(20);
        let out = normalize_goal_kind(&long);
        assert!(out.len() <= 40);
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
        assert!(out.starts_with("報表產生"));
    }

    #[test]
    fn cause_variable_prefers_the_failing_tool_and_never_invents_one() {
        assert_eq!(
            cause_variable("r2_infrastructure_failure", Some("db_select")),
            "tool_error:db_select"
        );
        assert_eq!(
            cause_variable("r3_tool_claim_without_native_events", None),
            "r3_tool_claim_without_native_events"
        );
        assert_eq!(cause_variable("  ", None), "unattributed");
        assert_eq!(cause_variable("x", Some("   ")), "x");
    }

    #[test]
    fn effect_variable_names_the_outcome_and_the_goal_kind() {
        assert_eq!(
            effect_variable(SettleOutcome::Rejected, "報表"),
            "task_rejected:報表"
        );
        assert_eq!(
            effect_variable(SettleOutcome::Escalated, ""),
            "task_escalated:unspecified"
        );
    }

    #[test]
    fn artifact_spans_are_exact_byte_ranges_into_the_content() {
        let (content, lines) = render_artifact(&rows()).unwrap();
        assert_eq!(lines.len(), 3);
        for line in &lines {
            assert_eq!(
                content.get(line.span_start..line.span_end),
                Some(line.text.as_str()),
                "the causal store slices the artifact by exactly this range"
            );
        }
    }

    #[test]
    fn artifact_spans_survive_cjk_content() {
        let cjk = vec![
            json!({"tool_name": "檔案讀取", "success": false, "error_class": "逾時", "note": "中文內容"}),
            json!({"tool_name": "記憶查詢", "success": true}),
        ];
        let (content, lines) = render_artifact(&cjk).unwrap();
        for line in &lines {
            assert_eq!(
                content.get(line.span_start..line.span_end),
                Some(line.text.as_str())
            );
        }
    }

    #[test]
    fn evidence_line_points_at_the_failure_when_there_is_one() {
        let (_, lines) = render_artifact(&rows()).unwrap();
        let picked = select_evidence_line(&lines).unwrap();
        assert!(picked.text.contains("web_fetch_cached"));
    }

    #[test]
    fn evidence_line_falls_back_to_the_last_row_when_nothing_failed() {
        let clean = vec![
            json!({"tool_name": "a", "success": true}),
            json!({"tool_name": "b", "success": true}),
        ];
        let (_, lines) = render_artifact(&clean).unwrap();
        let picked = select_evidence_line(&lines).unwrap();
        assert!(picked.text.contains("\"b\""));
    }

    #[test]
    fn failing_tool_reads_the_most_recent_failure_only() {
        assert_eq!(failing_tool(&rows()).as_deref(), Some("web_fetch_cached"));
        let clean = vec![json!({"tool_name": "a", "success": true})];
        assert_eq!(failing_tool(&clean), None);
    }

    #[test]
    fn empty_rows_render_nothing() {
        assert!(render_artifact(&[]).is_none());
    }

    #[test]
    fn enabled_defaults_on_and_is_switchable() {
        let dir = tempfile::tempdir().unwrap();
        assert!(enabled_from_home(dir.path()), "default on");
        std::fs::write(dir.path().join("config.toml"), "not = [toml").unwrap();
        assert!(enabled_from_home(dir.path()), "malformed keeps the default");
        std::fs::write(
            dir.path().join("config.toml"),
            "[causal]\naudit_ingest = false\n",
        )
        .unwrap();
        assert!(!enabled_from_home(dir.path()));
    }

    #[test]
    fn quota_dedups_on_cause_effect_and_day() {
        let dir = tempfile::tempdir().unwrap();
        assert!(try_consume(dir.path(), "a", "c1", "e1"));
        assert!(
            !try_consume(dir.path(), "a", "c1", "e1"),
            "the same pair must not file twice in one day"
        );
        assert!(
            try_consume(dir.path(), "a", "c2", "e1"),
            "a new cause files"
        );
        assert!(
            try_consume(dir.path(), "b", "c1", "e1"),
            "quotas are per agent"
        );
    }

    #[test]
    fn quota_stops_at_the_daily_limit() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..DAILY_CLAIM_LIMIT {
            assert!(
                try_consume(dir.path(), "a", &format!("c{i}"), "e"),
                "at {i}"
            );
        }
        assert!(
            !try_consume(dir.path(), "a", "one-too-many", "e"),
            "the 51st distinct pair is refused"
        );
    }

    #[test]
    fn causal_support_is_none_without_any_accepted_claim() {
        assert_eq!(
            causal_support_score(&["tool_error:db_select".into()], &[]),
            None,
            "no accepted claim means NOT MEASURED, never a measured zero"
        );
    }

    #[test]
    fn causal_support_counts_exact_signal_matches_only() {
        let accepted = vec![
            "tool_error:db_select".to_string(),
            "capability_blocked".to_string(),
        ];
        assert_eq!(
            causal_support_score(&["tool_error:db_select".into()], &accepted),
            Some(1.0)
        );
        assert_eq!(
            causal_support_score(
                &["tool_error:db_select".into(), "mistake:factual".into()],
                &accepted
            ),
            Some(0.5)
        );
        assert_eq!(
            causal_support_score(&["mistake:factual".into()], &accepted),
            Some(0.0)
        );
        // A bare tool name matches its `tool_error:` claim — same fact, two
        // spellings — but a prefix of one never does.
        assert_eq!(
            causal_support_score(&["db_select".into()], &accepted),
            Some(1.0)
        );
        assert_eq!(
            causal_support_score(&["db".into()], &accepted),
            Some(0.0),
            "substring matching would make every token 'supported'"
        );
    }

    #[test]
    fn causal_support_of_an_empty_signal_list_is_zero_not_one() {
        let accepted = vec!["capability_blocked".to_string()];
        assert_eq!(causal_support_score(&[], &accepted), Some(0.0));
    }

    #[test]
    fn require_causal_evidence_defaults_off() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!require_causal_evidence(dir.path()));
        std::fs::write(dir.path().join("config.toml"), "not = [toml").unwrap();
        assert!(!require_causal_evidence(dir.path()));
        std::fs::write(
            dir.path().join("config.toml"),
            "[evolution]\nrequire_causal_evidence = true\n",
        )
        .unwrap();
        assert!(require_causal_evidence(dir.path()));
    }

    #[test]
    fn disabled_config_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[causal]\naudit_ingest = false\n",
        )
        .unwrap();
        let out = ingest_settle(
            dir.path(),
            &SettleFacts {
                agent_id: "a",
                task_id: "t",
                round: 1,
                outcome: SettleOutcome::Rejected,
                goal_kind: "k",
                fault_reason: "model",
                since: "2020-01-01T00:00:00Z",
            },
        );
        assert!(out.is_none());
        assert!(!dir.path().join("memory.db").exists());
    }

    #[test]
    fn no_audit_rows_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let out = ingest_settle(
            dir.path(),
            &SettleFacts {
                agent_id: "a",
                task_id: "t",
                round: 1,
                outcome: SettleOutcome::Rejected,
                goal_kind: "k",
                fault_reason: "model",
                since: "2020-01-01T00:00:00Z",
            },
        );
        assert!(out.is_none(), "an empty audit trail is not a claim");
    }

    #[test]
    fn end_to_end_settle_files_one_replayable_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let since = (Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        duduclaw_security::audit::append_tool_call(dir.path(), "agent-1", "db_select", "{}", true);
        duduclaw_security::audit::append_tool_call_with_extras(
            dir.path(),
            "agent-1",
            "web_fetch_cached",
            "{}",
            false,
            &[("error_class", json!("timeout"))],
        );
        duduclaw_security::audit::append_tool_call(
            dir.path(),
            "agent-1",
            "tasks_complete",
            "{}",
            true,
        );
        let facts = SettleFacts {
            agent_id: "agent-1",
            task_id: "task-9",
            round: 2,
            outcome: SettleOutcome::Rejected,
            goal_kind: "報表產生",
            fault_reason: "r2_infrastructure_failure",
            since: &since,
        };
        let Some(ingested) = ingest_settle(dir.path(), &facts) else {
            panic!("a settle with audit rows must file a candidate");
        };
        assert_eq!(ingested.effect_variable, "task_rejected:報表產生");

        let store = CausalStore::new(dir.path().join("memory.db"));
        let scope = EvidenceScope {
            tenant_id: TENANT_ID.into(),
            acl: ACL.into(),
        };
        let claim = store.read_claim(&scope, &ingested.claim_id).unwrap();
        assert_eq!(claim.review_state, "candidate", "never auto-accepted");
        // The span replays to the exact original bytes.
        let source = store.source_text(&scope, &ingested.artifact_id).unwrap();
        let replayed = source
            .get(ingested.span_start..ingested.span_end)
            .expect("span must land on char boundaries");
        assert!(replayed.contains("web_fetch_cached"));

        // An unreviewed candidate contributes zero support.
        assert!(accepted_cause_tokens(dir.path(), 50).is_empty());
        // Accept it and it appears.
        store
            .review_claim_if_state(&scope, &ingested.claim_id, "operator", true, "candidate")
            .unwrap();
        assert_eq!(
            accepted_cause_tokens(dir.path(), 50),
            vec![ingested.cause_variable.clone()]
        );

        // Same cause+effect again the same day ⇒ deduped, not a second claim.
        assert!(ingest_settle(dir.path(), &facts).is_none());
    }
}
