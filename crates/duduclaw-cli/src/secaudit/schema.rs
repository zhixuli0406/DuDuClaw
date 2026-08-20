//! Finding / report schema for `duduclaw secaudit` (DESIGN-code-security-audit-2026-08 §3.2).
//!
//! This is the stable JSON contract other tooling (dashboard §P1, CI gates,
//! future rule-sedimentation §3.2 step 6) will parse — field names are fixed
//! `snake_case`, enums are fixed-shape (never bare strings that could drift),
//! and every field is additive-only going forward. OSS static scanners
//! (`scanners/`) only ever produce `FindingStatus::Candidate` rows; the AI
//! deep-audit step (`ai_audit.rs`, §3.2 step 3) also only produces
//! `Candidate`, but the adversarial-review step (`adversarial.rs`, §3.2 step
//! 4) settles each of those to `Refuted` or `NeedsHuman` — `Confirmed` is
//! still reserved for a later wave (an operator action via the dashboard),
//! never auto-set by this pipeline.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::intake::RepoProfile;

/// Severity, ordered low → high so derived `Ord`/`PartialOrd` gives the
/// intuitive `Critical > High > ... > Info` comparison used by `--fail-on`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

impl std::str::FromStr for Severity {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "critical" => Ok(Severity::Critical),
            "high" => Ok(Severity::High),
            "medium" | "med" => Ok(Severity::Medium),
            "low" => Ok(Severity::Low),
            "info" | "informational" => Ok(Severity::Info),
            other => Err(format!(
                "unknown severity {other:?} (expected one of: critical, high, medium, low, info)"
            )),
        }
    }
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Critical => "critical",
            Severity::High => "high",
            Severity::Medium => "medium",
            Severity::Low => "low",
            Severity::Info => "info",
        }
    }
}

/// Broad category of a finding — drives how the dashboard groups results and
/// which evidence fields are meaningful. `Other` is the fail-open bucket for
/// a scanner output that doesn't cleanly map (never dropped silently).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    Secret,
    StaticAnalysis,
    DependencyVulnerability,
    Other,
}

/// Lifecycle status. Every OSS scanner hit AND every ai_audit candidate
/// starts as `Candidate` — unverified until adversarial review (§3.2 step 4)
/// or a human looks at it (MAV discipline: "自稱發現不是證據"). OSS scanner
/// findings never move past `Candidate` in this pipeline (§3.2 step 4 only
/// re-verifies ai_audit's own candidates — deterministic scanner hits are
/// already ground truth). `Confirmed` is reserved for an explicit operator
/// action (dashboard); this pipeline never auto-sets it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingStatus {
    /// Raw scanner hit or unreviewed ai_audit candidate.
    Candidate,
    /// An operator confirmed the finding is real and reachable (dashboard
    /// action only — never auto-set by this pipeline).
    Confirmed,
    /// Adversarial review could not reproduce the finding — default lean
    /// per the design's fail-closed-toward-refuted MAV discipline.
    Refuted,
    /// Adversarial review judged the finding "plausible" — parked for an
    /// operator decision, never auto-`Confirmed`. (A verification call that
    /// itself failed or returned an unparseable verdict leaves the finding
    /// at `Candidate` with a `verify_error` evidence note instead — fail
    /// toward "unknown", not toward "needs human".)
    NeedsHuman,
    /// An operator explicitly dismissed the finding (false positive, known
    /// accepted risk, ...).
    Suppressed,
}

impl FindingStatus {
    /// Whether this finding should count toward severity stats and the
    /// `--fail-on` CI gate. Refuted (adversarial review killed it) and
    /// Suppressed (operator dismissed it) findings stay in the report for
    /// audit but must NOT fail a build — otherwise adversarial review's
    /// false-positive filtering has no effect on the gate (live-fire
    /// 2026-08-18: 4 of 5 ai_audit candidates were refuted yet still
    /// tripped `--fail-on high`).
    pub fn is_actionable(&self) -> bool {
        !matches!(self, FindingStatus::Refuted | FindingStatus::Suppressed)
    }
}

/// One piece of supporting evidence in a finding's evidence chain
/// (§3.1 "單一 finding 的證據鏈視圖：靜態命中 / AI 分析 / 覆核判定 / PoC transcript").
/// All five variants are live: OSS scanners emit `StaticHit`; `ai_audit.rs`
/// emits `AiAnalysis`; `adversarial.rs` emits `AdversarialReview`; `poc.rs`
/// emits `PocTranscript` (real execution) or `PocSkipped` (attempted but not
/// executed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// A deterministic OSS scanner hit (semgrep/gitleaks/osv-scanner/cargo-audit).
    StaticHit,
    /// AI deep-audit narrative (§3.2 step 3).
    AiAnalysis,
    /// Adversarial (zero-context) re-verification verdict (§3.2 step 4).
    AdversarialReview,
    /// Sandboxed PoC execution transcript (§3.2 step 5). Per §3.3 the PoC
    /// text itself must never leave the sandbox/report into a channel
    /// message — the source/exit-code/output recorded here are masked
    /// (`duduclaw_security::audit::mask_sensitive_text`) and truncated
    /// before ever reaching this struct. Reserved strictly for a REAL
    /// execution transcript (`poc::execute_in_sandbox` succeeded) — a
    /// skipped/failed/not-demonstrable PoC attempt uses [`EvidenceKind::PocSkipped`]
    /// instead, so `Summary.poc_ran` (a genuine-execution count) can be
    /// derived from evidence kind alone without parsing `detail` text.
    PocTranscript,
    /// A PoC step 5 was attempted for this finding but did not produce a
    /// real sandboxed execution — `detail` explains why (`poc_skipped`:
    /// sandbox infra unavailable; `poc_generation_failed`: the LLM call or
    /// its JSON response failed; `poc_not_demonstrable`: the model
    /// explicitly declined to write a standalone script). Never used to
    /// smuggle a host-run PoC's output — see the hard rule in `poc.rs`.
    PocSkipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceItem {
    pub kind: EvidenceKind,
    /// Where this evidence came from (engine name, e.g. `"semgrep"`).
    pub source: String,
    /// Human-readable supporting detail, already truncated by the producer.
    pub detail: String,
    /// RFC3339 timestamp of when this evidence was recorded.
    pub recorded_at: String,
}

/// Hard cap on `Finding.snippet` (task spec: "snippet 截斷 ≤500B"). Also used
/// as the cap for evidence detail text so a single scanner hit can't balloon
/// the report.
pub const SNIPPET_MAX_BYTES: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    /// Deterministic id derived from `(source_engine, rule_id, file, line,
    /// snippet)` — stable across re-scans of unchanged code so a future
    /// dedup / rule-sedimentation pass (§3.2 step 6) can recognize "the same
    /// finding again" without an external database.
    pub id: String,
    pub source_engine: String,
    pub kind: FindingKind,
    pub severity: Severity,
    pub title: String,
    /// Repo-relative path, POSIX separators, for portability across machines.
    pub file: String,
    /// `None` when the underlying scanner reports a whole-file or
    /// whole-package finding with no line (e.g. a dependency CVE).
    pub line: Option<u32>,
    /// Truncated to [`SNIPPET_MAX_BYTES`]. For secret-kind findings this is
    /// masked (never the raw secret material — see `scanners::gitleaks`).
    pub snippet: String,
    pub rule_id: String,
    pub evidence: Vec<EvidenceItem>,
    pub status: FindingStatus,
}

impl Finding {
    /// Construct a `Candidate` finding from raw scanner output. `raw_snippet`
    /// is truncated to [`SNIPPET_MAX_BYTES`] here so every call site gets the
    /// cap for free instead of remembering to apply it.
    #[allow(clippy::too_many_arguments)]
    pub fn candidate(
        source_engine: impl Into<String>,
        kind: FindingKind,
        severity: Severity,
        title: impl Into<String>,
        file: impl Into<String>,
        line: Option<u32>,
        raw_snippet: &str,
        rule_id: impl Into<String>,
        evidence: Vec<EvidenceItem>,
    ) -> Self {
        let source_engine = source_engine.into();
        let file = file.into();
        let rule_id = rule_id.into();
        let snippet = duduclaw_core::truncate_bytes(raw_snippet, SNIPPET_MAX_BYTES).to_string();
        let id = compute_finding_id(&source_engine, &rule_id, &file, line, &snippet);
        Finding {
            id,
            source_engine,
            kind,
            severity,
            title: title.into(),
            file,
            line,
            snippet,
            rule_id,
            evidence,
            status: FindingStatus::Candidate,
        }
    }
}

/// Deterministic id: sha256 over the identity-bearing fields, truncated to
/// 8 bytes (16 hex chars) and prefixed with the engine name for at-a-glance
/// readability. Not a security boundary — a collision just means two
/// findings dedup into one, which is the desired failure mode.
pub fn compute_finding_id(
    source_engine: &str,
    rule_id: &str,
    file: &str,
    line: Option<u32>,
    snippet: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(source_engine.as_bytes());
    hasher.update([0u8]);
    hasher.update(rule_id.as_bytes());
    hasher.update([0u8]);
    hasher.update(file.as_bytes());
    hasher.update([0u8]);
    hasher.update(line.map(|l| l.to_string()).unwrap_or_default().as_bytes());
    hasher.update([0u8]);
    hasher.update(snippet.as_bytes());
    let digest = hasher.finalize();
    format!("{source_engine}-{}", hex::encode(&digest[..8]))
}

/// `--profile quick|deep`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileMode {
    /// Scanners only (§3.2 step 2).
    Quick,
    /// Scanners + intake/threat-modeling hotspot analysis (§3.2 step 1).
    Deep,
}

impl std::str::FromStr for ProfileMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "quick" => Ok(ProfileMode::Quick),
            "deep" => Ok(ProfileMode::Deep),
            other => Err(format!("unknown profile {other:?} (expected \"quick\" or \"deep\")")),
        }
    }
}

/// The scan mode plus (deep-profile only) the intake/threat-model output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanProfile {
    pub mode: ProfileMode,
    /// `Some` only for `ProfileMode::Deep` — quick profile never runs intake.
    pub intake: Option<RepoProfile>,
}

/// One scanner's execution outcome, whether or not it found anything.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineRun {
    pub engine: String,
    pub findings_count: usize,
    pub duration_ms: u128,
    /// Set when the scanner ran but its output could not be parsed/trusted
    /// (never a panic — scanner output is untrusted DATA). `findings_count`
    /// is 0 whenever this is set.
    pub parse_error: Option<String>,
    /// Set when the process was killed for exceeding the wall-clock budget.
    pub timed_out: bool,
}

/// A scanner that was not run this pass, and why. Always listed explicitly —
/// never silently skipped (project discipline: "工具失效停工上報").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineMissing {
    pub engine: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SeverityCounts {
    pub critical: usize,
    pub high: usize,
    pub medium: usize,
    pub low: usize,
    pub info: usize,
}

impl SeverityCounts {
    pub fn record(&mut self, severity: Severity) {
        match severity {
            Severity::Critical => self.critical += 1,
            Severity::High => self.high += 1,
            Severity::Medium => self.medium += 1,
            Severity::Low => self.low += 1,
            Severity::Info => self.info += 1,
        }
    }

    /// Sum across all severities (= actionable findings, since only those
    /// are recorded — see `Summary::from_findings`).
    pub fn total(&self) -> usize {
        self.critical + self.high + self.medium + self.low + self.info
    }

    /// Count of findings at or above `threshold` (used for `--fail-on`).
    pub fn count_at_or_above(&self, threshold: Severity) -> usize {
        let mut total = 0;
        if Severity::Critical >= threshold {
            total += self.critical;
        }
        if Severity::High >= threshold {
            total += self.high;
        }
        if Severity::Medium >= threshold {
            total += self.medium;
        }
        if Severity::Low >= threshold {
            total += self.low;
        }
        if Severity::Info >= threshold {
            total += self.info;
        }
        total
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub total_findings: usize,
    /// Severity distribution of ACTIONABLE findings only (refuted/suppressed
    /// excluded) — this is what the `--fail-on` gate reads. `total_findings`
    /// still counts every finding in the report.
    pub by_severity: SeverityCounts,
    pub engines_run_count: usize,
    pub engines_missing_count: usize,
    /// Count of findings with `source_engine == "ai_audit"` (§3.2 step 3),
    /// regardless of their current status. `#[serde(default)]` so an older
    /// on-disk report (written before this wave) still deserializes.
    #[serde(default)]
    pub ai_audit_candidates: usize,
    /// Of the ai_audit candidates, how many adversarial review (§3.2 step 4)
    /// refuted.
    #[serde(default)]
    pub ai_audit_refuted: usize,
    /// Of the ai_audit candidates, how many adversarial review judged
    /// "plausible" and parked at `NeedsHuman` for operator review (this wave
    /// never auto-`Confirmed`s — see `adversarial.rs`).
    #[serde(default)]
    pub ai_audit_needs_human: usize,
    /// Count of findings carrying a genuine [`EvidenceKind::PocTranscript`]
    /// (a real sandboxed execution, not merely an attempted/skipped PoC —
    /// see that variant's doc comment).
    #[serde(default)]
    pub poc_ran: usize,
}

impl Summary {
    pub fn from_findings(findings: &[Finding], engines_run: usize, engines_missing: usize) -> Self {
        let mut by_severity = SeverityCounts::default();
        let mut ai_audit_candidates = 0;
        let mut ai_audit_refuted = 0;
        let mut ai_audit_needs_human = 0;
        let mut poc_ran = 0;
        for f in findings {
            // Severity stats (and thus the --fail-on gate, which reads them)
            // count actionable findings only — see FindingStatus::is_actionable.
            if f.status.is_actionable() {
                by_severity.record(f.severity);
            }
            if f.source_engine == "ai_audit" {
                ai_audit_candidates += 1;
                match f.status {
                    FindingStatus::Refuted => ai_audit_refuted += 1,
                    FindingStatus::NeedsHuman => ai_audit_needs_human += 1,
                    _ => {}
                }
            }
            if f.evidence.iter().any(|e| e.kind == EvidenceKind::PocTranscript) {
                poc_ran += 1;
            }
        }
        Summary {
            total_findings: findings.len(),
            by_severity,
            engines_run_count: engines_run,
            engines_missing_count: engines_missing,
            ai_audit_candidates,
            ai_audit_refuted,
            ai_audit_needs_human,
            poc_ran,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditReport {
    /// Canonicalized absolute path of the scanned repo.
    pub repo: String,
    /// RFC3339 timestamp of when the scan started.
    pub started_at: String,
    pub profile: ScanProfile,
    pub engines_run: Vec<EngineRun>,
    pub engines_missing: Vec<EngineMissing>,
    pub findings: Vec<Finding>,
    pub summary: Summary,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_ordering_matches_intuitive_ranking() {
        assert!(Severity::Critical > Severity::High);
        assert!(Severity::High > Severity::Medium);
        assert!(Severity::Medium > Severity::Low);
        assert!(Severity::Low > Severity::Info);
    }

    #[test]
    fn severity_from_str_accepts_known_values_case_insensitively() {
        assert_eq!("CRITICAL".parse::<Severity>().unwrap(), Severity::Critical);
        assert_eq!("High".parse::<Severity>().unwrap(), Severity::High);
        assert_eq!("medium".parse::<Severity>().unwrap(), Severity::Medium);
        assert_eq!("low".parse::<Severity>().unwrap(), Severity::Low);
        assert_eq!("info".parse::<Severity>().unwrap(), Severity::Info);
        assert!("bogus".parse::<Severity>().is_err());
    }

    #[test]
    fn severity_serializes_as_snake_case_string() {
        let json = serde_json::to_string(&Severity::High).unwrap();
        assert_eq!(json, "\"high\"");
    }

    #[test]
    fn profile_mode_from_str_roundtrips() {
        assert_eq!("quick".parse::<ProfileMode>().unwrap(), ProfileMode::Quick);
        assert_eq!("DEEP".parse::<ProfileMode>().unwrap(), ProfileMode::Deep);
        assert!("bogus".parse::<ProfileMode>().is_err());
    }

    #[test]
    fn finding_snippet_is_truncated_to_cap() {
        let long = "x".repeat(SNIPPET_MAX_BYTES * 2);
        let f = Finding::candidate(
            "semgrep",
            FindingKind::StaticAnalysis,
            Severity::Medium,
            "title",
            "src/main.rs",
            Some(10),
            &long,
            "rule-1",
            vec![],
        );
        assert!(f.snippet.len() <= SNIPPET_MAX_BYTES);
    }

    #[test]
    fn finding_snippet_truncation_is_utf8_safe() {
        // CJK chars are 3 bytes each; force a mid-char cut boundary.
        let long = "資安漏洞".repeat(100);
        let f = Finding::candidate(
            "semgrep",
            FindingKind::StaticAnalysis,
            Severity::Low,
            "title",
            "src/main.rs",
            None,
            &long,
            "rule-1",
            vec![],
        );
        assert!(f.snippet.len() <= SNIPPET_MAX_BYTES);
        // Must still be valid UTF-8 (guaranteed by the `String` type, but
        // assert char boundary correctness by round-tripping through chars).
        assert!(f.snippet.chars().count() > 0);
    }

    #[test]
    fn finding_id_is_deterministic_for_identical_inputs() {
        let a = compute_finding_id("gitleaks", "generic-api-key", "config.py", Some(12), "snippet");
        let b = compute_finding_id("gitleaks", "generic-api-key", "config.py", Some(12), "snippet");
        assert_eq!(a, b);
        assert!(a.starts_with("gitleaks-"));
    }

    #[test]
    fn finding_id_differs_when_any_identity_field_differs() {
        let base = compute_finding_id("gitleaks", "rule", "file.py", Some(1), "snip");
        assert_ne!(base, compute_finding_id("semgrep", "rule", "file.py", Some(1), "snip"));
        assert_ne!(base, compute_finding_id("gitleaks", "other-rule", "file.py", Some(1), "snip"));
        assert_ne!(base, compute_finding_id("gitleaks", "rule", "other.py", Some(1), "snip"));
        assert_ne!(base, compute_finding_id("gitleaks", "rule", "file.py", Some(2), "snip"));
        assert_ne!(base, compute_finding_id("gitleaks", "rule", "file.py", None, "snip"));
        assert_ne!(base, compute_finding_id("gitleaks", "rule", "file.py", Some(1), "other"));
    }

    #[test]
    fn severity_counts_at_or_above_threshold() {
        let mut counts = SeverityCounts::default();
        counts.record(Severity::Critical);
        counts.record(Severity::High);
        counts.record(Severity::High);
        counts.record(Severity::Medium);
        counts.record(Severity::Info);
        assert_eq!(counts.count_at_or_above(Severity::High), 3);
        assert_eq!(counts.count_at_or_above(Severity::Critical), 1);
        assert_eq!(counts.count_at_or_above(Severity::Info), 5);
        assert_eq!(counts.count_at_or_above(Severity::Low), 4);
    }

    #[test]
    fn refuted_and_suppressed_findings_are_excluded_from_severity_stats() {
        // Regression (live-fire 2026-08-18): adversarial review refuted 4 of
        // 5 High/Medium candidates, yet by_severity still counted them and
        // --fail-on high failed the build — the review had no effect on the
        // gate. Refuted/Suppressed must be visible in the report but inert
        // in the stats the gate reads.
        let mut refuted_high = Finding::candidate(
            "ai_audit",
            FindingKind::StaticAnalysis,
            Severity::High,
            "t",
            "f",
            None,
            "s",
            "r",
            vec![],
        );
        refuted_high.status = FindingStatus::Refuted;
        let mut suppressed_critical = Finding::candidate(
            "semgrep",
            FindingKind::StaticAnalysis,
            Severity::Critical,
            "t",
            "f",
            None,
            "s",
            "r",
            vec![],
        );
        suppressed_critical.status = FindingStatus::Suppressed;
        let mut needs_human_high = Finding::candidate(
            "ai_audit",
            FindingKind::StaticAnalysis,
            Severity::High,
            "t",
            "f",
            None,
            "s",
            "r",
            vec![],
        );
        needs_human_high.status = FindingStatus::NeedsHuman;

        let summary = Summary::from_findings(
            &[refuted_high, suppressed_critical, needs_human_high],
            1,
            0,
        );
        assert_eq!(summary.total_findings, 3, "report still lists everything");
        assert_eq!(summary.by_severity.critical, 0, "suppressed is inert");
        assert_eq!(
            summary.by_severity.high, 1,
            "only the NeedsHuman high is actionable"
        );
        assert_eq!(summary.by_severity.count_at_or_above(Severity::High), 1);
    }

    #[test]
    fn summary_from_findings_aggregates_by_severity() {
        let findings = vec![
            Finding::candidate(
                "gitleaks",
                FindingKind::Secret,
                Severity::Critical,
                "t",
                "f",
                None,
                "s",
                "r",
                vec![],
            ),
            Finding::candidate(
                "semgrep",
                FindingKind::StaticAnalysis,
                Severity::Medium,
                "t",
                "f",
                None,
                "s",
                "r",
                vec![],
            ),
        ];
        let summary = Summary::from_findings(&findings, 2, 1);
        assert_eq!(summary.total_findings, 2);
        assert_eq!(summary.by_severity.critical, 1);
        assert_eq!(summary.by_severity.medium, 1);
        assert_eq!(summary.engines_run_count, 2);
        assert_eq!(summary.engines_missing_count, 1);
    }

    #[test]
    fn audit_report_round_trips_through_json() {
        let report = AuditReport {
            repo: "/tmp/repo".to_string(),
            started_at: "2026-08-17T00:00:00Z".to_string(),
            profile: ScanProfile {
                mode: ProfileMode::Quick,
                intake: None,
            },
            engines_run: vec![EngineRun {
                engine: "gitleaks".to_string(),
                findings_count: 0,
                duration_ms: 10,
                parse_error: None,
                timed_out: false,
            }],
            engines_missing: vec![EngineMissing {
                engine: "osv-scanner".to_string(),
                reason: "requires network access".to_string(),
            }],
            findings: vec![],
            summary: Summary::from_findings(&[], 1, 1),
        };
        let json = serde_json::to_string(&report).unwrap();
        let back: AuditReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back.repo, "/tmp/repo");
        assert_eq!(back.engines_run.len(), 1);
        assert_eq!(back.engines_missing[0].engine, "osv-scanner");
    }

    #[test]
    fn summary_tracks_ai_audit_candidates_by_status_and_poc_evidence() {
        let mut refuted = Finding::candidate(
            "ai_audit",
            FindingKind::Other,
            Severity::High,
            "t",
            "f",
            None,
            "s",
            "r",
            vec![],
        );
        refuted.status = FindingStatus::Refuted;

        let mut needs_human = Finding::candidate(
            "ai_audit",
            FindingKind::Other,
            Severity::Critical,
            "t",
            "f",
            None,
            "s",
            "r",
            vec![],
        );
        needs_human.status = FindingStatus::NeedsHuman;
        needs_human.evidence.push(EvidenceItem {
            kind: EvidenceKind::PocTranscript,
            source: "poc".to_string(),
            detail: "exit_code=1".to_string(),
            recorded_at: "2026-08-18T00:00:00Z".to_string(),
        });

        let scanner_hit = Finding::candidate(
            "semgrep",
            FindingKind::StaticAnalysis,
            Severity::High,
            "t",
            "f",
            None,
            "s",
            "r",
            vec![],
        );

        let summary = Summary::from_findings(&[refuted, needs_human, scanner_hit], 1, 0);
        assert_eq!(summary.ai_audit_candidates, 2);
        assert_eq!(summary.ai_audit_refuted, 1);
        assert_eq!(summary.ai_audit_needs_human, 1);
        assert_eq!(summary.poc_ran, 1);
    }

    #[test]
    fn summary_new_fields_default_to_zero_when_deserializing_an_old_report() {
        // A report saved before this wave has no ai_audit_* / poc_ran keys.
        let old = r#"{
            "total_findings": 0,
            "by_severity": {"critical":0,"high":0,"medium":0,"low":0,"info":0},
            "engines_run_count": 1,
            "engines_missing_count": 0
        }"#;
        let summary: Summary = serde_json::from_str(old).unwrap();
        assert_eq!(summary.ai_audit_candidates, 0);
        assert_eq!(summary.poc_ran, 0);
    }

    #[test]
    fn poc_skipped_evidence_kind_serializes_distinct_from_poc_transcript() {
        let json = serde_json::to_string(&EvidenceKind::PocSkipped).unwrap();
        assert_eq!(json, "\"poc_skipped\"");
        assert_ne!(EvidenceKind::PocSkipped, EvidenceKind::PocTranscript);
    }

    #[test]
    fn field_names_are_snake_case_in_json() {
        let item = EvidenceItem {
            kind: EvidenceKind::StaticHit,
            source: "semgrep".to_string(),
            detail: "d".to_string(),
            recorded_at: "2026-08-17T00:00:00Z".to_string(),
        };
        let json = serde_json::to_value(&item).unwrap();
        assert!(json.get("recorded_at").is_some());
        assert_eq!(json["kind"], serde_json::json!("static_hit"));
    }
}
