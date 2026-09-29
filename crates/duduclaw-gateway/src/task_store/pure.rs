//! G1 pure helpers — no I/O, fully unit-tested.
//! Moved verbatim out of `task_store.rs`.

use super::*;

// ── G1 pure helpers (no I/O, fully unit-tested) ─────────────

/// Parse a `depends_on` JSON array of task ids. Malformed / non-array input is
/// treated as "no dependencies" (fail-open on the *shape*, not on gating — an
/// empty dep list just means immediately claimable).
pub fn parse_depends_on(json: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(json).unwrap_or_default()
}

/// Are every dependency id present in the `done` set? Empty deps ⇒ satisfied.
pub fn deps_satisfied(depends_on: &[String], done: &HashSet<String>) -> bool {
    depends_on.iter().all(|d| done.contains(d))
}

/// Has a lease (RFC3339) elapsed relative to `now` (RFC3339)? Unparseable
/// timestamps are treated as *expired* so a corrupt lease can't pin a zombie
/// forever (fail-safe toward reclaim).
pub fn lease_is_expired(lease_expires_at: &str, now: &str) -> bool {
    match (
        DateTime::parse_from_rfc3339(lease_expires_at),
        DateTime::parse_from_rfc3339(now),
    ) {
        (Ok(lease), Ok(now)) => now >= lease,
        _ => true,
    }
}

/// Conservative zombie-reclaim decision (G1 lease renewal, v1.36).
///
/// A claimed task is reclaim-due only when its lease has expired AND a further
/// full lease window has elapsed since expiry with no renewal. The window is
/// derived per task as `lease_expires_at - renewal_anchor` (anchor = last
/// renewal, falling back to the claim time), so the store needs no lease-length
/// config. A live worker's renewal ticker keeps pushing `lease_expires_at`
/// forward, so it never reaches expiry in the first place; the grace window
/// additionally absorbs a tick that is late or in flight.
///
/// Corrupt / unparseable lease or `now` ⇒ due (a corrupt lease must not pin a
/// zombie forever — same fail-safe direction as [`lease_is_expired`]). A
/// missing / unparseable anchor degrades to a zero grace window (legacy rows:
/// reclaim at plain expiry).
pub fn zombie_reclaim_due(lease_expires_at: &str, renewal_anchor: Option<&str>, now: &str) -> bool {
    let (lease, now_ts) = match (
        DateTime::parse_from_rfc3339(lease_expires_at),
        DateTime::parse_from_rfc3339(now),
    ) {
        (Ok(l), Ok(n)) => (l, n),
        _ => return true,
    };
    if now_ts < lease {
        return false; // lease still live
    }
    let window = renewal_anchor
        .and_then(|a| DateTime::parse_from_rfc3339(a).ok())
        .map(|a| (lease - a).max(chrono::Duration::zero()))
        .unwrap_or_else(chrono::Duration::zero);
    now_ts >= lease + window
}

/// Would setting `task_id.depends_on = new_deps` introduce a dependency cycle?
/// DFS from each new dep over the current `depends_on` edges with a visited
/// set; reaching `task_id` (or a direct self-dependency) closes a loop.
/// Pure + deterministic — fail-closed callers reject on `true`.
pub fn introduces_dependency_cycle(
    edges: &[(String, Vec<String>)],
    task_id: &str,
    new_deps: &[String],
) -> bool {
    if new_deps.iter().any(|d| d == task_id) {
        return true; // trivial self-dependency
    }
    use std::collections::HashMap;
    let dep_map: HashMap<&str, &[String]> = edges
        .iter()
        .map(|(id, deps)| (id.as_str(), deps.as_slice()))
        .collect();
    let mut visited: HashSet<&str> = HashSet::new();
    let mut stack: Vec<&str> = new_deps.iter().map(|s| s.as_str()).collect();
    while let Some(node) = stack.pop() {
        if node == task_id {
            return true;
        }
        if !visited.insert(node) {
            continue;
        }
        if let Some(deps) = dep_map.get(node) {
            for d in deps.iter() {
                stack.push(d.as_str());
            }
        }
    }
    false
}

/// Decide what to do with an expired-lease task given its retry state.
/// `retry_count < max_retries` ⇒ requeue (one more attempt); otherwise fail.
pub fn zombie_action(retry_count: i64, max_retries: i64) -> ZombieAction {
    if retry_count < max_retries {
        ZombieAction::Requeue
    } else {
        ZombieAction::Fail
    }
}

/// RFC-26 §4.5: would setting `child.parent = new_parent` introduce a cycle in the
/// task parent graph? Walks up from `new_parent` via the existing edges; a cycle
/// exists if the walk reaches `child` (or loops). Pure + deterministic.
///
/// `edges` is the current `(id, parent)` set. A self-parent (`child == new_parent`)
/// is a trivial cycle.
pub fn introduces_parent_cycle(
    edges: &[(String, Option<String>)],
    child: &str,
    new_parent: &str,
) -> bool {
    if child == new_parent {
        return true;
    }
    use std::collections::HashMap;
    let parent_of: HashMap<&str, Option<&str>> = edges
        .iter()
        .map(|(id, p)| (id.as_str(), p.as_deref()))
        .collect();

    // Walk ancestors of new_parent; if we hit `child`, adding the edge closes a loop.
    let mut seen = std::collections::HashSet::new();
    let mut cur = Some(new_parent);
    while let Some(node) = cur {
        if node == child {
            return true;
        }
        if !seen.insert(node) {
            // Pre-existing cycle in the data — treat as unsafe.
            return true;
        }
        cur = parent_of.get(node).copied().flatten();
    }
    false
}
