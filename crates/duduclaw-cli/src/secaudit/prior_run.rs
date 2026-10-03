//! Carry-over from a prior report (DESIGN-llm-contract-secaudit-v2 §3.4
//! item 8). On by default; `--no-prior` turns it off.
//!
//! The prior report is the newest file under `<home>/secaudit/reports/`
//! whose `repo` equals the current canonical repo path and whose
//! `schema_version >= 2`. Unparsable files and older-schema reports for the
//! same repo are skipped with one stderr line each; the search continues
//! with the next-newest file.
//!
//! Matching is by `root_fingerprint` (semantic identity) AND `file_hash`
//! (the file bytes are unchanged). Rules, applied to every current
//! `Candidate` finding (scanner and ai_audit alike) before adversarial
//! review:
//! - prior `Suppressed`, same hash ⇒ `Suppressed`, evidence
//!   `carried_from_prior: <file>`, zero LLM;
//! - prior `Refuted`, same hash ⇒ `Refuted`, same evidence, and therefore
//!   no verifier call;
//! - prior `Confirmed`, same hash ⇒ stays `Candidate` (re-verified on the
//!   current path), evidence `prior_confirmed_same_source`, counted in
//!   `revalidated_prior_confirmed`;
//! - a fingerprint match whose hash differs (or either hash is missing) ⇒
//!   not carried, counted in `changed_source`.
//!
//! When several prior findings share the fingerprint and the hash, a prior
//! `Confirmed` (or any still-open status) wins over suppression: a prior
//! record may raise attention, never hide something that was not
//! uniformly dismissed. `Suppressed` is carried only when every same-hash
//! match is `Suppressed`; `Refuted` when every match is `Refuted` or
//! `Suppressed`.

use std::collections::HashMap;
use std::path::Path;

use sha2::{Digest, Sha256};

use duduclaw_core::llm_contract::safe_path::SafeRepoPath;

use super::schema::{
    AuditReport, CARRIED_FROM_PRIOR_PREFIX, CURRENT_SCHEMA_VERSION, EvidenceItem, EvidenceKind,
    Finding, FindingStatus, PRIOR_CONFIRMED_PREFIX, PriorRunInfo,
};

/// Reports bigger than this are not read (a corrupted or hostile file must
/// not exhaust memory).
const MAX_REPORT_BYTES: u64 = 64 * 1024 * 1024;
/// Files bigger than this are not hashed (`file_hash = None`).
const MAX_HASH_BYTES: u64 = 256 * 1024 * 1024;

/// One prior finding, reduced to what the carry rules need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorEntry {
    pub status: FindingStatus,
    pub file_hash: Option<String>,
}

/// The prior report, indexed by root fingerprint.
#[derive(Debug, Clone, Default)]
pub struct PriorIndex {
    pub report_file: String,
    pub by_fingerprint: HashMap<String, Vec<PriorEntry>>,
}

impl PriorIndex {
    pub fn from_report(report_file: &str, report: &AuditReport) -> Self {
        let mut by_fingerprint: HashMap<String, Vec<PriorEntry>> = HashMap::new();
        for f in &report.findings {
            if f.root_fingerprint.is_empty() {
                continue;
            }
            by_fingerprint
                .entry(f.root_fingerprint.clone())
                .or_default()
                .push(PriorEntry {
                    status: f.status,
                    file_hash: f.file_hash.clone(),
                });
        }
        PriorIndex {
            report_file: report_file.to_string(),
            by_fingerprint,
        }
    }
}

/// Find and load the newest qualifying prior report. `None` when there is
/// none (no directory, no report for this repo, or only skipped files).
pub fn load_prior(home_dir: &Path, canonical_repo: &str) -> Option<PriorIndex> {
    let dir = home_dir.join("secaudit").join("reports");
    let entries = std::fs::read_dir(&dir).ok()?;
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| n.ends_with(".json"))
        .collect();
    // `<UTC-ISO8601-basic>.json` sorts lexically = chronologically.
    names.sort();
    names.reverse();
    for name in names {
        let path = dir.join(&name);
        let too_big = std::fs::metadata(&path)
            .map(|m| m.len() > MAX_REPORT_BYTES)
            .unwrap_or(true);
        if too_big {
            eprintln!("[secaudit] 略過先前報告 {name}：檔案無法讀取或過大");
            continue;
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[secaudit] 略過先前報告 {name}：無法讀取（{e}）");
                continue;
            }
        };
        let report: AuditReport = match serde_json::from_str(&raw) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[secaudit] 略過先前報告 {name}：格式無法解析（{e}）");
                continue;
            }
        };
        if report.repo != canonical_repo {
            continue;
        }
        if report.schema_version < CURRENT_SCHEMA_VERSION {
            eprintln!(
                "[secaudit] 略過先前報告 {name}：舊版格式（schema_version {}），沒有可比對的根因指紋",
                report.schema_version
            );
            continue;
        }
        return Some(PriorIndex::from_report(&name, &report));
    }
    None
}

/// What the carry rules decided for one finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarryDecision {
    None,
    Suppressed,
    Refuted,
    RevalidateConfirmed,
    ChangedSource,
}

/// Pure decision for one finding against the prior entries sharing its
/// fingerprint. See the module doc for the rules.
pub fn decide(current_hash: Option<&str>, prior: &[PriorEntry]) -> CarryDecision {
    if prior.is_empty() {
        return CarryDecision::None;
    }
    let same: Vec<&PriorEntry> = match current_hash {
        Some(h) => prior
            .iter()
            .filter(|p| p.file_hash.as_deref() == Some(h))
            .collect(),
        None => Vec::new(),
    };
    if same.is_empty() {
        return CarryDecision::ChangedSource;
    }
    if same.iter().any(|p| p.status == FindingStatus::Confirmed) {
        return CarryDecision::RevalidateConfirmed;
    }
    if same.iter().all(|p| p.status == FindingStatus::Suppressed) {
        return CarryDecision::Suppressed;
    }
    if same
        .iter()
        .all(|p| matches!(p.status, FindingStatus::Refuted | FindingStatus::Suppressed))
    {
        return CarryDecision::Refuted;
    }
    CarryDecision::None
}

fn push_prior_evidence(f: &mut Finding, detail: String) {
    f.evidence.push(EvidenceItem {
        kind: EvidenceKind::AdversarialReview,
        source: "prior_run".to_string(),
        detail,
        recorded_at: chrono::Utc::now().to_rfc3339(),
    });
}

/// Apply the carry rules to every current `Candidate` finding.
pub fn apply_prior(findings: &mut [Finding], prior: &PriorIndex) -> PriorRunInfo {
    let mut info = PriorRunInfo {
        report_file: prior.report_file.clone(),
        ..PriorRunInfo::default()
    };
    for f in findings.iter_mut() {
        if f.status != FindingStatus::Candidate || f.root_fingerprint.is_empty() {
            continue;
        }
        let Some(entries) = prior.by_fingerprint.get(&f.root_fingerprint) else {
            continue;
        };
        match decide(f.file_hash.as_deref(), entries) {
            CarryDecision::None => {}
            CarryDecision::ChangedSource => info.changed_source += 1,
            CarryDecision::Suppressed => {
                f.status = FindingStatus::Suppressed;
                push_prior_evidence(
                    f,
                    format!(
                        "{CARRIED_FROM_PRIOR_PREFIX} {} (prior status suppressed, source unchanged)",
                        prior.report_file
                    ),
                );
                info.carried_suppressed += 1;
            }
            CarryDecision::Refuted => {
                f.status = FindingStatus::Refuted;
                push_prior_evidence(
                    f,
                    format!(
                        "{CARRIED_FROM_PRIOR_PREFIX} {} (prior status refuted, source unchanged)",
                        prior.report_file
                    ),
                );
                info.carried_refuted += 1;
            }
            CarryDecision::RevalidateConfirmed => {
                push_prior_evidence(
                    f,
                    format!(
                        "{PRIOR_CONFIRMED_PREFIX}: {} confirmed this on identical source; re-verifying",
                        prior.report_file
                    ),
                );
                info.revalidated_prior_confirmed += 1;
            }
        }
    }
    info
}

/// sha256 hex of a file's bytes. `None` when the path is not a safe repo
/// path, is not a regular file, is too big, or cannot be read.
pub fn hash_repo_file(repo_root: &Path, file: &str) -> Option<String> {
    let safe = SafeRepoPath::parse(file).ok()?;
    let full = safe.join_under(repo_root);
    let meta = std::fs::metadata(&full).ok()?;
    if !meta.is_file() || meta.len() > MAX_HASH_BYTES {
        return None;
    }
    let bytes = std::fs::read(&full).ok()?;
    Some(hex::encode(Sha256::digest(&bytes)))
}

/// Fill `file_hash` on every finding, hashing each distinct file once.
pub fn fill_file_hashes(repo_root: &Path, findings: &mut [Finding]) {
    let mut cache: HashMap<String, Option<String>> = HashMap::new();
    for f in findings.iter_mut() {
        let h = cache
            .entry(f.file.clone())
            .or_insert_with(|| hash_repo_file(repo_root, &f.file))
            .clone();
        f.file_hash = h;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secaudit::schema::{
        FindingKind, GatePolicy, ProfileMode, ScanProfile, Severity, Summary, VerifierInfo,
    };
    use duduclaw_core::llm_contract::coverage::RunStatus;

    fn finding(file: &str, rule: &str, status: FindingStatus, hash: Option<&str>) -> Finding {
        let mut f = Finding::candidate(
            "semgrep",
            FindingKind::StaticAnalysis,
            Severity::High,
            "t",
            file,
            Some(1),
            "s",
            rule,
            vec![],
        );
        f.status = status;
        f.file_hash = hash.map(str::to_string);
        f
    }

    fn report(repo: &str, version: u32, findings: Vec<Finding>) -> AuditReport {
        AuditReport {
            schema_version: version,
            repo: repo.to_string(),
            started_at: "2026-10-01T00:00:00Z".into(),
            profile: ScanProfile {
                mode: ProfileMode::Quick,
                intake: None,
            },
            engines_run: vec![],
            engines_missing: vec![],
            summary: Summary::from_findings(&findings, 0, 0, GatePolicy::default(), &[]),
            findings,
            run_status: RunStatus::Complete,
            incomplete_reason: None,
            coverage: vec![],
            prior_run: None,
            verifier: VerifierInfo::default(),
        }
    }

    fn write(home: &Path, name: &str, r: &AuditReport) {
        let dir = home.join("secaudit").join("reports");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), serde_json::to_string(r).unwrap()).unwrap();
    }

    // ── decide ────────────────────────────────────────────────────────

    fn e(status: FindingStatus, h: &str) -> PriorEntry {
        PriorEntry {
            status,
            file_hash: Some(h.into()),
        }
    }

    #[test]
    fn decide_follows_the_carry_rules() {
        use FindingStatus::*;
        assert_eq!(decide(Some("h"), &[]), CarryDecision::None);
        assert_eq!(
            decide(Some("h"), &[e(Suppressed, "h")]),
            CarryDecision::Suppressed
        );
        assert_eq!(
            decide(Some("h"), &[e(Refuted, "h")]),
            CarryDecision::Refuted
        );
        assert_eq!(
            decide(Some("h"), &[e(Confirmed, "h")]),
            CarryDecision::RevalidateConfirmed
        );
        assert_eq!(
            decide(Some("h"), &[e(Refuted, "old")]),
            CarryDecision::ChangedSource
        );
        assert_eq!(
            decide(None, &[e(Refuted, "h")]),
            CarryDecision::ChangedSource
        );
        // Mixed: a Confirmed wins; an open status blocks suppression.
        assert_eq!(
            decide(Some("h"), &[e(Suppressed, "h"), e(Confirmed, "h")]),
            CarryDecision::RevalidateConfirmed
        );
        assert_eq!(
            decide(Some("h"), &[e(Refuted, "h"), e(NeedsHuman, "h")]),
            CarryDecision::None
        );
        assert_eq!(
            decide(Some("h"), &[e(Refuted, "h"), e(Suppressed, "h")]),
            CarryDecision::Refuted
        );
    }

    // ── load_prior + apply_prior ─────────────────────────────────────

    #[test]
    fn carry_rules_end_to_end_from_a_saved_report() {
        let home = tempfile::tempdir().unwrap();
        let prior = report(
            "/repo",
            2,
            vec![
                finding("a.rs", "r-sup", FindingStatus::Suppressed, Some("h1")),
                finding("b.rs", "r-ref", FindingStatus::Refuted, Some("h2")),
                finding("c.rs", "r-conf", FindingStatus::Confirmed, Some("h3")),
                finding("d.rs", "r-chg", FindingStatus::Refuted, Some("old")),
            ],
        );
        write(home.path(), "20261001T000000Z.json", &prior);

        let idx = load_prior(home.path(), "/repo").expect("prior found");
        assert_eq!(idx.report_file, "20261001T000000Z.json");

        // Current findings at different lines (fingerprint ignores line).
        let mut current = vec![
            finding("a.rs", "r-sup", FindingStatus::Candidate, Some("h1")),
            finding("b.rs", "r-ref", FindingStatus::Candidate, Some("h2")),
            finding("c.rs", "r-conf", FindingStatus::Candidate, Some("h3")),
            finding("d.rs", "r-chg", FindingStatus::Candidate, Some("new")),
            finding("e.rs", "r-new", FindingStatus::Candidate, Some("h5")),
        ];
        current[0].line = Some(77);
        let info = apply_prior(&mut current, &idx);
        assert_eq!(current[0].status, FindingStatus::Suppressed);
        assert_eq!(current[1].status, FindingStatus::Refuted);
        assert_eq!(current[2].status, FindingStatus::Candidate);
        assert_eq!(current[3].status, FindingStatus::Candidate);
        assert_eq!(current[4].status, FindingStatus::Candidate);
        assert!(
            current[0].evidence[0]
                .detail
                .starts_with(CARRIED_FROM_PRIOR_PREFIX)
        );
        assert_eq!(current[0].evidence[0].kind, EvidenceKind::AdversarialReview);
        assert!(
            current[2].evidence[0]
                .detail
                .starts_with(PRIOR_CONFIRMED_PREFIX)
        );
        assert!(current[3].evidence.is_empty());
        assert_eq!(
            info,
            PriorRunInfo {
                report_file: "20261001T000000Z.json".into(),
                carried_suppressed: 1,
                carried_refuted: 1,
                revalidated_prior_confirmed: 1,
                changed_source: 1,
            }
        );
    }

    #[test]
    fn load_prior_skips_other_repos_old_schema_and_broken_files() {
        let home = tempfile::tempdir().unwrap();
        // Newest first: broken, other repo, v1 same repo, then the good one.
        write(
            home.path(),
            "20261001T000000Z.json",
            &report(
                "/repo",
                2,
                vec![finding("a.rs", "r", FindingStatus::Refuted, Some("h"))],
            ),
        );
        write(
            home.path(),
            "20261002T000000Z.json",
            &report("/repo", 1, vec![]),
        );
        write(
            home.path(),
            "20261003T000000Z.json",
            &report("/other", 2, vec![]),
        );
        let dir = home.path().join("secaudit").join("reports");
        std::fs::write(dir.join("20261004T000000Z.json"), "{ not json").unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();

        let idx = load_prior(home.path(), "/repo").expect("falls through to the v2 report");
        assert_eq!(idx.report_file, "20261001T000000Z.json");
        assert_eq!(idx.by_fingerprint.len(), 1);
    }

    /// The real 2026-08 v1 file shape (no schema_version) is skipped
    /// gracefully: no prior, no panic.
    #[test]
    fn load_prior_skips_a_v1_report_without_schema_version() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("secaudit").join("reports");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("20260817T222007Z.json"),
            r#"{"repo":"/repo","started_at":"t","profile":{"mode":"quick","intake":null},
                "engines_run":[],"engines_missing":[],"findings":[],
                "summary":{"total_findings":0,"by_severity":{"critical":0,"high":0,"medium":0,"low":0,"info":0},
                "engines_run_count":0,"engines_missing_count":0}}"#,
        )
        .unwrap();
        assert!(load_prior(home.path(), "/repo").is_none());
    }

    #[test]
    fn load_prior_without_a_reports_dir_is_none() {
        let home = tempfile::tempdir().unwrap();
        assert!(load_prior(home.path(), "/repo").is_none());
    }

    #[test]
    fn apply_prior_leaves_settled_findings_alone() {
        let prior = PriorIndex::from_report(
            "p.json",
            &report(
                "/repo",
                2,
                vec![finding("a.rs", "r", FindingStatus::Suppressed, Some("h"))],
            ),
        );
        let mut current = vec![finding("a.rs", "r", FindingStatus::Refuted, Some("h"))];
        let info = apply_prior(&mut current, &prior);
        assert_eq!(current[0].status, FindingStatus::Refuted);
        assert!(current[0].evidence.is_empty());
        assert_eq!(info.carried_suppressed, 0);
    }

    // ── hashing ───────────────────────────────────────────────────────

    #[test]
    fn fill_file_hashes_hashes_real_files_and_refuses_unsafe_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "abc").unwrap();
        let mut fs = vec![
            finding("a.rs", "r1", FindingStatus::Candidate, None),
            finding("a.rs", "r2", FindingStatus::Candidate, None),
            finding("/etc/passwd", "r3", FindingStatus::Candidate, None),
            finding("missing.rs", "r4", FindingStatus::Candidate, None),
        ];
        fill_file_hashes(dir.path(), &mut fs);
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(fs[0].file_hash.as_deref(), Some(abc));
        assert_eq!(fs[1].file_hash.as_deref(), Some(abc));
        assert!(fs[2].file_hash.is_none());
        assert!(fs[3].file_hash.is_none());
    }
}
