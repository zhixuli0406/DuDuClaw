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

use duduclaw_core::llm_contract::coverage::{IncompleteReason, RunStatus};
use duduclaw_core::llm_contract::fingerprint::Fingerprint;

use super::intake::RepoProfile;

/// `schema_version` written by this pipeline. A report without the field is
/// read as version 1 (the 2026-08 shape).
pub const CURRENT_SCHEMA_VERSION: u32 = 2;

/// `source_engine` of AI deep-audit findings. Lives here (re-exported by
/// `ai_audit`) so schema-level code (`severity_basis`, the validator) can
/// tell model output from scanner output without depending on `ai_audit`.
pub const AI_AUDIT_ENGINE_NAME: &str = "ai_audit";

/// Severity, ordered low → high so derived `Ord`/`PartialOrd` gives the
/// intuitive `Critical > High > ... > Info` comparison used by `--fail-on`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// `informational` is accepted on read because the severity anchors in
    /// the prompts (`llm_contract::severity::ANCHORS_ZH_TW`) use that word.
    #[serde(alias = "informational")]
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

/// One piece of supporting evidence in a finding's evidence chain
/// (§3.1 "單一 finding 的證據鏈視圖：靜態命中 / AI 分析 / 覆核判定 / PoC transcript").
/// All variants are live: OSS scanners emit `StaticHit`; `ai_audit.rs`
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
    /// An operator decision recorded by the dashboard. Written ONLY by the
    /// gateway (`duduclaw-gateway::secaudit_reports::set_finding_status`),
    /// never by this pipeline. Contract, when an operator sets a status:
    /// - push `{ "kind": "operator_review", "source": "dashboard",
    ///   "detail": "operator_decision: <confirmed|suppressed|refuted>",
    ///   "recorded_at": <RFC3339> }`;
    /// - set the finding's `"severity_basis": "operator"`;
    /// - recompute `summary.by_severity` and `summary.needs_human_by_severity`
    ///   with [`severity_buckets`] (using `summary.gate_includes_needs_human`),
    ///   and `summary.ai_audit_refuted` / `summary.ai_audit_needs_human`
    ///   (ai_audit findings now `refuted` / `needs_human`); no other summary
    ///   field changes.
    OperatorReview,
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

/// Where a finding's `severity` came from (DESIGN-llm-contract-secaudit-v2
/// §3.2). A scanner rule's severity is a property of the rule; an ai_audit
/// severity is the model's own estimate and is labelled as such everywhere it
/// is shown. Dashboard confirmation keeps the original value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeverityBasis {
    /// Also the value a v1 report (no such field) reads as.
    #[default]
    ScannerRule,
    ModelSelfReported,
    Operator,
}

/// The six-slot threat model an ai_audit candidate must declare. Every slot
/// must contain visible text (checked by `precheck`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThreatModel {
    /// Who acts (e.g. an unauthenticated network client).
    pub principal: String,
    /// What they control (the attacker-supplied input).
    pub input: String,
    /// The security control that is supposed to stop them.
    pub control: String,
    /// The trust boundary that is crossed.
    pub boundary: String,
    /// Whose data or which resource is affected.
    pub affected: String,
    /// The concrete result if the claim holds.
    pub result: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceKind {
    Entrypoint,
    Propagation,
    Sink,
}

/// One step of a source-to-sink trace. Several steps: first `entrypoint`,
/// last `sink`, everything in between `propagation`. One step: `entrypoint`
/// or `sink`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceStep {
    pub kind: TraceKind,
    pub file: String,
    pub line: u32,
    /// Function / method / handler name the step sits in.
    pub scope: String,
    pub description: String,
}

/// The closed set of precondition kinds (§3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionKind {
    AuthenticationLevel,
    AuthorizationRole,
    UserInteraction,
    SystemConfiguration,
    NetworkRouting,
    EnvironmentalDependency,
    DataState,
    TimingDependency,
    ThirdPartyDependency,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    pub kind: ConditionKind,
    pub description: String,
}

/// How a human could settle a `NeedsHuman` finding. At least one field is
/// non-empty on a `NeedsHuman` finding.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationPlan {
    /// A check that can be run on a local checkout.
    pub local: Option<String>,
    /// A check that needs the deployed system.
    pub deployment: Option<String>,
}

impl ValidationPlan {
    /// True when neither field carries visible text.
    pub fn is_empty(&self) -> bool {
        let visible = |v: &Option<String>| {
            v.as_deref()
                .is_some_and(duduclaw_core::llm_contract::visible_text::has_visible_content)
        };
        !visible(&self.local) && !visible(&self.deployment)
    }
}

/// Result of the deterministic pre-check an ai_audit candidate goes through
/// before any verifier call (`precheck.rs`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrecheckResult {
    pub passed: bool,
    pub violations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    /// Deterministic id derived from `(source_engine, rule_id, file, line,
    /// snippet)` — locates this exact hit for the dashboard. Changes when
    /// the code above it moves; use [`Finding::root_fingerprint`] to match
    /// "the same issue" across runs.
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

    // ── v2 (all `#[serde(default)]` so a v1 report still reads) ─────────
    /// `Fingerprint::derive(&[engine, rule_id, file, scope])` — never line,
    /// snippet, severity or verdict. Scanner scope is the rule id; ai_audit
    /// scope is the last trace step's `scope` (else the file).
    #[serde(default)]
    pub root_fingerprint: String,
    /// sha256 hex of the file bytes at scan time; `None` when unreadable.
    #[serde(default)]
    pub file_hash: Option<String>,
    #[serde(default)]
    pub severity_basis: SeverityBasis,
    /// Required for ai_audit, `None` for scanners.
    #[serde(default)]
    pub threat_model: Option<ThreatModel>,
    #[serde(default)]
    pub trace: Vec<TraceStep>,
    #[serde(default)]
    pub conditions: Vec<Condition>,
    /// Non-empty exactly on `NeedsHuman` findings.
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub validation_plan: Option<ValidationPlan>,
    /// ai_audit only.
    #[serde(default)]
    pub precheck: Option<PrecheckResult>,
}

impl Finding {
    /// Construct a `Candidate` finding from raw scanner output. `raw_snippet`
    /// is truncated to [`SNIPPET_MAX_BYTES`] here so every call site gets the
    /// cap for free instead of remembering to apply it.
    ///
    /// v2 fields: `root_fingerprint` is derived with `scope = rule_id` (the
    /// scanner rule); ai_audit overwrites it with the trace scope.
    /// `severity_basis` follows the engine.
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
        let root_fingerprint = compute_root_fingerprint(&source_engine, &rule_id, &file, &rule_id);
        let severity_basis = if source_engine == AI_AUDIT_ENGINE_NAME {
            SeverityBasis::ModelSelfReported
        } else {
            SeverityBasis::ScannerRule
        };
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
            root_fingerprint,
            file_hash: None,
            severity_basis,
            threat_model: None,
            trace: Vec::new(),
            conditions: Vec::new(),
            blockers: Vec::new(),
            validation_plan: None,
            precheck: None,
        }
    }

    /// The precheck verdict, when one was recorded and it failed.
    pub fn precheck_failed(&self) -> bool {
        self.precheck.as_ref().is_some_and(|p| !p.passed)
    }
}

/// Stable root-cause identity (§3.2): semantic fields only.
pub fn compute_root_fingerprint(
    engine: &str,
    rule_or_kind: &str,
    file: &str,
    scope: &str,
) -> String {
    Fingerprint::derive(&[engine, rule_or_kind, file, scope])
        .as_str()
        .to_string()
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
            other => Err(format!(
                "unknown profile {other:?} (expected \"quick\" or \"deep\")"
            )),
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

    /// Sum across all severities (= the findings that were recorded into
    /// this bucket — see [`severity_buckets`]).
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

/// Evidence `detail` prefix for a status carried from a prior report
/// (`prior_run.rs`).
pub const CARRIED_FROM_PRIOR_PREFIX: &str = "carried_from_prior:";
/// Evidence `detail` prefix for a prior `Confirmed` finding that is being
/// re-verified on unchanged source.
pub const PRIOR_CONFIRMED_PREFIX: &str = "prior_confirmed_same_source";
/// Evidence `detail` prefix for a deterministic pre-check refutation.
pub const PRECHECK_PREFIX: &str = "precheck:";
/// Evidence `detail` prefix for a location correction the verifier made
/// (`verifier_corrected: line 10→14; trace[2].line 10→14`). Only applied
/// after the corrected values passed the pre-check location/trace rules.
pub const VERIFIER_CORRECTED_PREFIX: &str = "verifier_corrected:";
/// Evidence `detail` prefix for a verifier "plausible" verdict.
pub const PLAUSIBLE_PREFIX: &str = "plausible:";

/// Whether a finding carries an operator decision from the dashboard.
pub fn has_operator_review(f: &Finding) -> bool {
    f.evidence
        .iter()
        .any(|e| e.kind == EvidenceKind::OperatorReview)
}

/// THE severity-bucket rule shared by this pipeline, the report validator
/// and the gateway's dashboard status write (which cannot import this crate
/// and re-implements exactly this body):
///
/// ```text
/// by_severity = {}; needs_human_by_severity = {}
/// for f in findings:
///     match f.status:
///         refuted | suppressed      => skip                 // not actionable
///         needs_human               => if include_needs_human:
///                                          by_severity[f.severity] += 1
///                                      else:
///                                          needs_human_by_severity[f.severity] += 1
///         candidate | confirmed     => by_severity[f.severity] += 1
/// return (by_severity, needs_human_by_severity)
/// ```
///
/// Refuted/Suppressed stay in the report for audit but never count: otherwise
/// adversarial review's false-positive filtering would have no effect on the
/// gate (live-fire 2026-08-18: 4 of 5 refuted ai_audit candidates still
/// tripped `--fail-on high`).
///
/// `include_needs_human` is `summary.gate_includes_needs_human`
/// (`--fail-on-needs-human`). The `--fail-on` gate reads `by_severity` only.
pub fn severity_buckets(
    findings: &[Finding],
    include_needs_human: bool,
) -> (SeverityCounts, SeverityCounts) {
    let mut by_severity = SeverityCounts::default();
    let mut needs_human_by_severity = SeverityCounts::default();
    for f in findings {
        match f.status {
            FindingStatus::Refuted | FindingStatus::Suppressed => {}
            FindingStatus::NeedsHuman => {
                if include_needs_human {
                    by_severity.record(f.severity);
                } else {
                    needs_human_by_severity.record(f.severity);
                }
            }
            FindingStatus::Candidate | FindingStatus::Confirmed => by_severity.record(f.severity),
        }
    }
    (by_severity, needs_human_by_severity)
}

/// Whether an evidence chain carries a status carried from a prior report.
pub fn has_carried_evidence(f: &Finding) -> bool {
    f.evidence
        .iter()
        .any(|e| e.detail.starts_with(CARRIED_FROM_PRIOR_PREFIX))
}

/// Which statuses the `--fail-on` gate reads (D1=B).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GatePolicy {
    /// `--fail-on-needs-human`: also count `NeedsHuman` findings (whose
    /// severity is the model's own estimate) toward `--fail-on`.
    pub include_needs_human: bool,
}

/// Status of one ranked module in the deep-audit coverage list (§3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleCoverageStatus {
    /// Reviewed, zero candidates.
    Covered,
    /// Reviewed, at least one candidate (including ones the pre-check
    /// refuted).
    Candidate,
    /// Not reviewed: outside `--max-modules` (reason `max_modules`), the
    /// run-wide candidate cap was already reached (`candidate_cap`), or the
    /// engine became unavailable before this module (`engine_unavailable`).
    Deferred,
    /// Attempted; no readable text content.
    Unreadable,
    /// Attempted; the LLM call failed.
    LlmFailed,
    /// Attempted; the reply violated the JSON contract (discarded whole).
    ParseFailed,
}

impl ModuleCoverageStatus {
    pub fn is_failed(self) -> bool {
        matches!(
            self,
            ModuleCoverageStatus::Unreadable
                | ModuleCoverageStatus::LlmFailed
                | ModuleCoverageStatus::ParseFailed
        )
    }
}

/// One entry per ranked module, including the ones never sent to the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleCoverage {
    pub module_path: String,
    pub status: ModuleCoverageStatus,
    pub reason: Option<String>,
    /// Paths whose content went into the audit prompt.
    #[serde(default)]
    pub reviewed_paths: Vec<String>,
    /// Paths that went into the prompt only partially (prompt byte budget).
    #[serde(default)]
    pub truncated_paths: Vec<String>,
    /// Ids of the findings this module produced.
    #[serde(default)]
    pub candidate_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageSummary {
    pub modules_total: usize,
    pub covered: usize,
    pub candidate: usize,
    pub deferred: usize,
    pub failed: usize,
    /// At least one module was deferred or failed.
    pub partial: bool,
}

impl CoverageSummary {
    pub fn from_coverage(coverage: &[ModuleCoverage]) -> Self {
        let mut s = CoverageSummary {
            modules_total: coverage.len(),
            ..CoverageSummary::default()
        };
        for m in coverage {
            match m.status {
                ModuleCoverageStatus::Covered => s.covered += 1,
                ModuleCoverageStatus::Candidate => s.candidate += 1,
                ModuleCoverageStatus::Deferred => s.deferred += 1,
                _ => s.failed += 1,
            }
        }
        s.partial = s.deferred + s.failed > 0;
        s
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub total_findings: usize,
    /// Severity distribution the `--fail-on` gate reads (rule:
    /// [`severity_buckets`]): `Candidate` and `Confirmed`, plus `NeedsHuman`
    /// only when `gate_includes_needs_human`. Refuted/Suppressed are inert.
    /// Without the flag `NeedsHuman` (model self-reported severity) is
    /// counted in `needs_human_by_severity` instead. `total_findings` still
    /// counts every finding in the report.
    pub by_severity: SeverityCounts,
    pub engines_run_count: usize,
    pub engines_missing_count: usize,
    /// Count of findings with `source_engine == "ai_audit"` (§3.2 step 3),
    /// regardless of their current status. `#[serde(default)]` so an older
    /// on-disk report (written before this wave) still deserializes.
    #[serde(default)]
    pub ai_audit_candidates: usize,
    /// Of the ai_audit candidates, how many ended `Refuted` (verifier,
    /// pre-check, or carried from a prior report).
    #[serde(default)]
    pub ai_audit_refuted: usize,
    /// Of the ai_audit candidates, how many the verifier judged "plausible"
    /// and parked at `NeedsHuman` (never auto-`Confirmed`).
    #[serde(default)]
    pub ai_audit_needs_human: usize,
    /// Count of findings carrying a genuine [`EvidenceKind::PocTranscript`]
    /// (a real sandboxed execution, not merely an attempted/skipped PoC —
    /// see that variant's doc comment).
    #[serde(default)]
    pub poc_ran: usize,
    /// Severity distribution of `NeedsHuman` findings (model self-reported)
    /// when they are NOT gated; all zero when `gate_includes_needs_human`.
    #[serde(default)]
    pub needs_human_by_severity: SeverityCounts,
    /// `--fail-on-needs-human` was given: `NeedsHuman` findings are counted
    /// in `by_severity` (and therefore gate the run).
    #[serde(default)]
    pub gate_includes_needs_human: bool,
    #[serde(default)]
    pub coverage: CoverageSummary,
    /// ai_audit candidates the deterministic pre-check refuted (zero LLM).
    #[serde(default)]
    pub precheck_refuted: usize,
    /// Findings whose status was carried from a prior report (zero LLM).
    #[serde(default)]
    pub carried_from_prior: usize,
}

impl Summary {
    pub fn from_findings(
        findings: &[Finding],
        engines_run: usize,
        engines_missing: usize,
        gate: GatePolicy,
        coverage: &[ModuleCoverage],
    ) -> Self {
        let (by_severity, needs_human_by_severity) =
            severity_buckets(findings, gate.include_needs_human);
        let mut ai_audit_candidates = 0;
        let mut ai_audit_refuted = 0;
        let mut ai_audit_needs_human = 0;
        let mut poc_ran = 0;
        let mut precheck_refuted = 0;
        let mut carried_from_prior = 0;
        for f in findings {
            if f.source_engine == AI_AUDIT_ENGINE_NAME {
                ai_audit_candidates += 1;
                match f.status {
                    FindingStatus::Refuted => ai_audit_refuted += 1,
                    FindingStatus::NeedsHuman => ai_audit_needs_human += 1,
                    _ => {}
                }
                if f.precheck_failed() {
                    precheck_refuted += 1;
                }
            }
            if f.evidence
                .iter()
                .any(|e| e.kind == EvidenceKind::PocTranscript)
            {
                poc_ran += 1;
            }
            if has_carried_evidence(f) {
                carried_from_prior += 1;
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
            needs_human_by_severity,
            gate_includes_needs_human: gate.include_needs_human,
            coverage: CoverageSummary::from_coverage(coverage),
            precheck_refuted,
            carried_from_prior,
        }
    }

    /// The counts the `--fail-on` gate reads (= `by_severity`).
    pub fn gate_counts(&self) -> SeverityCounts {
        // NeedsHuman is already folded into `by_severity` when the flag is
        // set (see `severity_buckets`), so the gate reads it alone.
        self.by_severity.clone()
    }
}

/// Carry-over from the newest qualifying prior report (§3.4 item 8).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriorRunInfo {
    /// File name of the prior report under `<home>/secaudit/reports/`.
    pub report_file: String,
    pub carried_suppressed: usize,
    pub carried_refuted: usize,
    pub revalidated_prior_confirmed: usize,
    /// Findings whose root fingerprint matched a prior finding but whose
    /// file content changed (or could not be hashed) — not carried.
    pub changed_source: usize,
}

/// D2=B: whether the verifier was a different agent from the auditor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifierIndependence {
    SameAgent,
    DifferentAgent,
    #[default]
    NotRun,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifierInfo {
    pub independence: VerifierIndependence,
    /// Agent id the audit step ran under; `None` = global `[runtime]`.
    pub audit_agent: Option<String>,
    /// Agent id the verifier / PoC ran under; `None` = global `[runtime]`.
    pub verifier_agent: Option<String>,
}

fn schema_version_v1() -> u32 {
    1
}

fn run_status_complete() -> RunStatus {
    RunStatus::Complete
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditReport {
    /// `2` for this pipeline; a report without the field reads as `1`.
    #[serde(default = "schema_version_v1")]
    pub schema_version: u32,
    /// Canonicalized absolute path of the scanned repo.
    pub repo: String,
    /// RFC3339 timestamp of when the scan started.
    pub started_at: String,
    pub profile: ScanProfile,
    pub engines_run: Vec<EngineRun>,
    pub engines_missing: Vec<EngineMissing>,
    pub findings: Vec<Finding>,
    pub summary: Summary,
    #[serde(default = "run_status_complete")]
    pub run_status: RunStatus,
    /// Set exactly when `run_status` is `incomplete`.
    #[serde(default)]
    pub incomplete_reason: Option<IncompleteReason>,
    /// Every ranked module (deep profile), including the deferred ones.
    #[serde(default)]
    pub coverage: Vec<ModuleCoverage>,
    #[serde(default)]
    pub prior_run: Option<PriorRunInfo>,
    #[serde(default)]
    pub verifier: VerifierInfo,
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
        let a = compute_finding_id(
            "gitleaks",
            "generic-api-key",
            "config.py",
            Some(12),
            "snippet",
        );
        let b = compute_finding_id(
            "gitleaks",
            "generic-api-key",
            "config.py",
            Some(12),
            "snippet",
        );
        assert_eq!(a, b);
        assert!(a.starts_with("gitleaks-"));
    }

    #[test]
    fn finding_id_differs_when_any_identity_field_differs() {
        let base = compute_finding_id("gitleaks", "rule", "file.py", Some(1), "snip");
        assert_ne!(
            base,
            compute_finding_id("semgrep", "rule", "file.py", Some(1), "snip")
        );
        assert_ne!(
            base,
            compute_finding_id("gitleaks", "other-rule", "file.py", Some(1), "snip")
        );
        assert_ne!(
            base,
            compute_finding_id("gitleaks", "rule", "other.py", Some(1), "snip")
        );
        assert_ne!(
            base,
            compute_finding_id("gitleaks", "rule", "file.py", Some(2), "snip")
        );
        assert_ne!(
            base,
            compute_finding_id("gitleaks", "rule", "file.py", None, "snip")
        );
        assert_ne!(
            base,
            compute_finding_id("gitleaks", "rule", "file.py", Some(1), "other")
        );
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
            GatePolicy::default(),
            &[],
        );
        assert_eq!(summary.total_findings, 3, "report still lists everything");
        assert_eq!(summary.by_severity.critical, 0, "suppressed is inert");
        // v2 (D1=B): NeedsHuman carries a model self-reported severity, so it
        // moved out of `by_severity` into `needs_human_by_severity`. It used
        // to be the one actionable High here.
        assert_eq!(summary.by_severity.high, 0);
        assert_eq!(summary.needs_human_by_severity.high, 1);
        assert_eq!(summary.by_severity.count_at_or_above(Severity::High), 0);
        assert_eq!(summary.gate_counts().count_at_or_above(Severity::High), 0);
    }

    #[test]
    fn gate_policy_adds_needs_human_only_when_requested() {
        let mut nh = Finding::candidate(
            AI_AUDIT_ENGINE_NAME,
            FindingKind::Other,
            Severity::Critical,
            "t",
            "f",
            None,
            "s",
            "r",
            vec![],
        );
        nh.status = FindingStatus::NeedsHuman;
        let off =
            Summary::from_findings(std::slice::from_ref(&nh), 1, 0, GatePolicy::default(), &[]);
        assert!(!off.gate_includes_needs_human);
        assert_eq!(off.by_severity.critical, 0);
        assert_eq!(off.gate_counts().critical, 0);
        let on = Summary::from_findings(
            &[nh],
            1,
            0,
            GatePolicy {
                include_needs_human: true,
            },
            &[],
        );
        assert!(on.gate_includes_needs_human);
        // Flag on: NeedsHuman moves into by_severity (severity_buckets rule).
        assert_eq!(on.by_severity.critical, 1);
        assert_eq!(on.needs_human_by_severity.critical, 0);
        assert_eq!(on.gate_counts().critical, 1);
        assert_eq!(off.needs_human_by_severity.critical, 1);
    }

    #[test]
    fn severity_buckets_follow_the_documented_rule() {
        let mk = |status, sev| {
            let mut f = Finding::candidate(
                "semgrep",
                FindingKind::StaticAnalysis,
                sev,
                "t",
                "f",
                None,
                "s",
                "r",
                vec![],
            );
            f.status = status;
            f
        };
        let fs = vec![
            mk(FindingStatus::Candidate, Severity::High),
            mk(FindingStatus::Confirmed, Severity::Low),
            mk(FindingStatus::NeedsHuman, Severity::Critical),
            mk(FindingStatus::Refuted, Severity::High),
            mk(FindingStatus::Suppressed, Severity::Medium),
        ];
        let (by, nh) = severity_buckets(&fs, false);
        assert_eq!((by.high, by.low, by.critical, by.medium), (1, 1, 0, 0));
        assert_eq!((nh.critical, nh.total()), (1, 1));
        let (by, nh) = severity_buckets(&fs, true);
        assert_eq!((by.high, by.low, by.critical, by.medium), (1, 1, 1, 0));
        assert_eq!(nh.total(), 0);
    }

    #[test]
    fn operator_review_evidence_kind_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&EvidenceKind::OperatorReview).unwrap(),
            "\"operator_review\""
        );
    }

    #[test]
    fn summary_counts_precheck_refuted_carried_and_coverage() {
        let mut pre = Finding::candidate(
            AI_AUDIT_ENGINE_NAME,
            FindingKind::Other,
            Severity::High,
            "t",
            "f",
            None,
            "s",
            "r",
            vec![],
        );
        pre.status = FindingStatus::Refuted;
        pre.precheck = Some(PrecheckResult {
            passed: false,
            violations: vec!["x".into()],
        });
        let mut carried = Finding::candidate(
            "semgrep",
            FindingKind::StaticAnalysis,
            Severity::High,
            "t",
            "g",
            None,
            "s",
            "r",
            vec![],
        );
        carried.status = FindingStatus::Suppressed;
        carried.evidence.push(EvidenceItem {
            kind: EvidenceKind::AdversarialReview,
            source: "prior_run".into(),
            detail: format!("{CARRIED_FROM_PRIOR_PREFIX} a.json"),
            recorded_at: "t".into(),
        });
        let coverage = vec![
            ModuleCoverage {
                module_path: "a".into(),
                status: ModuleCoverageStatus::Covered,
                reason: None,
                reviewed_paths: vec!["a/x".into()],
                truncated_paths: vec![],
                candidate_ids: vec![],
            },
            ModuleCoverage {
                module_path: "b".into(),
                status: ModuleCoverageStatus::Deferred,
                reason: Some("max_modules".into()),
                reviewed_paths: vec![],
                truncated_paths: vec![],
                candidate_ids: vec![],
            },
            ModuleCoverage {
                module_path: "c".into(),
                status: ModuleCoverageStatus::ParseFailed,
                reason: Some("x".into()),
                reviewed_paths: vec!["c/x".into()],
                truncated_paths: vec![],
                candidate_ids: vec![],
            },
        ];
        let s = Summary::from_findings(&[pre, carried], 1, 0, GatePolicy::default(), &coverage);
        assert_eq!(s.precheck_refuted, 1);
        assert_eq!(s.carried_from_prior, 1);
        assert_eq!(s.coverage.modules_total, 3);
        assert_eq!(s.coverage.covered, 1);
        assert_eq!(s.coverage.deferred, 1);
        assert_eq!(s.coverage.failed, 1);
        assert!(s.coverage.partial);
    }

    /// The 2026-08 v1 report shape (`~/.duduclaw/secaudit/reports/20260817T222007Z.json`):
    /// no schema_version / run_status / coverage / verifier, findings without
    /// any v2 field. It must still deserialize, read as version 1.
    #[test]
    fn v1_report_still_deserializes() {
        let v1 = r#"{
          "repo": "/Users/x/repo",
          "started_at": "2026-08-17T22:20:07Z",
          "profile": {"mode": "quick", "intake": null},
          "engines_run": [{"engine":"semgrep","findings_count":1,"duration_ms":5,"parse_error":null,"timed_out":false}],
          "engines_missing": [{"engine":"osv-scanner","reason":"network"}],
          "findings": [{
            "id": "semgrep-0011223344556677",
            "source_engine": "semgrep",
            "kind": "static_analysis",
            "severity": "high",
            "title": "t",
            "file": "src/a.rs",
            "line": 3,
            "snippet": "s",
            "rule_id": "r",
            "evidence": [{"kind":"static_hit","source":"semgrep","detail":"d","recorded_at":"2026-08-17T22:20:07Z"}],
            "status": "candidate"
          }],
          "summary": {
            "total_findings": 1,
            "by_severity": {"critical":0,"high":1,"medium":0,"low":0,"info":0},
            "engines_run_count": 1,
            "engines_missing_count": 1
          }
        }"#;
        let r: AuditReport = serde_json::from_str(v1).unwrap();
        assert_eq!(r.schema_version, 1);
        assert_eq!(r.run_status, RunStatus::Complete);
        assert!(r.incomplete_reason.is_none());
        assert!(r.coverage.is_empty());
        assert!(r.prior_run.is_none());
        assert_eq!(r.verifier.independence, VerifierIndependence::NotRun);
        let f = &r.findings[0];
        assert!(f.root_fingerprint.is_empty());
        assert!(f.file_hash.is_none());
        assert!(f.trace.is_empty());
        assert!(f.precheck.is_none());
        assert_eq!(r.summary.needs_human_by_severity, SeverityCounts::default());
        assert!(!r.summary.gate_includes_needs_human);
    }

    #[test]
    fn new_enums_serialize_snake_case() {
        assert_eq!(
            serde_json::to_string(&SeverityBasis::ModelSelfReported).unwrap(),
            "\"model_self_reported\""
        );
        assert_eq!(
            serde_json::to_string(&ConditionKind::ThirdPartyDependency).unwrap(),
            "\"third_party_dependency\""
        );
        assert_eq!(
            serde_json::to_string(&ModuleCoverageStatus::LlmFailed).unwrap(),
            "\"llm_failed\""
        );
        assert_eq!(
            serde_json::to_string(&VerifierIndependence::DifferentAgent).unwrap(),
            "\"different_agent\""
        );
        assert!(serde_json::from_str::<ConditionKind>("\"moon_phase\"").is_err());
    }

    #[test]
    fn root_fingerprint_ignores_line_and_snippet() {
        let a = Finding::candidate(
            "semgrep",
            FindingKind::StaticAnalysis,
            Severity::High,
            "t",
            "src/a.rs",
            Some(3),
            "one",
            "rule-x",
            vec![],
        );
        let b = Finding::candidate(
            "semgrep",
            FindingKind::StaticAnalysis,
            Severity::Low,
            "t",
            "src/a.rs",
            Some(90),
            "two",
            "rule-x",
            vec![],
        );
        assert_ne!(a.id, b.id);
        assert_eq!(a.root_fingerprint, b.root_fingerprint);
        assert!(Fingerprint::parse(&a.root_fingerprint).is_ok());
        assert_eq!(a.severity_basis, SeverityBasis::ScannerRule);
    }

    #[test]
    fn severity_accepts_informational_alias() {
        assert_eq!(
            serde_json::from_str::<Severity>("\"informational\"").unwrap(),
            Severity::Info
        );
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
        let summary = Summary::from_findings(&findings, 2, 1, GatePolicy::default(), &[]);
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
            summary: Summary::from_findings(&[], 1, 1, GatePolicy::default(), &[]),
            schema_version: CURRENT_SCHEMA_VERSION,
            run_status: RunStatus::Complete,
            incomplete_reason: None,
            coverage: vec![],
            prior_run: None,
            verifier: VerifierInfo::default(),
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

        let summary = Summary::from_findings(
            &[refuted, needs_human, scanner_hit],
            1,
            0,
            GatePolicy::default(),
            &[],
        );
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
