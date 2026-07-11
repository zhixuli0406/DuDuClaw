//! WP4 — delegation permission decay ("narrower wins").
//!
//! When agent A delegates work to agent B, B must not be able to *widen* what A
//! could do. Before this, B ran with B's own permissions, so a low-trust A could
//! reach data through a high-trust B. The fix, borrowed from paperclip's layered
//! trust boundaries: carry A's effective-permission snapshot on the bus task and
//! run B with the **intersection** of (B's own permissions ∩ A's snapshot).
//!
//! This module owns the deterministic core: the [`PermissionSnapshot`] shape and
//! the [`intersect`] rule. The dispatcher attaches a snapshot to each delegated
//! bus task; the executor intersects it with the callee's permissions and passes
//! the result to the CLI spawn (`--disallowedTools`) and the Odoo pool.
//!
//! ## Empty-set semantics (the subtle part)
//!
//! For allow-lists, an **empty** list means "no restriction" (allow all), which
//! is *wider* than any non-empty list. So `intersect(empty, non_empty)` yields
//! the non-empty list — the narrower of the two wins. For deny-lists, the
//! **union** is taken (either party's denial applies).

use serde::{Deserialize, Serialize};

/// A snapshot of an agent's effective permissions, carried across a delegation
/// hop. All lists are additive/optional so old bus tasks (no snapshot) keep
/// working — a `None` snapshot means "no decay applied" (legacy behaviour).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionSnapshot {
    /// Tool allow-list. Empty ⇒ all tools allowed (no restriction).
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Tool deny-list. Union across a delegation chain.
    #[serde(default)]
    pub denied_tools: Vec<String>,
    /// Odoo model allow-list (e.g. `crm.lead`). Empty ⇒ all models.
    #[serde(default)]
    pub odoo_allowed_models: Vec<String>,
    /// Odoo action allow-list (e.g. `read`, `write:crm.lead`). Empty ⇒ all.
    #[serde(default)]
    pub odoo_allowed_actions: Vec<String>,
}

/// Intersect an allow-list under "empty = no restriction" semantics.
///
/// - both empty ⇒ empty (still no restriction)
/// - one empty ⇒ the other (the restrictive one wins)
/// - both non-empty ⇒ set intersection (only items in BOTH survive)
fn intersect_allowlist(a: &[String], b: &[String]) -> Vec<String> {
    match (a.is_empty(), b.is_empty()) {
        (true, true) => Vec::new(),
        (true, false) => dedup_preserve(b),
        (false, true) => dedup_preserve(a),
        (false, false) => {
            let mut out: Vec<String> = a
                .iter()
                .filter(|x| b.iter().any(|y| y == *x))
                .cloned()
                .collect();
            out.sort();
            out.dedup();
            out
        }
    }
}

/// Union two deny-lists (either party's denial applies), deduped + sorted.
fn union_denylist(a: &[String], b: &[String]) -> Vec<String> {
    let mut out: Vec<String> = a.iter().chain(b.iter()).cloned().collect();
    out.sort();
    out.dedup();
    out
}

fn dedup_preserve(v: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    v.iter().filter(|x| seen.insert((*x).clone())).cloned().collect()
}

/// Compute the effective permissions for the callee: `callee ∩ delegated`
/// following the narrower-wins rules above. Symmetric for allow-lists; deny is
/// a union so nobody can drop a restriction by delegating.
pub fn intersect(delegated: &PermissionSnapshot, callee: &PermissionSnapshot) -> PermissionSnapshot {
    PermissionSnapshot {
        allowed_tools: intersect_allowlist(&delegated.allowed_tools, &callee.allowed_tools),
        denied_tools: union_denylist(&delegated.denied_tools, &callee.denied_tools),
        odoo_allowed_models: intersect_allowlist(
            &delegated.odoo_allowed_models,
            &callee.odoo_allowed_models,
        ),
        odoo_allowed_actions: intersect_allowlist(
            &delegated.odoo_allowed_actions,
            &callee.odoo_allowed_actions,
        ),
    }
}

/// Default maximum delegation depth. A chain deeper than this is refused by the
/// dispatcher (prevents unbounded permission-laundering hops + runaway fan-out).
pub const DEFAULT_MAX_DELEGATION_DEPTH: u8 = 3;

/// Whether a hop at `depth` is within the allowed limit.
pub fn depth_within_limit(depth: u8, max: u8) -> bool {
    depth <= max
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(allowed: &[&str], denied: &[&str]) -> PermissionSnapshot {
        PermissionSnapshot {
            allowed_tools: allowed.iter().map(|s| s.to_string()).collect(),
            denied_tools: denied.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn narrower_allowlist_wins_when_a_is_narrower() {
        // A allows only Read; B allows Read+Write ⇒ effective = Read.
        let a = snap(&["Read"], &[]);
        let b = snap(&["Read", "Write"], &[]);
        let eff = intersect(&a, &b);
        assert_eq!(eff.allowed_tools, vec!["Read".to_string()]);
    }

    #[test]
    fn narrower_allowlist_wins_when_b_is_narrower() {
        let a = snap(&["Read", "Write"], &[]);
        let b = snap(&["Write"], &[]);
        let eff = intersect(&a, &b);
        assert_eq!(eff.allowed_tools, vec!["Write".to_string()]);
    }

    #[test]
    fn empty_allowlist_means_no_restriction_so_other_wins() {
        // 空 ∩ 非空 = 非空那組 (the restrictive party wins).
        let a = snap(&[], &[]); // A: unrestricted
        let b = snap(&["Read"], &[]); // B: only Read
        assert_eq!(intersect(&a, &b).allowed_tools, vec!["Read".to_string()]);
        // symmetric
        assert_eq!(intersect(&b, &a).allowed_tools, vec!["Read".to_string()]);
    }

    #[test]
    fn both_empty_stays_unrestricted() {
        assert!(intersect(&snap(&[], &[]), &snap(&[], &[])).allowed_tools.is_empty());
    }

    #[test]
    fn denylist_is_union() {
        let a = snap(&[], &["Bash"]);
        let b = snap(&[], &["WebFetch"]);
        assert_eq!(
            intersect(&a, &b).denied_tools,
            vec!["Bash".to_string(), "WebFetch".to_string()]
        );
    }

    #[test]
    fn odoo_models_intersect() {
        let a = PermissionSnapshot {
            odoo_allowed_models: vec!["crm.lead".into()],
            ..Default::default()
        };
        let b = PermissionSnapshot {
            odoo_allowed_models: vec!["crm.lead".into(), "res.partner".into()],
            ..Default::default()
        };
        // A can only see crm.lead ⇒ delegating to B does not widen to res.partner.
        assert_eq!(intersect(&a, &b).odoo_allowed_models, vec!["crm.lead".to_string()]);
    }

    #[test]
    fn depth_limit() {
        assert!(depth_within_limit(0, DEFAULT_MAX_DELEGATION_DEPTH));
        assert!(depth_within_limit(3, 3));
        assert!(!depth_within_limit(4, 3));
    }
}
