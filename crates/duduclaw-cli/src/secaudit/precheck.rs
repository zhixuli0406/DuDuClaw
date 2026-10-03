//! Deterministic pre-check of every ai_audit candidate
//! (DESIGN-llm-contract-secaudit-v2 §3.4 item 3).
//!
//! Runs before any verifier call and costs zero LLM. Checks, in order:
//! (a) `file` is a [`SafeRepoPath`] and is one of the paths this module's
//!     audit prompt actually contained;
//! (b) `line`, when given, is within `1..=line_count(file)`;
//! (c) every `trace` step names a safe path that exists under the repo with
//!     the line in range, and the step kinds have the required shape (first
//!     `entrypoint`, last `sink`, middle `propagation`; a single step is
//!     `entrypoint` or `sink`; an empty trace fails);
//! (d) all six `threat_model` slots carry visible text;
//! (e) `conditions.kind` is one of the nine kinds — already enforced by
//!     serde (`ConditionKind` is closed, `deny_unknown_fields`), so a reply
//!     with an unknown kind never reaches this module.
//!
//! A failing candidate becomes `Refuted` with `precheck.passed = false` and
//! one `AdversarialReview` evidence item `precheck: <violations>`, and is not
//! sent to the verifier.
//!
//! Everything here is pure except [`DiskLines`], the filesystem-backed
//! [`LineOracle`]; tests use an in-memory oracle.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;

use duduclaw_core::llm_contract::safe_path::SafeRepoPath;
use duduclaw_core::llm_contract::visible_text::has_visible_content;

use super::schema::{
    EvidenceItem, EvidenceKind, Finding, FindingStatus, PRECHECK_PREFIX, PrecheckResult,
    ThreatModel, TraceKind, TraceStep,
};

/// Cap on the joined violation text stored in evidence.
const EVIDENCE_MAX_BYTES: usize = 2000;

/// Line counts of repo files. `None` = the file does not exist or cannot be
/// read.
pub trait LineOracle {
    fn line_count(&self, path: &SafeRepoPath) -> Option<usize>;
}

/// [`LineOracle`] over the real repository, cached per path. Reads only
/// through [`SafeRepoPath::join_under`].
pub struct DiskLines<'a> {
    repo_root: &'a Path,
    cache: Mutex<HashMap<String, Option<usize>>>,
}

impl<'a> DiskLines<'a> {
    pub fn new(repo_root: &'a Path) -> Self {
        DiskLines {
            repo_root,
            cache: Mutex::new(HashMap::new()),
        }
    }
}

impl LineOracle for DiskLines<'_> {
    fn line_count(&self, path: &SafeRepoPath) -> Option<usize> {
        // A poisoned lock only means another reader panicked mid-insert;
        // the map is still a valid cache.
        let cached = self
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(path.as_str())
            .copied();
        if let Some(hit) = cached {
            return hit;
        }
        let full = path.join_under(self.repo_root);
        let count = if full.is_file() {
            std::fs::read(&full)
                .ok()
                .map(|bytes| String::from_utf8_lossy(&bytes).lines().count())
        } else {
            None
        };
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(path.as_str().to_string(), count);
        count
    }
}

fn check_line(label: &str, line: u32, count: usize, out: &mut Vec<String>) {
    if line == 0 || line as usize > count {
        out.push(format!("{label}: line {line} outside 1..={count}"));
    }
}

/// (a) + (b): the claimed location.
pub fn check_location(
    file: &str,
    line: Option<u32>,
    prompt_paths: &HashSet<String>,
    oracle: &impl LineOracle,
) -> Vec<String> {
    let mut out = Vec::new();
    let safe = match SafeRepoPath::parse(file) {
        Ok(p) => p,
        Err(e) => {
            out.push(format!("file: unsafe path ({e})"));
            return out;
        }
    };
    if !prompt_paths.contains(safe.as_str()) {
        out.push("file: not one of the paths in this module's audit prompt".to_string());
    }
    out.extend(check_line_in_file(&safe, line, oracle));
    out
}

/// (a) without the module-path-set rule + (b): used for a verifier's
/// corrected location, which may legitimately be any file under the repo.
pub fn check_file_and_line(file: &str, line: Option<u32>, oracle: &impl LineOracle) -> Vec<String> {
    match SafeRepoPath::parse(file) {
        Ok(safe) => check_line_in_file(&safe, line, oracle),
        Err(e) => vec![format!("file: unsafe path ({e})")],
    }
}

fn check_line_in_file(
    safe: &SafeRepoPath,
    line: Option<u32>,
    oracle: &impl LineOracle,
) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(l) = line {
        match oracle.line_count(safe) {
            Some(count) => check_line("file", l, count, &mut out),
            None => out.push("file: does not exist under the repository".to_string()),
        }
    }
    out
}

/// (c): trace shape and every step's location.
pub fn check_trace(trace: &[TraceStep], oracle: &impl LineOracle) -> Vec<String> {
    let mut out = Vec::new();
    if trace.is_empty() {
        out.push("trace: empty".to_string());
        return out;
    }
    out.extend(trace_shape_violations(trace));
    for (i, step) in trace.iter().enumerate() {
        let label = format!("trace[{i}]");
        match SafeRepoPath::parse(&step.file) {
            Err(e) => out.push(format!("{label}.file: unsafe path ({e})")),
            Ok(safe) => match oracle.line_count(&safe) {
                None => out.push(format!("{label}.file: does not exist under the repository")),
                Some(count) => check_line(&format!("{label}.line"), step.line, count, &mut out),
            },
        }
    }
    out
}

/// Kind order only (shared with the report validator). An empty trace has
/// no shape violation here; callers that require a trace check emptiness.
pub fn trace_shape_violations(trace: &[TraceStep]) -> Vec<String> {
    let mut out = Vec::new();
    let n = trace.len();
    if n == 1 {
        if trace[0].kind == TraceKind::Propagation {
            out.push("trace[0].kind: a single step must be entrypoint or sink".to_string());
        }
        return out;
    }
    for (i, step) in trace.iter().enumerate() {
        let expected = if i == 0 {
            TraceKind::Entrypoint
        } else if i + 1 == n {
            TraceKind::Sink
        } else {
            TraceKind::Propagation
        };
        if step.kind != expected {
            out.push(
                format!(
                    "trace[{i}].kind: expected {expected:?}, got {:?}",
                    step.kind
                )
                .to_lowercase(),
            );
        }
    }
    out
}

/// (d): all six slots visible.
pub fn check_threat_model(tm: &ThreatModel) -> Vec<String> {
    [
        ("principal", &tm.principal),
        ("input", &tm.input),
        ("control", &tm.control),
        ("boundary", &tm.boundary),
        ("affected", &tm.affected),
        ("result", &tm.result),
    ]
    .into_iter()
    .filter(|(_, v)| !has_visible_content(v))
    .map(|(slot, _)| format!("threat_model.{slot}: no visible content"))
    .collect()
}

/// All checks, in the order of the module doc.
pub fn check_candidate(
    file: &str,
    line: Option<u32>,
    trace: &[TraceStep],
    threat_model: &ThreatModel,
    prompt_paths: &HashSet<String>,
    oracle: &impl LineOracle,
) -> Vec<String> {
    let mut out = check_location(file, line, prompt_paths, oracle);
    out.extend(check_trace(trace, oracle));
    out.extend(check_threat_model(threat_model));
    out
}

/// Record the pre-check outcome on `finding`. Any violation ⇒ `Refuted`
/// with a `precheck:` evidence item; none ⇒ `precheck.passed = true` and the
/// status is left alone.
pub fn apply_precheck(finding: &mut Finding, violations: Vec<String>) {
    if violations.is_empty() {
        finding.precheck = Some(PrecheckResult {
            passed: true,
            violations,
        });
        return;
    }
    let joined = violations.join("; ");
    finding.status = FindingStatus::Refuted;
    finding.evidence.push(EvidenceItem {
        kind: EvidenceKind::AdversarialReview,
        source: "precheck".to_string(),
        detail: format!(
            "{PRECHECK_PREFIX} {}",
            duduclaw_core::truncate_bytes(&joined, EVIDENCE_MAX_BYTES)
        ),
        recorded_at: chrono::Utc::now().to_rfc3339(),
    });
    finding.precheck = Some(PrecheckResult {
        passed: false,
        violations,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secaudit::schema::{FindingKind, Severity};

    struct MemLines(HashMap<&'static str, usize>);
    impl LineOracle for MemLines {
        fn line_count(&self, path: &SafeRepoPath) -> Option<usize> {
            self.0.get(path.as_str()).copied()
        }
    }

    fn oracle() -> MemLines {
        MemLines(HashMap::from([("src/a.rs", 50), ("src/b.rs", 10)]))
    }

    fn prompt_paths() -> HashSet<String> {
        HashSet::from(["src/a.rs".to_string()])
    }

    fn step(kind: TraceKind, file: &str, line: u32) -> TraceStep {
        TraceStep {
            kind,
            file: file.to_string(),
            line,
            scope: "handler".to_string(),
            description: "d".to_string(),
        }
    }

    fn tm() -> ThreatModel {
        ThreatModel {
            principal: "anonymous client".into(),
            input: "query string".into(),
            control: "auth middleware".into(),
            boundary: "network → app".into(),
            affected: "other tenants".into(),
            result: "read other tenants' rows".into(),
        }
    }

    fn good_trace() -> Vec<TraceStep> {
        vec![
            step(TraceKind::Entrypoint, "src/a.rs", 3),
            step(TraceKind::Propagation, "src/b.rs", 5),
            step(TraceKind::Sink, "src/a.rs", 40),
        ]
    }

    #[test]
    fn a_fully_valid_candidate_passes() {
        let v = check_candidate(
            "src/a.rs",
            Some(10),
            &good_trace(),
            &tm(),
            &prompt_paths(),
            &oracle(),
        );
        assert!(v.is_empty(), "{v:?}");
    }

    // (a)
    #[test]
    fn unsafe_file_path_fails() {
        for bad in ["../../etc/passwd", "/etc/passwd", "C:\\x", "a//b", "./a"] {
            let v = check_location(bad, None, &prompt_paths(), &oracle());
            assert!(v[0].starts_with("file: unsafe path"), "{bad}: {v:?}");
        }
    }

    #[test]
    fn file_outside_the_module_prompt_fails() {
        let v = check_location("src/b.rs", Some(1), &prompt_paths(), &oracle());
        assert!(
            v.iter().any(|s| s.contains("not one of the paths")),
            "{v:?}"
        );
    }

    // (b)
    #[test]
    fn line_out_of_range_fails_and_in_range_passes() {
        assert!(check_location("src/a.rs", Some(50), &prompt_paths(), &oracle()).is_empty());
        assert!(check_location("src/a.rs", None, &prompt_paths(), &oracle()).is_empty());
        let v = check_location("src/a.rs", Some(51), &prompt_paths(), &oracle());
        assert!(v[0].contains("outside 1..=50"), "{v:?}");
        let v = check_location("src/a.rs", Some(0), &prompt_paths(), &oracle());
        assert!(!v.is_empty());
    }

    #[test]
    fn check_file_and_line_skips_the_prompt_path_rule() {
        // src/b.rs is not in the prompt set but exists: fine for a correction.
        assert!(check_file_and_line("src/b.rs", Some(10), &oracle()).is_empty());
        assert!(!check_file_and_line("src/b.rs", Some(11), &oracle()).is_empty());
        assert!(!check_file_and_line("/etc/passwd", Some(1), &oracle()).is_empty());
        assert!(!check_file_and_line("src/none.rs", Some(1), &oracle()).is_empty());
    }

    // (c)
    #[test]
    fn empty_trace_fails() {
        assert_eq!(
            check_trace(&[], &oracle()),
            vec!["trace: empty".to_string()]
        );
    }

    #[test]
    fn single_step_trace_must_be_entrypoint_or_sink() {
        assert!(check_trace(&[step(TraceKind::Sink, "src/a.rs", 1)], &oracle()).is_empty());
        assert!(check_trace(&[step(TraceKind::Entrypoint, "src/a.rs", 1)], &oracle()).is_empty());
        let v = check_trace(&[step(TraceKind::Propagation, "src/a.rs", 1)], &oracle());
        assert!(v[0].contains("single step"), "{v:?}");
    }

    #[test]
    fn multi_step_trace_kind_order_is_enforced() {
        let bad = vec![
            step(TraceKind::Sink, "src/a.rs", 1),
            step(TraceKind::Sink, "src/a.rs", 2),
            step(TraceKind::Entrypoint, "src/a.rs", 3),
        ];
        let v = check_trace(&bad, &oracle());
        assert_eq!(v.len(), 3, "{v:?}");
        assert!(check_trace(&good_trace(), &oracle()).is_empty());
    }

    #[test]
    fn trace_step_with_unsafe_missing_or_out_of_range_location_fails() {
        let t = vec![
            step(TraceKind::Entrypoint, "/etc/passwd", 1),
            step(TraceKind::Propagation, "src/missing.rs", 1),
            step(TraceKind::Sink, "src/b.rs", 11),
        ];
        let v = check_trace(&t, &oracle());
        assert!(
            v.iter()
                .any(|s| s.starts_with("trace[0].file: unsafe path")),
            "{v:?}"
        );
        assert!(
            v.iter()
                .any(|s| s.starts_with("trace[1].file: does not exist")),
            "{v:?}"
        );
        assert!(v.iter().any(|s| s.starts_with("trace[2].line")), "{v:?}");
    }

    // (d)
    #[test]
    fn blank_or_invisible_threat_model_slots_fail() {
        assert!(check_threat_model(&tm()).is_empty());
        let mut t = tm();
        t.control = "   ".into();
        t.result = "\u{200b}\u{feff}".into();
        let v = check_threat_model(&t);
        assert_eq!(
            v,
            vec![
                "threat_model.control: no visible content".to_string(),
                "threat_model.result: no visible content".to_string()
            ]
        );
    }

    #[test]
    fn apply_precheck_refutes_with_evidence_or_marks_passed() {
        let base = Finding::candidate(
            "ai_audit",
            FindingKind::Other,
            Severity::High,
            "t",
            "src/a.rs",
            Some(1),
            "s",
            "ai-audit/x",
            vec![],
        );
        let mut ok = base.clone();
        apply_precheck(&mut ok, vec![]);
        assert_eq!(ok.status, FindingStatus::Candidate);
        assert!(ok.precheck.as_ref().unwrap().passed);
        assert!(ok.evidence.is_empty());

        let mut bad = base;
        apply_precheck(&mut bad, vec!["trace: empty".into(), "file: x".into()]);
        assert_eq!(bad.status, FindingStatus::Refuted);
        assert!(!bad.precheck.as_ref().unwrap().passed);
        let ev = bad.evidence.last().unwrap();
        assert_eq!(ev.kind, EvidenceKind::AdversarialReview);
        assert_eq!(ev.detail, "precheck: trace: empty; file: x");
    }

    #[test]
    fn disk_lines_counts_real_files_and_reports_missing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "a\nb\nc\n").unwrap();
        let o = DiskLines::new(dir.path());
        assert_eq!(
            o.line_count(&SafeRepoPath::parse("src/a.rs").unwrap()),
            Some(3)
        );
        assert_eq!(
            o.line_count(&SafeRepoPath::parse("src/none.rs").unwrap()),
            None
        );
        assert_eq!(o.line_count(&SafeRepoPath::parse("src").unwrap()), None);
    }
}
