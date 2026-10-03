//! Deep-audit module coverage (DESIGN-llm-contract-secaudit-v2 §3.1).
//!
//! Every ranked module gets exactly one [`ModuleCoverage`] entry, in rank
//! order, including the ones never sent to the model: a report must show
//! how much was audited, not only what was found. Status table:
//!
//! | outcome | status |
//! |---|---|
//! | reviewed, zero findings | `covered` |
//! | reviewed, ≥1 finding (pre-check refutations included) | `candidate` |
//! | outside `--max-modules` | `deferred`, reason `max_modules` |
//! | run-wide candidate cap already reached | `deferred`, reason `candidate_cap` |
//! | not attempted after the engine was found unreachable | `deferred`, reason `engine_unavailable` |
//! | no readable text | `unreadable` |
//! | LLM call failed | `llm_failed` |
//! | reply violated the JSON contract | `parse_failed` |

use std::collections::HashMap;

use super::ai_audit::{ModuleAuditReport, ModuleOutcome, ModuleTarget};
use super::schema::{ModuleCoverage, ModuleCoverageStatus};

pub const DEFER_MAX_MODULES: &str = "max_modules";
pub const DEFER_CANDIDATE_CAP: &str = "candidate_cap";
pub const DEFER_ENGINE_UNAVAILABLE: &str = "engine_unavailable";

fn from_report(r: &ModuleAuditReport) -> ModuleCoverage {
    let (status, reason) = match &r.outcome {
        ModuleOutcome::Reviewed { note } => {
            if r.candidate_ids.is_empty() {
                (ModuleCoverageStatus::Covered, note.clone())
            } else {
                (ModuleCoverageStatus::Candidate, note.clone())
            }
        }
        ModuleOutcome::Unreadable { reason } => {
            (ModuleCoverageStatus::Unreadable, Some(reason.clone()))
        }
        ModuleOutcome::LlmFailed { reason } => {
            (ModuleCoverageStatus::LlmFailed, Some(reason.clone()))
        }
        ModuleOutcome::ParseFailed { reason } => {
            (ModuleCoverageStatus::ParseFailed, Some(reason.clone()))
        }
        ModuleOutcome::Deferred { reason } => {
            (ModuleCoverageStatus::Deferred, Some(reason.clone()))
        }
    };
    ModuleCoverage {
        module_path: r.module_path.clone(),
        status,
        reason,
        reviewed_paths: r.reviewed_paths.clone(),
        truncated_paths: r.truncated_paths.clone(),
        candidate_ids: r.candidate_ids.clone(),
    }
}

/// Build the coverage list from ALL ranked modules plus the per-module audit
/// reports. A ranked module with no report was never handed to the audit
/// step (outside `--max-modules`) and is `deferred` / `max_modules`.
pub fn build_module_coverage(
    ranked: &[ModuleTarget],
    reports: &[ModuleAuditReport],
) -> Vec<ModuleCoverage> {
    let by_path: HashMap<&str, &ModuleAuditReport> = reports
        .iter()
        .map(|r| (r.module_path.as_str(), r))
        .collect();
    ranked
        .iter()
        .map(|m| match by_path.get(m.module_path.as_str()) {
            Some(r) => from_report(r),
            None => ModuleCoverage {
                module_path: m.module_path.clone(),
                status: ModuleCoverageStatus::Deferred,
                reason: Some(DEFER_MAX_MODULES.to_string()),
                reviewed_paths: Vec::new(),
                truncated_paths: Vec::new(),
                candidate_ids: Vec::new(),
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(p: &str) -> ModuleTarget {
        ModuleTarget {
            module_path: p.to_string(),
            score: 1,
            files: vec![format!("{p}/x.rs")],
        }
    }

    fn report(p: &str, outcome: ModuleOutcome, ids: &[&str]) -> ModuleAuditReport {
        ModuleAuditReport {
            module_path: p.to_string(),
            outcome,
            reviewed_paths: vec![format!("{p}/x.rs")],
            truncated_paths: vec![],
            candidate_ids: ids.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn every_status_maps_and_unreported_modules_are_deferred_max_modules() {
        let ranked: Vec<ModuleTarget> = ["a", "b", "c", "d", "e", "f", "g", "h"]
            .iter()
            .map(|p| target(p))
            .collect();
        let reports = vec![
            report("a", ModuleOutcome::Reviewed { note: None }, &[]),
            report("b", ModuleOutcome::Reviewed { note: None }, &["id1"]),
            report(
                "c",
                ModuleOutcome::Unreadable {
                    reason: "bin".into(),
                },
                &[],
            ),
            report("d", ModuleOutcome::LlmFailed { reason: "t".into() }, &[]),
            report("e", ModuleOutcome::ParseFailed { reason: "p".into() }, &[]),
            report(
                "f",
                ModuleOutcome::Deferred {
                    reason: DEFER_CANDIDATE_CAP.into(),
                },
                &[],
            ),
            report(
                "g",
                ModuleOutcome::Deferred {
                    reason: DEFER_ENGINE_UNAVAILABLE.into(),
                },
                &[],
            ),
        ];
        let cov = build_module_coverage(&ranked, &reports);
        let statuses: Vec<ModuleCoverageStatus> = cov.iter().map(|c| c.status).collect();
        assert_eq!(
            statuses,
            vec![
                ModuleCoverageStatus::Covered,
                ModuleCoverageStatus::Candidate,
                ModuleCoverageStatus::Unreadable,
                ModuleCoverageStatus::LlmFailed,
                ModuleCoverageStatus::ParseFailed,
                ModuleCoverageStatus::Deferred,
                ModuleCoverageStatus::Deferred,
                ModuleCoverageStatus::Deferred,
            ]
        );
        assert_eq!(cov[5].reason.as_deref(), Some(DEFER_CANDIDATE_CAP));
        assert_eq!(cov[6].reason.as_deref(), Some(DEFER_ENGINE_UNAVAILABLE));
        assert_eq!(cov[7].reason.as_deref(), Some(DEFER_MAX_MODULES));
        assert!(cov[7].reviewed_paths.is_empty());
        assert_eq!(cov[1].candidate_ids, vec!["id1".to_string()]);
        assert!(cov[0].reason.is_none());
    }

    #[test]
    fn empty_ranking_is_empty_coverage() {
        assert!(build_module_coverage(&[], &[]).is_empty());
    }
}
