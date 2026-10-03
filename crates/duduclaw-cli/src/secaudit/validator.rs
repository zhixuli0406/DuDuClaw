//! Report validator (DESIGN-llm-contract-secaudit-v2 §3.4 item 9).
//!
//! The semantic invariants of an [`AuditReport`] are checked by this code,
//! not trusted to the pipeline that produced the report. `write_json_report`
//! (and therefore `--save`) runs it before writing anything: a report with a
//! violation is never written, the violations are printed and the command
//! exits 2. `duduclaw secaudit-validate <report.json>` runs it on a file.
//!
//! Interpretations where the design is silent:
//! - Status provenance. An operator decision (dashboard) is recorded by the
//!   gateway as an `operator_review` evidence item plus
//!   `severity_basis = operator` (contract on `EvidenceKind::OperatorReview`).
//!   `Confirmed` is valid only with that operator provenance (either
//!   marker). `Refuted` and `Suppressed` are valid with operator provenance
//!   OR an `adversarial_review` evidence item (verifier verdict, pre-check,
//!   or `carried_from_prior:`).
//! - Summary after a dashboard edit: the gateway recomputes
//!   `by_severity` / `needs_human_by_severity` (rule:
//!   `schema::severity_buckets`) and `ai_audit_refuted` /
//!   `ai_audit_needs_human`, so every summary field is compared strictly,
//!   operator-edited or not. Blockers left on a
//!   former `NeedsHuman` finding the operator settled are allowed.
//! - `verifier.independence = not_run` means no finding carries evidence
//!   from the verifier (`source == "adversarial"`).
//! - A finding the pre-check refuted keeps the model's claim verbatim (it
//!   is the record of why it was refuted), so the file / line / trace
//!   location rules are not applied to it.
//! - Coverage status shape: `candidate` ⇔ non-empty `candidate_ids`;
//!   `covered` has none; `deferred` has no reviewed paths.

use std::collections::HashSet;
use std::fmt;

use duduclaw_core::llm_contract::coverage::RunStatus;
use duduclaw_core::llm_contract::fingerprint::Fingerprint;
use duduclaw_core::llm_contract::safe_path::SafeRepoPath;

use super::precheck::trace_shape_violations;
use super::schema::{
    AI_AUDIT_ENGINE_NAME, AuditReport, CURRENT_SCHEMA_VERSION, EvidenceKind, Finding,
    FindingStatus, GatePolicy, ModuleCoverageStatus, PLAUSIBLE_PREFIX, SeverityBasis, Summary,
    VERIFIER_CORRECTED_PREFIX, VerifierIndependence, has_operator_review,
};

/// Closed set of report violations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportViolation {
    SchemaVersion {
        found: u32,
    },
    DuplicateFindingId {
        id: String,
    },
    UnsafeFindingPath {
        id: String,
        detail: String,
    },
    LineZero {
        id: String,
    },
    InvalidRootFingerprint {
        id: String,
    },
    TraceShape {
        id: String,
        detail: String,
    },
    RefutedWithoutReviewEvidence {
        id: String,
    },
    NeedsHumanWithoutPlausible {
        id: String,
    },
    NeedsHumanWithoutBlockers {
        id: String,
    },
    NeedsHumanWithoutValidationPlan {
        id: String,
    },
    BlockersOnNonNeedsHuman {
        id: String,
    },
    StatusWithoutProvenance {
        id: String,
        status: FindingStatus,
    },
    SeverityBasisMismatch {
        id: String,
        engine: String,
        basis: SeverityBasis,
    },
    SummaryMismatch {
        field: &'static str,
    },
    DuplicateCoverageModule {
        module_path: String,
    },
    CoverageCandidateMissing {
        module_path: String,
        id: String,
    },
    CoverageStatusShape {
        module_path: String,
        detail: String,
    },
    IncompleteWithoutReason,
    CompleteWithReason,
    VerifierInconsistent {
        detail: String,
    },
    /// A finding carrying a `verifier_corrected:` evidence item must have
    /// `precheck.passed == true`.
    CorrectedWithoutPassedPrecheck {
        id: String,
    },
}

impl fmt::Display for ReportViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use ReportViolation::*;
        match self {
            SchemaVersion { found } => {
                write!(
                    f,
                    "schema_version is {found}, expected {CURRENT_SCHEMA_VERSION}"
                )
            }
            DuplicateFindingId { id } => write!(f, "finding id {id} appears more than once"),
            UnsafeFindingPath { id, detail } => {
                write!(
                    f,
                    "finding {id}: file is not a safe repo-relative path ({detail})"
                )
            }
            LineZero { id } => write!(f, "finding {id}: line must be >= 1"),
            InvalidRootFingerprint { id } => {
                write!(f, "finding {id}: root_fingerprint is missing or malformed")
            }
            TraceShape { id, detail } => write!(f, "finding {id}: trace shape: {detail}"),
            RefutedWithoutReviewEvidence { id } => write!(
                f,
                "finding {id}: refuted without adversarial_review evidence (verifier, precheck or carried) or an operator decision"
            ),
            NeedsHumanWithoutPlausible { id } => {
                write!(
                    f,
                    "finding {id}: needs_human without a plausible verdict in evidence"
                )
            }
            NeedsHumanWithoutBlockers { id } => {
                write!(f, "finding {id}: needs_human with empty blockers")
            }
            NeedsHumanWithoutValidationPlan { id } => {
                write!(f, "finding {id}: needs_human without a validation plan")
            }
            BlockersOnNonNeedsHuman { id } => {
                write!(
                    f,
                    "finding {id}: blockers set on a finding that is not needs_human"
                )
            }
            StatusWithoutProvenance { id, status } => write!(
                f,
                "finding {id}: status {status:?} must come from an operator decision (or, for suppressed, review/carried evidence)"
            ),
            SeverityBasisMismatch { id, engine, basis } => write!(
                f,
                "finding {id}: severity_basis {basis:?} does not match engine {engine}"
            ),
            SummaryMismatch { field } => {
                write!(f, "summary.{field} does not match the findings/coverage")
            }
            DuplicateCoverageModule { module_path } => {
                write!(f, "coverage lists module {module_path} more than once")
            }
            CoverageCandidateMissing { module_path, id } => write!(
                f,
                "coverage module {module_path} references finding {id} which is not in findings"
            ),
            CoverageStatusShape {
                module_path,
                detail,
            } => write!(f, "coverage module {module_path}: {detail}"),
            IncompleteWithoutReason => {
                write!(f, "run_status is incomplete but incomplete_reason is null")
            }
            CompleteWithReason => {
                write!(f, "run_status is complete but incomplete_reason is set")
            }
            VerifierInconsistent { detail } => write!(f, "verifier: {detail}"),
            CorrectedWithoutPassedPrecheck { id } => write!(
                f,
                "finding {id}: carries a verifier_corrected record but precheck.passed is not true"
            ),
        }
    }
}

impl std::error::Error for ReportViolation {}

fn check_finding(f: &Finding, out: &mut Vec<ReportViolation>) {
    let id = || f.id.clone();
    // A pre-check refutation keeps the model's claim verbatim (an unsafe
    // path, line 0, a malformed trace) as the record of WHY it was refuted;
    // the location rules apply to every other finding.
    let refuted_by_precheck = f.precheck_failed() && f.status == FindingStatus::Refuted;
    if !refuted_by_precheck {
        if let Err(e) = SafeRepoPath::parse(&f.file) {
            out.push(ReportViolation::UnsafeFindingPath {
                id: id(),
                detail: e.to_string(),
            });
        }
        if f.line == Some(0) {
            out.push(ReportViolation::LineZero { id: id() });
        }
    }
    if Fingerprint::parse(&f.root_fingerprint).is_err() {
        out.push(ReportViolation::InvalidRootFingerprint { id: id() });
    }
    if !refuted_by_precheck {
        for (i, step) in f.trace.iter().enumerate() {
            if step.line == 0 {
                out.push(ReportViolation::TraceShape {
                    id: id(),
                    detail: format!("trace[{i}].line must be >= 1"),
                });
            }
            if let Err(e) = SafeRepoPath::parse(&step.file) {
                out.push(ReportViolation::TraceShape {
                    id: id(),
                    detail: format!("trace[{i}].file unsafe ({e})"),
                });
            }
        }
        for v in trace_shape_violations(&f.trace) {
            out.push(ReportViolation::TraceShape {
                id: id(),
                detail: v,
            });
        }
    }

    let has_review = f
        .evidence
        .iter()
        .any(|e| e.kind == EvidenceKind::AdversarialReview);
    let by_operator = has_operator_review(f) || f.severity_basis == SeverityBasis::Operator;
    match f.status {
        FindingStatus::Refuted if !(has_review || by_operator) => {
            out.push(ReportViolation::RefutedWithoutReviewEvidence { id: id() })
        }
        FindingStatus::NeedsHuman => {
            let plausible = f.evidence.iter().any(|e| {
                e.kind == EvidenceKind::AdversarialReview && e.detail.starts_with(PLAUSIBLE_PREFIX)
            });
            if !plausible {
                out.push(ReportViolation::NeedsHumanWithoutPlausible { id: id() });
            }
            if f.blockers.is_empty() {
                out.push(ReportViolation::NeedsHumanWithoutBlockers { id: id() });
            }
            if f.validation_plan.as_ref().is_none_or(|p| p.is_empty()) {
                out.push(ReportViolation::NeedsHumanWithoutValidationPlan { id: id() });
            }
        }
        FindingStatus::Suppressed if !(has_review || by_operator) => {
            out.push(ReportViolation::StatusWithoutProvenance {
                id: id(),
                status: f.status,
            })
        }
        FindingStatus::Confirmed if !by_operator => {
            out.push(ReportViolation::StatusWithoutProvenance {
                id: id(),
                status: f.status,
            })
        }
        _ => {}
    }
    // An operator decision on a former NeedsHuman finding keeps the
    // verifier's blockers as history (the gateway only changes status).
    if f.status != FindingStatus::NeedsHuman && !f.blockers.is_empty() && !by_operator {
        out.push(ReportViolation::BlockersOnNonNeedsHuman { id: id() });
    }

    // A verifier correction is only ever applied to a finding that passed
    // the pre-check (and the corrected values re-passed it).
    let corrected = f
        .evidence
        .iter()
        .any(|e| e.detail.starts_with(VERIFIER_CORRECTED_PREFIX));
    if corrected && !f.precheck.as_ref().is_some_and(|p| p.passed) {
        out.push(ReportViolation::CorrectedWithoutPassedPrecheck { id: id() });
    }

    let is_ai = f.source_engine == AI_AUDIT_ENGINE_NAME;
    let basis_ok = match f.severity_basis {
        SeverityBasis::Operator => true,
        SeverityBasis::ModelSelfReported => is_ai,
        SeverityBasis::ScannerRule => !is_ai,
    };
    if !basis_ok {
        out.push(ReportViolation::SeverityBasisMismatch {
            id: id(),
            engine: f.source_engine.clone(),
            basis: f.severity_basis,
        });
    }
}

fn check_summary(r: &AuditReport, out: &mut Vec<ReportViolation>) {
    let expect = Summary::from_findings(
        &r.findings,
        r.engines_run.len(),
        r.engines_missing.len(),
        GatePolicy {
            include_needs_human: r.summary.gate_includes_needs_human,
        },
        &r.coverage,
    );
    let got = &r.summary;
    let mut field = |name: &'static str, same: bool| {
        if !same {
            out.push(ReportViolation::SummaryMismatch { field: name });
        }
    };
    field(
        "total_findings",
        got.total_findings == expect.total_findings,
    );
    field("by_severity", got.by_severity == expect.by_severity);
    field(
        "engines_run_count",
        got.engines_run_count == expect.engines_run_count,
    );
    field(
        "engines_missing_count",
        got.engines_missing_count == expect.engines_missing_count,
    );
    field(
        "ai_audit_candidates",
        got.ai_audit_candidates == expect.ai_audit_candidates,
    );
    field(
        "ai_audit_refuted",
        got.ai_audit_refuted == expect.ai_audit_refuted,
    );
    field(
        "ai_audit_needs_human",
        got.ai_audit_needs_human == expect.ai_audit_needs_human,
    );
    field("poc_ran", got.poc_ran == expect.poc_ran);
    field(
        "needs_human_by_severity",
        got.needs_human_by_severity == expect.needs_human_by_severity,
    );
    field("coverage", got.coverage == expect.coverage);
    field(
        "precheck_refuted",
        got.precheck_refuted == expect.precheck_refuted,
    );
    field(
        "carried_from_prior",
        got.carried_from_prior == expect.carried_from_prior,
    );
}

fn check_coverage(r: &AuditReport, ids: &HashSet<&str>, out: &mut Vec<ReportViolation>) {
    let mut seen: HashSet<&str> = HashSet::new();
    for m in &r.coverage {
        if !seen.insert(m.module_path.as_str()) {
            out.push(ReportViolation::DuplicateCoverageModule {
                module_path: m.module_path.clone(),
            });
        }
        for cid in &m.candidate_ids {
            if !ids.contains(cid.as_str()) {
                out.push(ReportViolation::CoverageCandidateMissing {
                    module_path: m.module_path.clone(),
                    id: cid.clone(),
                });
            }
        }
        let shape = |detail: &str| ReportViolation::CoverageStatusShape {
            module_path: m.module_path.clone(),
            detail: detail.to_string(),
        };
        match m.status {
            ModuleCoverageStatus::Candidate if m.candidate_ids.is_empty() => {
                out.push(shape("candidate status with no candidate_ids"))
            }
            ModuleCoverageStatus::Covered if !m.candidate_ids.is_empty() => {
                out.push(shape("covered status with candidate_ids"))
            }
            ModuleCoverageStatus::Deferred if !m.reviewed_paths.is_empty() => {
                out.push(shape("deferred status with reviewed_paths"))
            }
            ModuleCoverageStatus::Deferred if m.reason.is_none() => {
                out.push(shape("deferred status without a reason"))
            }
            s if s.is_failed() && m.reason.is_none() => {
                out.push(shape("failed status without a reason"))
            }
            _ => {}
        }
    }
}

fn check_verifier(r: &AuditReport, out: &mut Vec<ReportViolation>) {
    let v = &r.verifier;
    let mut bad = |detail: &str| {
        out.push(ReportViolation::VerifierInconsistent {
            detail: detail.to_string(),
        })
    };
    match v.independence {
        VerifierIndependence::SameAgent if v.audit_agent != v.verifier_agent => {
            bad("independence same_agent but audit_agent differs from verifier_agent")
        }
        VerifierIndependence::DifferentAgent if v.audit_agent == v.verifier_agent => {
            bad("independence different_agent but audit_agent equals verifier_agent")
        }
        VerifierIndependence::NotRun => {
            let any = r
                .findings
                .iter()
                .flat_map(|f| f.evidence.iter())
                .any(|e| e.source == "adversarial");
            if any {
                bad("independence not_run but findings carry verifier evidence");
            }
        }
        _ => {}
    }
}

/// Check every rule in the module doc. Empty ⇒ valid.
pub fn validate_report(r: &AuditReport) -> Vec<ReportViolation> {
    let mut out = Vec::new();
    if r.schema_version != CURRENT_SCHEMA_VERSION {
        out.push(ReportViolation::SchemaVersion {
            found: r.schema_version,
        });
    }
    let mut ids: HashSet<&str> = HashSet::new();
    for f in &r.findings {
        if !ids.insert(f.id.as_str()) {
            out.push(ReportViolation::DuplicateFindingId { id: f.id.clone() });
        }
        check_finding(f, &mut out);
    }
    check_summary(r, &mut out);
    check_coverage(r, &ids, &mut out);
    match (r.run_status, r.incomplete_reason) {
        (RunStatus::Incomplete, None) => out.push(ReportViolation::IncompleteWithoutReason),
        (RunStatus::Complete, Some(_)) => out.push(ReportViolation::CompleteWithReason),
        _ => {}
    }
    check_verifier(r, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secaudit::schema::{
        CARRIED_FROM_PRIOR_PREFIX, EvidenceItem, FindingKind, ModuleCoverage, ProfileMode,
        ScanProfile, Severity, TraceKind, TraceStep, ValidationPlan, VerifierInfo,
    };
    use duduclaw_core::llm_contract::coverage::IncompleteReason;

    fn ev(source: &str, detail: &str) -> EvidenceItem {
        EvidenceItem {
            kind: EvidenceKind::AdversarialReview,
            source: source.into(),
            detail: detail.into(),
            recorded_at: "t".into(),
        }
    }

    fn scanner(file: &str) -> Finding {
        Finding::candidate(
            "semgrep",
            FindingKind::StaticAnalysis,
            Severity::High,
            "t",
            file,
            Some(1),
            "s",
            "r",
            vec![],
        )
    }

    fn ai(file: &str) -> Finding {
        let mut f = Finding::candidate(
            AI_AUDIT_ENGINE_NAME,
            FindingKind::Other,
            Severity::High,
            "t",
            file,
            Some(1),
            "s",
            "ai-audit/x",
            vec![],
        );
        f.trace = vec![TraceStep {
            kind: TraceKind::Sink,
            file: file.into(),
            line: 1,
            scope: "f".into(),
            description: "d".into(),
        }];
        f
    }

    fn needs_human(file: &str) -> Finding {
        let mut f = ai(file);
        f.status = FindingStatus::NeedsHuman;
        f.blockers = vec!["b".into()];
        f.validation_plan = Some(ValidationPlan {
            local: Some("x".into()),
            deployment: None,
        });
        f.evidence.push(ev("adversarial", "plausible: r"));
        f
    }

    /// A report whose summary is recomputed from its findings (so only the
    /// rule under test can fail).
    fn report(findings: Vec<Finding>, coverage: Vec<ModuleCoverage>) -> AuditReport {
        let any_adv = findings
            .iter()
            .flat_map(|f| f.evidence.iter())
            .any(|e| e.source == "adversarial");
        AuditReport {
            schema_version: CURRENT_SCHEMA_VERSION,
            repo: "/repo".into(),
            started_at: "t".into(),
            profile: ScanProfile {
                mode: ProfileMode::Deep,
                intake: None,
            },
            engines_run: vec![],
            engines_missing: vec![],
            summary: Summary::from_findings(&findings, 0, 0, GatePolicy::default(), &coverage),
            findings,
            run_status: RunStatus::Complete,
            incomplete_reason: None,
            coverage,
            prior_run: None,
            verifier: VerifierInfo {
                independence: if any_adv {
                    VerifierIndependence::SameAgent
                } else {
                    VerifierIndependence::NotRun
                },
                audit_agent: None,
                verifier_agent: None,
            },
        }
    }

    fn only(r: &AuditReport) -> Vec<ReportViolation> {
        validate_report(r)
    }

    #[test]
    fn a_well_formed_report_is_valid() {
        let mut refuted = ai("src/b.rs");
        refuted.status = FindingStatus::Refuted;
        refuted.evidence.push(ev("adversarial", "refuted: no"));
        let cov = vec![ModuleCoverage {
            module_path: "src".into(),
            status: ModuleCoverageStatus::Candidate,
            reason: None,
            reviewed_paths: vec!["src/a.rs".into()],
            truncated_paths: vec![],
            candidate_ids: vec![refuted.id.clone()],
        }];
        let r = report(
            vec![scanner("src/a.rs"), needs_human("src/c.rs"), refuted],
            cov,
        );
        assert_eq!(only(&r), vec![]);
    }

    #[test]
    fn schema_version_must_be_2() {
        let mut r = report(vec![], vec![]);
        r.schema_version = 1;
        assert_eq!(only(&r), vec![ReportViolation::SchemaVersion { found: 1 }]);
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let a = scanner("src/a.rs");
        let r = report(vec![a.clone(), a.clone()], vec![]);
        assert!(only(&r).contains(&ReportViolation::DuplicateFindingId { id: a.id }));
    }

    #[test]
    fn unsafe_file_line_zero_and_bad_fingerprint() {
        let mut f = scanner("../etc/passwd");
        f.line = Some(0);
        f.root_fingerprint = "!bad".into();
        let id = f.id.clone();
        let v = only(&report(vec![f], vec![]));
        assert!(
            v.iter()
                .any(|x| matches!(x, ReportViolation::UnsafeFindingPath { .. }))
        );
        assert!(v.contains(&ReportViolation::LineZero { id: id.clone() }));
        assert!(v.contains(&ReportViolation::InvalidRootFingerprint { id }));
    }

    #[test]
    fn trace_shape_is_checked_unless_precheck_refuted() {
        let mut f = ai("src/a.rs");
        f.trace[0].kind = TraceKind::Propagation;
        assert!(
            only(&report(vec![f.clone()], vec![]))
                .iter()
                .any(|x| matches!(x, ReportViolation::TraceShape { .. }))
        );
        crate::secaudit::precheck::apply_precheck(&mut f, vec!["trace[0].kind".into()]);
        assert_eq!(only(&report(vec![f], vec![])), vec![]);
    }

    #[test]
    fn precheck_refuted_hostile_path_is_recorded_not_rejected() {
        let mut f = ai("/etc/passwd");
        f.line = Some(0);
        crate::secaudit::precheck::apply_precheck(&mut f, vec!["file: unsafe path".into()]);
        assert_eq!(only(&report(vec![f], vec![])), vec![]);
    }

    #[test]
    fn refuted_needs_review_evidence() {
        let mut f = scanner("src/a.rs");
        f.status = FindingStatus::Refuted;
        let id = f.id.clone();
        assert_eq!(
            only(&report(vec![f], vec![])),
            vec![ReportViolation::RefutedWithoutReviewEvidence { id }]
        );
    }

    #[test]
    fn needs_human_needs_plausible_blockers_and_plan() {
        let mut f = needs_human("src/a.rs");
        f.evidence.clear();
        f.blockers.clear();
        f.validation_plan = None;
        let id = f.id.clone();
        let v = only(&report(vec![f], vec![]));
        assert!(v.contains(&ReportViolation::NeedsHumanWithoutPlausible { id: id.clone() }));
        assert!(v.contains(&ReportViolation::NeedsHumanWithoutBlockers { id: id.clone() }));
        assert!(v.contains(&ReportViolation::NeedsHumanWithoutValidationPlan { id }));
    }

    #[test]
    fn blockers_only_on_needs_human() {
        let mut f = scanner("src/a.rs");
        f.blockers = vec!["x".into()];
        let id = f.id.clone();
        assert_eq!(
            only(&report(vec![f], vec![])),
            vec![ReportViolation::BlockersOnNonNeedsHuman { id }]
        );
    }

    #[test]
    fn suppressed_and_confirmed_need_operator_or_carried_provenance() {
        let mut s = scanner("src/a.rs");
        s.status = FindingStatus::Suppressed;
        let mut c = scanner("src/b.rs");
        c.status = FindingStatus::Confirmed;
        let v = only(&report(vec![s.clone(), c.clone()], vec![]));
        assert_eq!(v.len(), 2, "{v:?}");
        s.evidence.push(ev(
            "prior_run",
            &format!("{CARRIED_FROM_PRIOR_PREFIX} p.json"),
        ));
        c.severity_basis = SeverityBasis::Operator;
        assert_eq!(only(&report(vec![s, c], vec![])), vec![]);
    }

    /// The gateway's dashboard write (contract on
    /// `EvidenceKind::OperatorReview`): status + operator_review evidence +
    /// severity_basis operator + by_severity / needs_human_by_severity
    /// recomputed with `severity_buckets`; nothing else in the summary.
    fn operator_edit(r: &mut AuditReport, idx: usize, status: FindingStatus) {
        let f = &mut r.findings[idx];
        f.status = status;
        f.severity_basis = SeverityBasis::Operator;
        f.evidence.push(EvidenceItem {
            kind: EvidenceKind::OperatorReview,
            source: "dashboard".into(),
            detail: format!("operator_decision: {status:?}").to_lowercase(),
            recorded_at: "2026-10-03T00:00:00Z".into(),
        });
        let (by, nh) = crate::secaudit::schema::severity_buckets(
            &r.findings,
            r.summary.gate_includes_needs_human,
        );
        r.summary.by_severity = by;
        r.summary.needs_human_by_severity = nh;
        r.summary.ai_audit_refuted = r
            .findings
            .iter()
            .filter(|f| {
                f.source_engine == AI_AUDIT_ENGINE_NAME && f.status == FindingStatus::Refuted
            })
            .count();
        r.summary.ai_audit_needs_human = r
            .findings
            .iter()
            .filter(|f| {
                f.source_engine == AI_AUDIT_ENGINE_NAME && f.status == FindingStatus::NeedsHuman
            })
            .count();
    }

    #[test]
    fn operator_decisions_validate_for_each_status() {
        for status in [
            FindingStatus::Confirmed,
            FindingStatus::Suppressed,
            FindingStatus::Refuted,
        ] {
            // A scanner finding and an ai_audit NeedsHuman finding.
            let mut r = report(vec![scanner("src/a.rs"), needs_human("src/c.rs")], vec![]);
            operator_edit(&mut r, 0, status);
            assert_eq!(only(&r), vec![], "scanner -> {status:?}");
            operator_edit(&mut r, 1, status);
            assert_eq!(only(&r), vec![], "needs_human -> {status:?}");
        }
    }

    #[test]
    fn operator_review_evidence_alone_or_basis_alone_is_enough() {
        let mut ev_only = scanner("src/a.rs");
        ev_only.status = FindingStatus::Confirmed;
        ev_only.evidence.push(EvidenceItem {
            kind: EvidenceKind::OperatorReview,
            source: "dashboard".into(),
            detail: "operator_decision: confirmed".into(),
            recorded_at: "t".into(),
        });
        let mut basis_only = scanner("src/b.rs");
        basis_only.status = FindingStatus::Refuted;
        basis_only.severity_basis = SeverityBasis::Operator;
        assert_eq!(only(&report(vec![ev_only, basis_only], vec![])), vec![]);
    }

    #[test]
    fn operator_edit_without_summary_recompute_is_rejected() {
        let mut r = report(vec![scanner("src/a.rs")], vec![]);
        let before = r.summary.clone();
        operator_edit(&mut r, 0, FindingStatus::Suppressed);
        r.summary = before;
        assert!(only(&r).contains(&ReportViolation::SummaryMismatch {
            field: "by_severity"
        }));
    }

    #[test]
    fn operator_edit_with_stale_ai_audit_counters_is_rejected() {
        let mut r = report(vec![needs_human("src/c.rs")], vec![]);
        let stale = r.summary.clone();
        operator_edit(&mut r, 0, FindingStatus::Refuted);
        r.summary.ai_audit_refuted = stale.ai_audit_refuted;
        r.summary.ai_audit_needs_human = stale.ai_audit_needs_human;
        let v = only(&r);
        assert!(v.contains(&ReportViolation::SummaryMismatch {
            field: "ai_audit_refuted"
        }));
        assert!(v.contains(&ReportViolation::SummaryMismatch {
            field: "ai_audit_needs_human"
        }));
    }

    #[test]
    fn confirmed_needs_operator_provenance_even_with_review_evidence() {
        let mut c = ai("src/a.rs");
        c.status = FindingStatus::Confirmed;
        c.evidence.push(ev("adversarial", "plausible: r"));
        let id = c.id.clone();
        let v = only(&report(vec![c], vec![]));
        assert!(v.contains(&ReportViolation::StatusWithoutProvenance {
            id,
            status: FindingStatus::Confirmed
        }));
    }

    #[test]
    fn severity_basis_must_match_engine() {
        let mut a = ai("src/a.rs");
        a.severity_basis = SeverityBasis::ScannerRule;
        let mut b = scanner("src/b.rs");
        b.severity_basis = SeverityBasis::ModelSelfReported;
        let v = only(&report(vec![a, b], vec![]));
        assert_eq!(
            v.iter()
                .filter(|x| matches!(x, ReportViolation::SeverityBasisMismatch { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn summary_must_match_a_recomputation() {
        let mut r = report(vec![scanner("src/a.rs")], vec![]);
        r.summary.by_severity.high = 0;
        r.summary.carried_from_prior = 3;
        let v = only(&r);
        assert!(v.contains(&ReportViolation::SummaryMismatch {
            field: "by_severity"
        }));
        assert!(v.contains(&ReportViolation::SummaryMismatch {
            field: "carried_from_prior"
        }));
    }

    #[test]
    fn coverage_paths_unique_ids_present_and_shapes() {
        let m = |p: &str, status, ids: Vec<String>| ModuleCoverage {
            module_path: p.into(),
            status,
            reason: None,
            reviewed_paths: vec![],
            truncated_paths: vec![],
            candidate_ids: ids,
        };
        let cov = vec![
            m("a", ModuleCoverageStatus::Covered, vec![]),
            m("a", ModuleCoverageStatus::Covered, vec![]),
            m("b", ModuleCoverageStatus::Candidate, vec!["ghost".into()]),
            m("c", ModuleCoverageStatus::Candidate, vec![]),
            m("d", ModuleCoverageStatus::Deferred, vec![]),
            m("e", ModuleCoverageStatus::ParseFailed, vec![]),
        ];
        let v = only(&report(vec![], cov));
        assert!(v.contains(&ReportViolation::DuplicateCoverageModule {
            module_path: "a".into()
        }));
        assert!(v.contains(&ReportViolation::CoverageCandidateMissing {
            module_path: "b".into(),
            id: "ghost".into()
        }));
        let shapes = v
            .iter()
            .filter(|x| matches!(x, ReportViolation::CoverageStatusShape { .. }))
            .count();
        assert_eq!(shapes, 3, "{v:?}");
    }

    #[test]
    fn run_status_and_reason_must_agree() {
        let mut r = report(vec![], vec![]);
        r.run_status = RunStatus::Incomplete;
        assert_eq!(only(&r), vec![ReportViolation::IncompleteWithoutReason]);
        r.run_status = RunStatus::Complete;
        r.incomplete_reason = Some(IncompleteReason::EngineUnavailable);
        assert_eq!(only(&r), vec![ReportViolation::CompleteWithReason]);
        r.run_status = RunStatus::Incomplete;
        assert_eq!(only(&r), vec![]);
    }

    #[test]
    fn verifier_independence_must_match_agents_and_evidence() {
        let mut r = report(vec![], vec![]);
        r.verifier = VerifierInfo {
            independence: VerifierIndependence::DifferentAgent,
            audit_agent: Some("a".into()),
            verifier_agent: Some("a".into()),
        };
        assert_eq!(only(&r).len(), 1);
        r.verifier.independence = VerifierIndependence::SameAgent;
        r.verifier.verifier_agent = Some("b".into());
        assert_eq!(only(&r).len(), 1);
        r.verifier.verifier_agent = Some("a".into());
        assert_eq!(only(&r), vec![]);

        let mut nh = report(vec![needs_human("src/a.rs")], vec![]);
        nh.verifier.independence = VerifierIndependence::NotRun;
        assert_eq!(only(&nh).len(), 1);
    }

    #[test]
    fn verifier_correction_requires_a_passed_precheck() {
        let mut f = needs_human("src/a.rs");
        f.evidence
            .push(ev("adversarial", "verifier_corrected: line 10→14"));
        let id = f.id.clone();
        assert_eq!(
            only(&report(vec![f.clone()], vec![])),
            vec![ReportViolation::CorrectedWithoutPassedPrecheck { id: id.clone() }]
        );
        f.precheck = Some(crate::secaudit::schema::PrecheckResult {
            passed: false,
            violations: vec!["x".into()],
        });
        assert!(
            only(&report(vec![f.clone()], vec![]))
                .contains(&ReportViolation::CorrectedWithoutPassedPrecheck { id })
        );
        f.precheck = Some(crate::secaudit::schema::PrecheckResult {
            passed: true,
            violations: vec![],
        });
        assert_eq!(only(&report(vec![f], vec![])), vec![]);
    }

    #[test]
    fn violations_display_as_one_line_each() {
        let v = ReportViolation::SummaryMismatch { field: "coverage" };
        assert_eq!(
            v.to_string(),
            "summary.coverage does not match the findings/coverage"
        );
        assert!(
            !ReportViolation::IncompleteWithoutReason
                .to_string()
                .contains('\n')
        );
    }
}
