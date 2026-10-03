//! Coverage ledger: a closed state table that makes "what was looked at, by
//! whom, with what result" checkable.
//!
//! Each [`CoverageUnit`] is one thing that must be accounted for (a module, a
//! rule × technique × locale cell, an acceptance criterion). Its
//! [`UnitStatus`] decides which fields must be empty or filled, so a model
//! cannot claim `covered` without naming the paths and checks behind it.
//! Mirrors Cloudflare security-audit-skill (MIT) `validate-coverage-ledger.cjs`
//! (`validateStateInvariants`, `validateReviewedPathOwnership`,
//! `validateChecks`, and the document-level id uniqueness/sort checks).

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::fingerprint::Fingerprint;
use super::safe_path::SafeRepoPath;
use super::visible_text::is_visible_text;

/// Maximum length (code points) of free-text fields and ids.
pub const MAX_TEXT_CHARS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitStatus {
    Planned,
    NotApplicable,
    OutOfScope,
    Deferred,
    InProgress,
    Blocked,
    Covered,
    Candidate,
}

impl UnitStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::NotApplicable => "not_applicable",
            Self::OutOfScope => "out_of_scope",
            Self::Deferred => "deferred",
            Self::InProgress => "in_progress",
            Self::Blocked => "blocked",
            Self::Covered => "covered",
            Self::Candidate => "candidate",
        }
    }
}

impl fmt::Display for UnitStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckMethod {
    /// Reasoned from source only; produces no artifact.
    Source,
    /// Ran something locally; must point at the artifact it produced.
    Local,
}

impl fmt::Display for CheckMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Source => "source",
            Self::Local => "local",
        })
    }
}

/// One concrete check behind a unit's claim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub owner: String,
    pub reviewed_paths: Vec<String>,
    pub invariant: String,
    pub method: CheckMethod,
    pub result: String,
    pub artifact: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageUnit {
    pub id: String,
    pub status: UnitStatus,
    pub owner: Option<String>,
    pub reviewed_paths: Vec<String>,
    pub checks: Vec<Check>,
    pub result_fingerprints: Vec<String>,
    pub unresolved: Vec<String>,
}

/// Why a unit or ledger is invalid. Closed set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoverageViolation {
    /// The status requires an owner and none is set.
    OwnerRequired,
    /// The status requires the unit to be unassigned.
    OwnerMustBeAbsent,
    FieldMustBeEmpty {
        field: &'static str,
    },
    FieldMustBeNonEmpty {
        field: &'static str,
    },
    /// `reviewed_paths` differs (as a set) from the union of the checks' paths.
    ReviewedPathsNotUnionOfChecks,
    DuplicateId {
        id: String,
    },
    /// Unit ids are not in ascending lexicographic order.
    NotSorted,
    SourceCheckHasArtifact,
    LocalCheckMissingArtifact,
    /// A local check's artifact is not a safe path under `agents/<owner>/artifacts/`.
    ArtifactNotOwnedByCheck,
    /// The unit id is not visible text.
    InvalidId,
    /// An owner (unit or check) is not a canonical lowercase agent id.
    InvalidOwner,
    /// An entry of a path field is not a safe repository-relative path.
    InvalidPath {
        field: &'static str,
    },
    /// A free-text field is empty, untrimmed, invisible or too long.
    InvalidText {
        field: &'static str,
    },
    /// A `result_fingerprints` entry is not a valid fingerprint.
    InvalidFingerprint,
    /// A list field repeats an entry.
    DuplicateEntry {
        field: &'static str,
    },
}

impl fmt::Display for CoverageViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OwnerRequired => write!(f, "this status requires an owner"),
            Self::OwnerMustBeAbsent => write!(f, "this status requires the unit to be unassigned"),
            Self::FieldMustBeEmpty { field } => write!(f, "{field} must be empty for this status"),
            Self::FieldMustBeNonEmpty { field } => {
                write!(f, "{field} must not be empty for this status")
            }
            Self::ReviewedPathsNotUnionOfChecks => {
                write!(
                    f,
                    "reviewed_paths must equal the union of the checks' reviewed_paths"
                )
            }
            Self::DuplicateId { id } => write!(f, "duplicate coverage unit id {id:?}"),
            Self::NotSorted => write!(f, "coverage units must be sorted by id"),
            Self::SourceCheckHasArtifact => write!(f, "a source check must not name an artifact"),
            Self::LocalCheckMissingArtifact => write!(f, "a local check must name its artifact"),
            Self::ArtifactNotOwnedByCheck => write!(
                f,
                "a local check's artifact must be a safe path under agents/<owner>/artifacts/"
            ),
            Self::InvalidId => write!(f, "coverage unit id must be visible text"),
            Self::InvalidOwner => write!(f, "owner must be a canonical lowercase agent id"),
            Self::InvalidPath { field } => {
                write!(f, "{field} contains an unsafe repository-relative path")
            }
            Self::InvalidText { field } => write!(f, "{field} contains invalid text"),
            Self::InvalidFingerprint => {
                write!(f, "result_fingerprints contains an invalid fingerprint")
            }
            Self::DuplicateEntry { field } => write!(f, "{field} contains a duplicate entry"),
        }
    }
}

impl std::error::Error for CoverageViolation {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Complete,
    Incomplete,
}

impl fmt::Display for RunStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Complete => "complete",
            Self::Incomplete => "incomplete",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncompleteReason {
    BudgetCannotFundReserves,
    ValidationBudgetExhausted,
    CriticBudgetExhausted,
    EngineUnavailable,
    Interrupted,
}

impl fmt::Display for IncompleteReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::BudgetCannotFundReserves => "the budget cannot fund the validation reserves",
            Self::ValidationBudgetExhausted => "the validation budget ran out",
            Self::CriticBudgetExhausted => "the critic budget ran out",
            Self::EngineUnavailable => "an analysis engine was unavailable",
            Self::Interrupted => "the run was interrupted",
        })
    }
}

/// `^[a-z0-9][a-z0-9_-]{0,63}$`, not a Windows device name.
fn is_safe_owner_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    let shape = !bytes.is_empty()
        && bytes.len() <= 64
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-');
    if !shape {
        return false;
    }
    let reserved = matches!(id, "con" | "prn" | "aux" | "nul")
        || (id.len() == 4
            && (id.starts_with("com") || id.starts_with("lpt"))
            && matches!(bytes[3], b'1'..=b'9'));
    !reserved
}

/// Safe path under `agents/<owner>/artifacts/` with something after the prefix.
fn is_owned_artifact(artifact: &str, owner: &str) -> bool {
    if !is_safe_owner_id(owner) || SafeRepoPath::parse(artifact).is_err() {
        return false;
    }
    let prefix = format!("agents/{owner}/artifacts/");
    matches!(artifact.strip_prefix(prefix.as_str()), Some(rest) if !rest.is_empty())
}

fn check_unique<'a>(
    items: impl IntoIterator<Item = &'a String>,
    field: &'static str,
    out: &mut Vec<CoverageViolation>,
) {
    let mut seen = BTreeSet::new();
    for item in items {
        if !seen.insert(item.as_str()) {
            out.push(CoverageViolation::DuplicateEntry { field });
            return;
        }
    }
}

fn check_paths(paths: &[String], field: &'static str, out: &mut Vec<CoverageViolation>) {
    if paths.iter().any(|p| SafeRepoPath::parse(p).is_err()) {
        out.push(CoverageViolation::InvalidPath { field });
    }
    check_unique(paths, field, out);
}

fn check_text(text: &str, field: &'static str, out: &mut Vec<CoverageViolation>) {
    if !is_visible_text(text, MAX_TEXT_CHARS) {
        out.push(CoverageViolation::InvalidText { field });
    }
}

fn validate_check(check: &Check, out: &mut Vec<CoverageViolation>) {
    if !is_safe_owner_id(&check.owner) {
        out.push(CoverageViolation::InvalidOwner);
    }
    if check.reviewed_paths.is_empty() {
        out.push(CoverageViolation::FieldMustBeNonEmpty {
            field: "checks[].reviewed_paths",
        });
    }
    check_paths(&check.reviewed_paths, "checks[].reviewed_paths", out);
    check_text(&check.invariant, "checks[].invariant", out);
    check_text(&check.result, "checks[].result", out);
    match (check.method, check.artifact.as_deref()) {
        (CheckMethod::Source, None) => {}
        (CheckMethod::Source, Some(_)) => out.push(CoverageViolation::SourceCheckHasArtifact),
        (CheckMethod::Local, None) => out.push(CoverageViolation::LocalCheckMissingArtifact),
        (CheckMethod::Local, Some(artifact)) => {
            if !is_owned_artifact(artifact, &check.owner) {
                out.push(CoverageViolation::ArtifactNotOwnedByCheck);
            }
        }
    }
}

fn require_empty<T>(v: &[T], field: &'static str, out: &mut Vec<CoverageViolation>) {
    if !v.is_empty() {
        out.push(CoverageViolation::FieldMustBeEmpty { field });
    }
}

fn require_non_empty<T>(v: &[T], field: &'static str, out: &mut Vec<CoverageViolation>) {
    if v.is_empty() {
        out.push(CoverageViolation::FieldMustBeNonEmpty { field });
    }
}

fn validate_state(unit: &CoverageUnit, out: &mut Vec<CoverageViolation>) {
    use UnitStatus::*;

    let require_owner = |out: &mut Vec<CoverageViolation>| match unit.owner.as_deref() {
        None => out.push(CoverageViolation::OwnerRequired),
        Some(o) if !is_safe_owner_id(o) => out.push(CoverageViolation::InvalidOwner),
        Some(_) => {}
    };
    let require_unassigned = |out: &mut Vec<CoverageViolation>| {
        if unit.owner.is_some() {
            out.push(CoverageViolation::OwnerMustBeAbsent);
        }
    };

    if unit.status != Candidate {
        require_empty(&unit.result_fingerprints, "result_fingerprints", out);
    }
    match unit.status {
        Planned => {
            require_unassigned(out);
            require_empty(&unit.reviewed_paths, "reviewed_paths", out);
            require_empty(&unit.checks, "checks", out);
            require_empty(&unit.unresolved, "unresolved", out);
        }
        NotApplicable | OutOfScope | Deferred => {
            require_unassigned(out);
            require_empty(&unit.reviewed_paths, "reviewed_paths", out);
            require_empty(&unit.checks, "checks", out);
            require_non_empty(&unit.unresolved, "unresolved", out);
        }
        InProgress => {
            require_owner(out);
            require_empty(&unit.reviewed_paths, "reviewed_paths", out);
            require_empty(&unit.checks, "checks", out);
            require_empty(&unit.unresolved, "unresolved", out);
        }
        Blocked => {
            require_owner(out);
            require_non_empty(&unit.reviewed_paths, "reviewed_paths", out);
            require_non_empty(&unit.checks, "checks", out);
            require_non_empty(&unit.unresolved, "unresolved", out);
        }
        Covered => {
            require_owner(out);
            require_non_empty(&unit.reviewed_paths, "reviewed_paths", out);
            require_non_empty(&unit.checks, "checks", out);
            require_empty(&unit.unresolved, "unresolved", out);
        }
        Candidate => {
            require_owner(out);
            require_non_empty(&unit.reviewed_paths, "reviewed_paths", out);
            require_non_empty(&unit.checks, "checks", out);
            require_non_empty(&unit.result_fingerprints, "result_fingerprints", out);
        }
    }
}

/// Validate one unit against the state table and field rules. Empty ⇒ valid.
///
/// State table (same as Cloudflare): `planned` unassigned, all four lists
/// empty; `not_applicable`/`out_of_scope`/`deferred` unassigned, paths/checks/
/// fingerprints empty, `unresolved` non-empty; `in_progress` owned, all four
/// empty; `blocked` owned, paths+checks non-empty, fingerprints empty,
/// `unresolved` non-empty; `covered` owned, paths+checks non-empty,
/// fingerprints and `unresolved` empty; `candidate` owned, paths+checks+
/// fingerprints non-empty. In every state `reviewed_paths` must equal the
/// union of the checks' `reviewed_paths` as a set.
pub fn validate_unit(unit: &CoverageUnit) -> Vec<CoverageViolation> {
    let mut out = Vec::new();
    if !is_visible_text(&unit.id, MAX_TEXT_CHARS) {
        out.push(CoverageViolation::InvalidId);
    }
    validate_state(unit, &mut out);

    check_paths(&unit.reviewed_paths, "reviewed_paths", &mut out);
    if unit
        .result_fingerprints
        .iter()
        .any(|f| Fingerprint::parse(f).is_err())
    {
        out.push(CoverageViolation::InvalidFingerprint);
    }
    check_unique(&unit.result_fingerprints, "result_fingerprints", &mut out);
    for item in &unit.unresolved {
        if !is_visible_text(item, MAX_TEXT_CHARS) {
            out.push(CoverageViolation::InvalidText {
                field: "unresolved",
            });
            break;
        }
    }
    check_unique(&unit.unresolved, "unresolved", &mut out);

    for check in &unit.checks {
        validate_check(check, &mut out);
    }

    let aggregate: BTreeSet<&str> = unit.reviewed_paths.iter().map(String::as_str).collect();
    let owned: BTreeSet<&str> = unit
        .checks
        .iter()
        .flat_map(|c| c.reviewed_paths.iter().map(String::as_str))
        .collect();
    if aggregate != owned {
        out.push(CoverageViolation::ReviewedPathsNotUnionOfChecks);
    }
    out
}

/// [`validate_unit`] for every unit, plus: ids unique and in ascending
/// lexicographic (byte) order. Empty ⇒ valid.
pub fn validate_ledger(units: &[CoverageUnit]) -> Vec<CoverageViolation> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut previous: Option<&str> = None;
    for unit in units {
        out.extend(validate_unit(unit));
        if !seen.insert(unit.id.as_str()) {
            out.push(CoverageViolation::DuplicateId {
                id: unit.id.clone(),
            });
        }
        if let Some(prev) = previous
            && prev > unit.id.as_str()
        {
            out.push(CoverageViolation::NotSorted);
        }
        previous = Some(unit.id.as_str());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use CoverageViolation as V;

    fn src_check(owner: &str, paths: &[&str]) -> Check {
        Check {
            owner: owner.into(),
            reviewed_paths: paths.iter().map(|s| s.to_string()).collect(),
            invariant: "input is validated before use".into(),
            method: CheckMethod::Source,
            result: "holds".into(),
            artifact: None,
        }
    }

    fn unit(id: &str, status: UnitStatus) -> CoverageUnit {
        CoverageUnit {
            id: id.into(),
            status,
            owner: None,
            reviewed_paths: vec![],
            checks: vec![],
            result_fingerprints: vec![],
            unresolved: vec![],
        }
    }

    fn with_evidence(mut u: CoverageUnit) -> CoverageUnit {
        u.owner = Some("hunter-1".into());
        u.reviewed_paths = vec!["src/a.rs".into()];
        u.checks = vec![src_check("hunter-1", &["src/a.rs"])];
        u
    }

    fn fp() -> String {
        Fingerprint::derive(&["engine", "kind", "src/a.rs"])
            .as_str()
            .to_owned()
    }

    #[test]
    fn planned() {
        assert!(validate_unit(&unit("m1", UnitStatus::Planned)).is_empty());
        let mut u = unit("m1", UnitStatus::Planned);
        u.owner = Some("a".into());
        u.unresolved = vec!["why".into()];
        let v = validate_unit(&u);
        assert!(v.contains(&V::OwnerMustBeAbsent));
        assert!(v.contains(&V::FieldMustBeEmpty {
            field: "unresolved"
        }));
    }

    #[test]
    fn unassigned_with_reason_states() {
        for s in [
            UnitStatus::NotApplicable,
            UnitStatus::OutOfScope,
            UnitStatus::Deferred,
        ] {
            let mut ok = unit("m1", s);
            ok.unresolved = vec!["budget".into()];
            assert!(validate_unit(&ok).is_empty(), "{s}");
            let bad = unit("m1", s);
            assert_eq!(
                validate_unit(&bad),
                vec![V::FieldMustBeNonEmpty {
                    field: "unresolved"
                }]
            );
            let mut bad2 = with_evidence(ok.clone());
            bad2.unresolved = vec!["x".into()];
            let v = validate_unit(&bad2);
            assert!(v.contains(&V::OwnerMustBeAbsent), "{s}");
            assert!(v.contains(&V::FieldMustBeEmpty {
                field: "reviewed_paths"
            }));
            assert!(v.contains(&V::FieldMustBeEmpty { field: "checks" }));
        }
    }

    #[test]
    fn in_progress() {
        let mut ok = unit("m1", UnitStatus::InProgress);
        ok.owner = Some("hunter-1".into());
        assert!(validate_unit(&ok).is_empty());
        assert_eq!(
            validate_unit(&unit("m1", UnitStatus::InProgress)),
            vec![V::OwnerRequired]
        );
        let bad = with_evidence(unit("m1", UnitStatus::InProgress));
        let v = validate_unit(&bad);
        assert!(v.contains(&V::FieldMustBeEmpty {
            field: "reviewed_paths"
        }));
        assert!(v.contains(&V::FieldMustBeEmpty { field: "checks" }));
    }

    #[test]
    fn blocked() {
        let mut ok = with_evidence(unit("m1", UnitStatus::Blocked));
        ok.unresolved = vec!["needs a running database".into()];
        assert!(validate_unit(&ok).is_empty(), "{:?}", validate_unit(&ok));
        let bad = with_evidence(unit("m1", UnitStatus::Blocked));
        assert_eq!(
            validate_unit(&bad),
            vec![V::FieldMustBeNonEmpty {
                field: "unresolved"
            }]
        );
        let mut bad2 = ok.clone();
        bad2.result_fingerprints = vec![fp()];
        assert_eq!(
            validate_unit(&bad2),
            vec![V::FieldMustBeEmpty {
                field: "result_fingerprints"
            }]
        );
    }

    #[test]
    fn covered() {
        let ok = with_evidence(unit("m1", UnitStatus::Covered));
        assert!(validate_unit(&ok).is_empty());
        let mut bad = unit("m1", UnitStatus::Covered);
        bad.owner = Some("hunter-1".into());
        let v = validate_unit(&bad);
        assert!(v.contains(&V::FieldMustBeNonEmpty {
            field: "reviewed_paths"
        }));
        assert!(v.contains(&V::FieldMustBeNonEmpty { field: "checks" }));
        let mut bad2 = ok.clone();
        bad2.unresolved = vec!["x".into()];
        assert_eq!(
            validate_unit(&bad2),
            vec![V::FieldMustBeEmpty {
                field: "unresolved"
            }]
        );
        let mut bad3 = ok.clone();
        bad3.owner = None;
        assert_eq!(validate_unit(&bad3), vec![V::OwnerRequired]);
    }

    #[test]
    fn candidate() {
        let mut ok = with_evidence(unit("m1", UnitStatus::Candidate));
        ok.result_fingerprints = vec![fp()];
        assert!(validate_unit(&ok).is_empty());
        let mut with_unresolved = ok.clone();
        with_unresolved.unresolved = vec!["partial".into()];
        assert!(validate_unit(&with_unresolved).is_empty());
        let bad = with_evidence(unit("m1", UnitStatus::Candidate));
        assert_eq!(
            validate_unit(&bad),
            vec![V::FieldMustBeNonEmpty {
                field: "result_fingerprints"
            }]
        );
        let mut bad2 = ok.clone();
        bad2.result_fingerprints = vec!["-bad".into()];
        assert_eq!(validate_unit(&bad2), vec![V::InvalidFingerprint]);
        let mut bad3 = ok.clone();
        bad3.result_fingerprints = vec![fp(), fp()];
        assert_eq!(
            validate_unit(&bad3),
            vec![V::DuplicateEntry {
                field: "result_fingerprints"
            }]
        );
    }

    #[test]
    fn reviewed_paths_must_equal_union() {
        let mut u = with_evidence(unit("m1", UnitStatus::Covered));
        u.reviewed_paths.push("src/b.rs".into());
        assert_eq!(validate_unit(&u), vec![V::ReviewedPathsNotUnionOfChecks]);
        let mut u2 = with_evidence(unit("m1", UnitStatus::Covered));
        u2.checks.push(src_check("hunter-2", &["src/c.rs"]));
        assert_eq!(validate_unit(&u2), vec![V::ReviewedPathsNotUnionOfChecks]);
        // Union across checks, order-insensitive.
        let mut u3 = with_evidence(unit("m1", UnitStatus::Covered));
        u3.checks
            .push(src_check("hunter-2", &["src/c.rs", "src/a.rs"]));
        u3.reviewed_paths = vec!["src/c.rs".into(), "src/a.rs".into()];
        assert!(validate_unit(&u3).is_empty());
    }

    #[test]
    fn check_artifact_rules() {
        let mut u = with_evidence(unit("m1", UnitStatus::Covered));
        u.checks[0].artifact = Some("agents/hunter-1/artifacts/run.log".into());
        assert_eq!(validate_unit(&u), vec![V::SourceCheckHasArtifact]);

        u.checks[0].method = CheckMethod::Local;
        assert!(validate_unit(&u).is_empty());

        for bad in [
            "agents/hunter-2/artifacts/run.log",
            "agents/hunter-1/artifacts/",
            "agents/hunter-1/artifacts/../../x",
            "agents/hunter-1/artifactsX/run.log",
            "/agents/hunter-1/artifacts/run.log",
            "x/agents/hunter-1/artifacts/run.log",
        ] {
            u.checks[0].artifact = Some(bad.into());
            assert_eq!(validate_unit(&u), vec![V::ArtifactNotOwnedByCheck], "{bad}");
        }
        u.checks[0].artifact = None;
        assert_eq!(validate_unit(&u), vec![V::LocalCheckMissingArtifact]);
    }

    #[test]
    fn field_level_rules() {
        let mut u = with_evidence(unit("m1", UnitStatus::Covered));
        u.reviewed_paths = vec!["../x".into()];
        u.checks[0].reviewed_paths = vec!["../x".into()];
        let v = validate_unit(&u);
        assert!(v.contains(&V::InvalidPath {
            field: "reviewed_paths"
        }));
        assert!(v.contains(&V::InvalidPath {
            field: "checks[].reviewed_paths"
        }));

        let mut u = with_evidence(unit("m1", UnitStatus::Covered));
        u.checks[0].owner = "Hunter".into();
        assert!(validate_unit(&u).contains(&V::InvalidOwner));
        u.checks[0].owner = "com1".into();
        assert!(validate_unit(&u).contains(&V::InvalidOwner));

        let mut u = with_evidence(unit("m1", UnitStatus::Covered));
        u.owner = Some("bad owner".into());
        assert_eq!(validate_unit(&u), vec![V::InvalidOwner]);

        let mut u = with_evidence(unit("m1", UnitStatus::Covered));
        u.checks[0].invariant = " ".into();
        u.checks[0].result = "\u{200b}".into();
        let v = validate_unit(&u);
        assert!(v.contains(&V::InvalidText {
            field: "checks[].invariant"
        }));
        assert!(v.contains(&V::InvalidText {
            field: "checks[].result"
        }));

        let mut u = with_evidence(unit("m1", UnitStatus::Covered));
        u.checks[0].reviewed_paths.clear();
        let v = validate_unit(&u);
        assert!(v.contains(&V::FieldMustBeNonEmpty {
            field: "checks[].reviewed_paths"
        }));

        let mut u = unit("m1", UnitStatus::Deferred);
        u.unresolved = vec!["a".into(), "a".into()];
        assert_eq!(
            validate_unit(&u),
            vec![V::DuplicateEntry {
                field: "unresolved"
            }]
        );
        u.unresolved = vec!["".into()];
        assert_eq!(
            validate_unit(&u),
            vec![V::InvalidText {
                field: "unresolved"
            }]
        );

        for bad_id in ["", " m1", "\u{200b}"] {
            let mut u = unit(bad_id, UnitStatus::Planned);
            u.id = bad_id.into();
            assert_eq!(validate_unit(&u), vec![V::InvalidId], "{bad_id:?}");
        }
    }

    #[test]
    fn ledger_unique_and_sorted() {
        let a = unit("a", UnitStatus::Planned);
        let b = unit("b", UnitStatus::Planned);
        assert!(validate_ledger(&[a.clone(), b.clone()]).is_empty());
        assert!(validate_ledger(&[]).is_empty());
        assert_eq!(validate_ledger(&[b.clone(), a.clone()]), vec![V::NotSorted]);
        assert_eq!(
            validate_ledger(&[a.clone(), a.clone()]),
            vec![V::DuplicateId { id: "a".into() }]
        );
        let mut bad = unit("c", UnitStatus::Planned);
        bad.owner = Some("x".into());
        assert_eq!(validate_ledger(&[a, b, bad]), vec![V::OwnerMustBeAbsent]);
    }

    #[test]
    fn serde_shapes() {
        let mut u = with_evidence(unit("mod::a", UnitStatus::NotApplicable));
        u.status = UnitStatus::Covered;
        let json = serde_json::to_value(&u).unwrap();
        assert_eq!(json["status"], "covered");
        assert_eq!(json["checks"][0]["method"], "source");
        assert!(json["checks"][0]["artifact"].is_null());
        let back: CoverageUnit = serde_json::from_value(json).unwrap();
        assert_eq!(back, u);
        assert!(serde_json::from_str::<CoverageUnit>(
            r#"{"id":"a","status":"planned","owner":null,"reviewed_paths":[],"checks":[],"result_fingerprints":[],"unresolved":[],"extra":1}"#
        )
        .is_err());
        for (s, wire) in [
            (UnitStatus::NotApplicable, "not_applicable"),
            (UnitStatus::OutOfScope, "out_of_scope"),
            (UnitStatus::InProgress, "in_progress"),
        ] {
            assert_eq!(serde_json::to_value(s).unwrap(), wire);
            assert_eq!(s.to_string(), wire);
        }
        assert_eq!(
            serde_json::to_value(RunStatus::Incomplete).unwrap(),
            "incomplete"
        );
        assert_eq!(
            serde_json::to_value(IncompleteReason::BudgetCannotFundReserves).unwrap(),
            "budget_cannot_fund_reserves"
        );
        let r: IncompleteReason = serde_json::from_str("\"engine_unavailable\"").unwrap();
        assert_eq!(r, IncompleteReason::EngineUnavailable);
        assert_eq!(serde_json::to_value(CheckMethod::Local).unwrap(), "local");
    }

    #[test]
    fn display_messages() {
        let all = [
            V::OwnerRequired,
            V::OwnerMustBeAbsent,
            V::FieldMustBeEmpty { field: "checks" },
            V::FieldMustBeNonEmpty {
                field: "unresolved",
            },
            V::ReviewedPathsNotUnionOfChecks,
            V::DuplicateId { id: "a".into() },
            V::NotSorted,
            V::SourceCheckHasArtifact,
            V::LocalCheckMissingArtifact,
            V::ArtifactNotOwnedByCheck,
            V::InvalidId,
            V::InvalidOwner,
            V::InvalidPath {
                field: "reviewed_paths",
            },
            V::InvalidText {
                field: "unresolved",
            },
            V::InvalidFingerprint,
            V::DuplicateEntry {
                field: "unresolved",
            },
        ];
        for v in &all {
            assert!(!v.to_string().is_empty(), "{v:?}");
        }
        assert_eq!(all[2].to_string(), "checks must be empty for this status");
        assert_eq!(all[5].to_string(), "duplicate coverage unit id \"a\"");
        assert_eq!(RunStatus::Complete.to_string(), "complete");
        assert_eq!(
            IncompleteReason::Interrupted.to_string(),
            "the run was interrupted"
        );
        assert_eq!(CheckMethod::Source.to_string(), "source");
    }
}
