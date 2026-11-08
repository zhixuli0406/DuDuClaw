//! RM-027 — the one validation rule for changing an agent's place in the org
//! chart (`reports_to` / `department`).
//!
//! Every writer that moves an agent in the authoritative org record
//! ([`crate::org_store`]) runs [`validate_org_change`] first: the dashboard's
//! `agents.update`, the `org.propose_change` / `org.apply_change` RPCs and the
//! approval that applies a Manager's proposal (re-validated at decision time,
//! since the tree may have moved while the request waited).
//!
//! # What is checked
//!
//! - the agent being moved exists (`<home>/agents/<id>/agent.toml`);
//! - a new supervisor, when one is named, is not the agent itself, exists, and
//!   does not make the agent its own ancestor. The chain is walked through the
//!   **authority** — `org.toml` where it has an entry, the agent's
//!   `agent.toml` mirror otherwise (the [`crate::org_store`] authority rule) —
//!   so a cycle that only exists in the authority cannot slip through;
//! - a new department is a valid name and, when departments can be
//!   enumerated at all, one of them ([`known_departments`]). Clearing the
//!   department (empty string) is always allowed.
//!
//! The agent's *current* placement is never re-judged: a value that equals
//! what the authority already says is accepted, so a no-op cannot be refused
//! because of drift elsewhere.

use std::collections::{BTreeSet, HashSet};
use std::fmt;
use std::path::Path;

use crate::org_store::{self, OrgEntry};

/// Safety bound on the supervisor walk (the tree is operator-sized; a longer
/// chain is treated as a cycle — fail closed).
const MAX_DEPTH: usize = 64;

/// Why a proposed org change was refused. `Display` is end-user zh-TW copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrgChangeError {
    /// The agent id is malformed or no such agent exists.
    AgentNotFound(String),
    /// `reports_to` names the agent itself.
    SelfReport(String),
    /// `reports_to` names an agent that does not exist.
    TargetNotFound(String),
    /// Setting `reports_to` would put the agent under its own subtree.
    Cycle { agent_id: String, reports_to: String },
    /// The department name is not a valid department identifier.
    InvalidDepartment(String),
    /// Departments are enumerated and this one is not among them.
    UnknownDepartment(String),
    /// The authority (`org.toml`) was seeded but is lost / unreadable /
    /// corrupt / of a newer schema: nothing can be validated against it, and
    /// the mirror fallback is not trusted for a write.
    AuthorityUnavailable(org_store::OrgStoreError),
}

impl OrgChangeError {
    /// Stable machine-readable code (for UIs that branch on the reason).
    pub fn code(&self) -> &'static str {
        match self {
            Self::AgentNotFound(_) => "agent_not_found",
            Self::SelfReport(_) => "self_report",
            Self::TargetNotFound(_) => "target_not_found",
            Self::Cycle { .. } => "cycle",
            Self::InvalidDepartment(_) => "invalid_department",
            Self::UnknownDepartment(_) => "unknown_department",
            Self::AuthorityUnavailable(e) => e.code(),
        }
    }
}

impl fmt::Display for OrgChangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn short(s: &str) -> String {
            crate::truncate_chars(s, 64)
        }
        match self {
            Self::AgentNotFound(a) => write!(f, "找不到 AI 員工「{}」。", short(a)),
            Self::SelfReport(a) => write!(f, "「{}」不能設定成自己的主管。", short(a)),
            Self::TargetNotFound(t) => write!(
                f,
                "指定的主管「{}」不存在；請選擇現有的 AI 員工，或留空代表最上層。",
                short(t)
            ),
            Self::Cycle { agent_id, reports_to } => write!(
                f,
                "不能讓「{}」向「{}」回報：「{}」目前在「{}」的團隊之下，這樣會形成循環。",
                short(agent_id),
                short(reports_to),
                short(reports_to),
                short(agent_id)
            ),
            Self::InvalidDepartment(d) => write!(
                f,
                "部門名稱「{}」不合法（1–64 位元組，不可含空白、斜線或控制字元）。",
                short(d)
            ),
            Self::UnknownDepartment(d) => write!(
                f,
                "部門「{}」不存在；請先在部門管理建立，或選擇現有部門。",
                short(d)
            ),
            Self::AuthorityUnavailable(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for OrgChangeError {}

/// Does `<home>/agents/<id>/agent.toml` exist for a well-formed, non-internal id?
pub fn agent_exists(home: &Path, agent_id: &str) -> bool {
    let id = agent_id.trim();
    crate::is_valid_agent_id(id)
        && !crate::is_internal_agent_dir(id)
        && home.join("agents").join(id).join("agent.toml").is_file()
}

/// The agent's **effective** placement under the authority rule: the
/// `org.toml` entry when there is one, the `agent.toml` mirror otherwise.
/// `None` when neither exists. Tolerant ([`org_store::load`]) — for display;
/// a writer uses [`effective_in`] on a store from
/// [`org_store::OrgMutationGuard::load`].
pub fn effective_entry(home: &Path, agent_id: &str) -> Option<OrgEntry> {
    effective_from(&org_store::load(home), home, agent_id)
}

/// [`effective_entry`] against an already-loaded (strict) store.
pub fn effective_in(store: &org_store::OrgStore, home: &Path, agent_id: &str) -> Option<OrgEntry> {
    effective_from(store, home, agent_id)
}

fn effective_from(store: &org_store::OrgStore, home: &Path, agent_id: &str) -> Option<OrgEntry> {
    let id = agent_id.trim();
    if let Some(entry) = store.get(id) {
        return Some(entry.clone());
    }
    if !crate::is_valid_agent_id(id) || crate::is_internal_agent_dir(id) {
        return None;
    }
    org_store::read_mirror(&home.join("agents").join(id).join("agent.toml"))
}

/// Fingerprint of the agent's current effective placement — the `base` an
/// org-change request is made against. Includes whether the value came from
/// the authority or the mirror fallback, so adopting a mirror into the store
/// also counts as a change. Lowercase sha256 hex. Tolerant (display); the
/// compare-and-set inside the org lock uses [`entry_hash_in`].
pub fn entry_hash(home: &Path, agent_id: &str) -> String {
    entry_hash_in(&org_store::load(home), home, agent_id)
}

/// [`entry_hash`] against an already-loaded (strict) store.
pub fn entry_hash_in(store: &org_store::OrgStore, home: &Path, agent_id: &str) -> String {
    let id = agent_id.trim();
    let (source, entry) = match store.get(id) {
        Some(e) => ("store", Some(e.clone())),
        None => ("mirror", effective_from(store, home, id)),
    };
    let entry = entry.unwrap_or_default();
    digest(&[id, source, &entry.reports_to, &entry.department])
}

fn digest(parts: &[&str]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for part in parts {
        h.update((part.len() as u64).to_be_bytes());
        h.update(part.as_bytes());
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Every department name the install knows about: departments recorded in
/// the authority and in agent mirrors, plus the shared-wiki / shared-skill
/// `departments/<name>/` directories (the same union `departments.list`
/// shows). Only valid names are returned.
pub fn known_departments(home: &Path) -> BTreeSet<String> {
    known_departments_in(&org_store::load(home), home)
}

/// [`known_departments`] with the authority's departments taken from `store`.
pub fn known_departments_in(store: &org_store::OrgStore, home: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (_, entry) in store.iter() {
        out.insert(entry.department.clone());
    }
    for (_, entry) in org_store::scan_mirrors(home) {
        out.insert(entry.department);
    }
    for root in ["wiki", "skills"] {
        let dir = home
            .join("shared")
            .join(root)
            .join(crate::department::DEPARTMENTS_NAMESPACE);
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                if e.file_type().is_ok_and(|t| t.is_dir())
                    && let Ok(name) = e.file_name().into_string()
                {
                    out.insert(name);
                }
            }
        }
    }
    out.retain(|d| crate::department::is_valid_department(d));
    out
}

/// Validate moving `agent_id` to `new_reports_to` and/or `new_department`.
///
/// `None` leaves that field untouched (not checked). `Some("")` (or
/// `Some("none")` for `reports_to`) means root / no department. Values are
/// trimmed before checking.
///
/// Reads the authority **strictly** ([`org_store::load_for_write`]): a
/// seeded-but-unusable `org.toml` refuses with
/// [`OrgChangeError::AuthorityUnavailable`] rather than validating against
/// the `agent.toml` mirrors. This is an early check; the decisive one runs
/// inside the org mutation lock through [`validate_org_change_in`].
pub fn validate_org_change(
    home: &Path,
    agent_id: &str,
    new_reports_to: Option<&str>,
    new_department: Option<&str>,
) -> Result<(), OrgChangeError> {
    let agent_id = agent_id.trim();
    if !agent_exists(home, agent_id) {
        return Err(OrgChangeError::AgentNotFound(agent_id.to_string()));
    }
    let store = org_store::load_for_write(home).map_err(OrgChangeError::AuthorityUnavailable)?;
    validate_org_change_in(&store, home, agent_id, new_reports_to, new_department)
}

/// [`validate_org_change`] against `store` — the authority as re-read inside
/// [`org_store::OrgMutationGuard`], so the whole tree it walks is the tree
/// the following commit lands on.
pub fn validate_org_change_in(
    store: &org_store::OrgStore,
    home: &Path,
    agent_id: &str,
    new_reports_to: Option<&str>,
    new_department: Option<&str>,
) -> Result<(), OrgChangeError> {
    let agent_id = agent_id.trim();
    if !agent_exists(home, agent_id) {
        return Err(OrgChangeError::AgentNotFound(agent_id.to_string()));
    }
    let current = effective_from(store, home, agent_id).unwrap_or_default();

    if let Some(raw) = new_reports_to {
        let target = OrgEntry::new(raw, "").reports_to;
        if !target.is_empty() && target != current.reports_to {
            check_reports_to(store, home, agent_id, &target)?;
        } else if !target.is_empty() && target == agent_id {
            // Defensive: an authority already pointing at itself is still refused.
            return Err(OrgChangeError::SelfReport(agent_id.to_string()));
        }
    }

    if let Some(raw) = new_department {
        let dept = raw.trim();
        if !dept.is_empty() && dept != current.department {
            if !crate::department::is_valid_department(dept) {
                return Err(OrgChangeError::InvalidDepartment(dept.to_string()));
            }
            let known = known_departments_in(store, home);
            if !known.is_empty() && !known.contains(dept) {
                return Err(OrgChangeError::UnknownDepartment(dept.to_string()));
            }
        }
    }
    Ok(())
}

fn check_reports_to(
    store: &org_store::OrgStore,
    home: &Path,
    agent_id: &str,
    target: &str,
) -> Result<(), OrgChangeError> {
    if target == agent_id {
        return Err(OrgChangeError::SelfReport(agent_id.to_string()));
    }
    if !agent_exists(home, target) {
        return Err(OrgChangeError::TargetNotFound(target.to_string()));
    }
    // Walk up from the new supervisor; reaching `agent_id` (or revisiting a
    // node, or running past the bound) means the move would close a loop.
    let cycle = || OrgChangeError::Cycle {
        agent_id: agent_id.to_string(),
        reports_to: target.to_string(),
    };
    let mut visited: HashSet<String> = HashSet::new();
    let mut current = target.to_string();
    for _ in 0..MAX_DEPTH {
        if current == agent_id || !visited.insert(current.clone()) {
            return Err(cycle());
        }
        let next = effective_from(store, home, &current)
            .map(|e| e.reports_to)
            .unwrap_or_default();
        if next.is_empty() {
            return Ok(()); // reached a root (or a dangling reference)
        }
        current = next;
    }
    Err(cycle())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_agent(home: &Path, dir: &str, reports_to: &str, department: &str) {
        let d = home.join("agents").join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("agent.toml"),
            format!(
                "[agent]\nname = \"{dir}\"\ndisplay_name = \"{dir}\"\nrole = \"specialist\"\n\
                 status = \"active\"\ntrigger = \"@{dir}\"\nreports_to = \"{reports_to}\"\n\
                 department = \"{department}\"\nicon = \"x\"\n"
            ),
        )
        .unwrap();
    }

    /// ceo ← lead ← rep (all in 業務 except ceo in 經營).
    fn tree() -> tempfile::TempDir {
        let h = tempfile::tempdir().unwrap();
        write_agent(h.path(), "ceo", "", "經營");
        write_agent(h.path(), "lead", "ceo", "業務");
        write_agent(h.path(), "rep", "lead", "業務");
        org_store::seed_if_absent(h.path()).unwrap();
        h
    }

    #[test]
    fn a_valid_move_passes() {
        let h = tree();
        assert_eq!(validate_org_change(h.path(), "rep", Some("ceo"), Some("經營")), Ok(()));
        // Root and clearing the department are always allowed.
        assert_eq!(validate_org_change(h.path(), "rep", Some(""), Some("")), Ok(()));
        assert_eq!(validate_org_change(h.path(), "rep", Some("none"), None), Ok(()));
    }

    #[test]
    fn self_report_is_refused() {
        let h = tree();
        assert_eq!(
            validate_org_change(h.path(), "lead", Some("lead"), None),
            Err(OrgChangeError::SelfReport("lead".into()))
        );
    }

    #[test]
    fn missing_agent_or_target_is_refused() {
        let h = tree();
        assert_eq!(
            validate_org_change(h.path(), "ghost", Some("ceo"), None).unwrap_err().code(),
            "agent_not_found"
        );
        assert_eq!(
            validate_org_change(h.path(), "rep", Some("ghost"), None),
            Err(OrgChangeError::TargetNotFound("ghost".into()))
        );
        // Path-shaped ids never resolve to a directory.
        for bad in ["../ceo", "_trash", ".ephemeral", "a/b"] {
            assert!(validate_org_change(h.path(), bad, None, None).is_err(), "{bad}");
            assert!(validate_org_change(h.path(), "rep", Some(bad), None).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_cycle_is_refused_including_one_only_the_authority_knows() {
        let h = tree();
        // ceo → rep: rep is under lead under ceo.
        assert_eq!(
            validate_org_change(h.path(), "ceo", Some("rep"), None).unwrap_err().code(),
            "cycle"
        );
        assert_eq!(
            validate_org_change(h.path(), "lead", Some("rep"), None).unwrap_err().code(),
            "cycle"
        );
        // The mirror says rep is a root, the authority still says rep → lead:
        // the walk must follow the authority.
        write_agent(h.path(), "rep", "", "業務");
        assert_eq!(
            validate_org_change(h.path(), "lead", Some("rep"), None).unwrap_err().code(),
            "cycle"
        );
    }

    #[test]
    fn departments_must_be_valid_and_known() {
        let h = tree();
        assert_eq!(
            validate_org_change(h.path(), "rep", None, Some("a b")).unwrap_err().code(),
            "invalid_department"
        );
        assert_eq!(
            validate_org_change(h.path(), "rep", None, Some("行銷")).unwrap_err().code(),
            "unknown_department"
        );
        // A pre-created department (shared-wiki dir) is known.
        std::fs::create_dir_all(h.path().join("shared/wiki/departments/行銷")).unwrap();
        assert_eq!(validate_org_change(h.path(), "rep", None, Some("行銷")), Ok(()));
        // An install with no departments at all does not enumerate any, so
        // the first one may be named freely.
        let empty = tempfile::tempdir().unwrap();
        write_agent(empty.path(), "solo", "", "");
        assert_eq!(validate_org_change(empty.path(), "solo", None, Some("新部門")), Ok(()));
    }

    /// RM-027 review: a seeded-but-unusable authority must not be replaced by
    /// the mirror fallback for validation (the mirrors are what an agent can
    /// tamper with).
    #[test]
    fn an_unusable_authority_refuses_validation_instead_of_trusting_mirrors() {
        let h = tree();
        // Corrupt: the mirror tree alone would accept lead → rep once rep's
        // mirror is tampered to root; the strict check refuses outright.
        write_agent(h.path(), "rep", "", "業務");
        std::fs::write(org_store::org_store_path(h.path()), "[[broken").unwrap();
        let e = validate_org_change(h.path(), "lead", Some("rep"), None).unwrap_err();
        assert_eq!(e.code(), "org_authority_unavailable", "{e}");
        assert!(e.to_string().contains("org sync"), "{e}");
        // Lost on a bootstrapped home.
        std::fs::remove_file(org_store::org_store_path(h.path())).unwrap();
        assert!(matches!(
            validate_org_change(h.path(), "rep", Some("ceo"), None),
            Err(OrgChangeError::AuthorityUnavailable(org_store::OrgStoreError::Lost))
        ));
        // Newer schema.
        std::fs::write(org_store::org_store_path(h.path()), "schema = 7\n").unwrap();
        assert_eq!(
            validate_org_change(h.path(), "rep", None, Some("經營")).unwrap_err().code(),
            "org_authority_unavailable"
        );
        // Display paths stay tolerant.
        assert!(effective_entry(h.path(), "rep").is_some());
        assert_eq!(entry_hash(h.path(), "rep").len(), 64);
    }

    #[test]
    fn unchanged_values_are_never_re_judged() {
        let h = tree();
        // Current values pass even though nothing else would be checked.
        assert_eq!(validate_org_change(h.path(), "rep", Some("lead"), Some("業務")), Ok(()));
    }

    #[test]
    fn the_base_hash_follows_the_authority() {
        let h = tree();
        let before = entry_hash(h.path(), "rep");
        assert_eq!(before, entry_hash(h.path(), "rep"));
        // Mirror-only edits do not move the base (the authority is unchanged)…
        write_agent(h.path(), "rep", "ceo", "業務");
        assert_eq!(before, entry_hash(h.path(), "rep"));
        // …an authority write does.
        org_store::upsert(h.path(), "rep", OrgEntry::new("ceo", "業務")).unwrap();
        assert_ne!(before, entry_hash(h.path(), "rep"));
        assert_eq!(before.len(), 64);
    }
}
