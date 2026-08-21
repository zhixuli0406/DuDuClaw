// WP-S5b3-H (S5b 第三波, 2026-08-21) — data model + tree-flattening for the
// "組織架構" page (`screens/org.rs`, 方案 A 縮排階層清單 — B/C alternatives
// rejected by the user's own 2026-08-21 拍板, per this task's brief).
//
// ── Data source (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed) ────────────────────────────────────────────
//   `agents.list {}` (dispatch same RPC `screens::agents_data::
//   parse_agents_list` already reads) → `{"agents": [...]}`, each row
//   shaped by `handle_agents_list_filtered` (~L25692) — this page needs
//   `reports_to` (~L25738: `cfg.agent.reports_to`, a plain `String`, empty
//   = no parent), which `agents_data::AgentListItem` does NOT carry (that
//   struct is a shared cross-page type this task's own boundary leaves
//   alone, per `screens/agents.rs`'s own "local copy over widened
//   visibility" precedent already documented on that module's `role_label`)
//   — so this module parses the raw payload itself instead of extending
//   that shared struct. `role`/`status` are BOTH `format!("{:?}", enum).
//   to_lowercase()` (same wire shape `agents_data.rs`'s header comment
//   documents) — `"main"|"specialist"|"worker"|...` / `"active"|"paused"|
//   "terminated"`.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgAgentRow {
    pub id: String,
    pub display_name: String,
    pub role: String,
    pub department: String,
    pub status: String,
    pub icon: Option<String>,
    /// Empty = no parent (a root). `agents.toml`'s own field is a plain
    /// `String`, never `Option` — matches `AgentListItem::department`'s
    /// identical "empty IS unset" convention.
    pub reports_to: String,
}

pub fn parse_org_agents(v: &Value) -> Vec<OrgAgentRow> {
    v.get("agents")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    let id = a.get("name")?.as_str()?.to_string();
                    let display_name = a
                        .get("display_name")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| id.clone());
                    let role = a.get("role").and_then(Value::as_str).unwrap_or("").to_string();
                    let department = a.get("department").and_then(Value::as_str).unwrap_or("").to_string();
                    let status = a.get("status").and_then(Value::as_str).unwrap_or("active").to_string();
                    let icon = a.get("icon").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
                    let reports_to = a.get("reports_to").and_then(Value::as_str).unwrap_or("").to_string();
                    Some(OrgAgentRow { id, display_name, role, department, status, icon, reports_to })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One flattened row of the indented list — `depth` drives the 26px-per-
/// level indent the approved canvas (`OrgIndented.dc.html`) itself uses;
/// `has_children` decides whether the disclosure triangle renders at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgNode {
    pub agent: OrgAgentRow,
    pub depth: usize,
    pub has_children: bool,
}

/// Builds the `reports_to` tree and flattens it into render order (depth-
/// first, `main`-role roots first, every other level preserving
/// `agents.list`'s own row order) — collapsed subtrees (`collapsed` holds
/// agent ids whose children are hidden, pure client UI state per the task
/// brief's "純 UI 態") are skipped during the walk but the collapsed node
/// itself still renders with `has_children: true` so its disclosure
/// triangle stays visible.
///
/// Defensive against data an operator could hand-edit into a bad shape: a
/// `reports_to` pointing at a non-existent id, at itself, or forming a pure
/// cycle disconnected from every root — none of these ever silently drop an
/// agent from the list (see the trailing orphan pass), matching this
/// crate's "empty result over fabricated result, never a silent gap"
/// honesty convention.
pub fn flatten_tree(agents: &[OrgAgentRow], collapsed: &HashSet<String>) -> Vec<OrgNode> {
    let mut seen_ids: HashSet<&str> = HashSet::new();
    let mut dedup: Vec<&OrgAgentRow> = Vec::new();
    for a in agents {
        if seen_ids.insert(a.id.as_str()) {
            dedup.push(a);
        }
    }
    let by_id: HashMap<&str, &OrgAgentRow> = dedup.iter().map(|a| (a.id.as_str(), *a)).collect();

    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut roots: Vec<&str> = Vec::new();
    for a in &dedup {
        let parent = a.reports_to.trim();
        if !parent.is_empty() && parent != a.id.as_str() && by_id.contains_key(parent) {
            children.entry(parent).or_default().push(a.id.as_str());
        } else {
            roots.push(a.id.as_str());
        }
    }
    // Stable sort: `main` role first, everyone else keeps `agents.list`'s
    // own relative order (matches `OrgIndented.dc.html`'s own 小杜-first
    // rendering — the mockup's single `主要` row leads its siblings).
    roots.sort_by_key(|id| if by_id[id].role == "main" { 0 } else { 1 });

    // Phase 1: full tree reachability from every root, IGNORING `collapsed`
    // — this defines which ids are genuinely orphaned (a `reports_to` cycle
    // disconnected from any root), as opposed to merely hidden behind a
    // collapsed ancestor. Collapsing a node must never make its subtree look
    // like data-integrity orphans and reappear as fake top-level rows.
    let mut reachable: HashSet<&str> = HashSet::new();
    let mut reach_stack: Vec<&str> = roots.clone();
    while let Some(id) = reach_stack.pop() {
        if !reachable.insert(id) {
            continue; // cycle guard
        }
        if let Some(kids) = children.get(id) {
            reach_stack.extend(kids.iter().copied());
        }
    }

    // Phase 2: depth-first emission that DOES respect `collapsed`.
    let mut out: Vec<OrgNode> = Vec::new();
    let mut emitted: HashSet<&str> = HashSet::new();
    let mut stack: Vec<(&str, usize)> = roots.into_iter().rev().map(|id| (id, 0)).collect();
    while let Some((id, depth)) = stack.pop() {
        if !emitted.insert(id) {
            continue; // cycle guard
        }
        let agent = by_id[id];
        let kids = children.get(id).cloned().unwrap_or_default();
        out.push(OrgNode { agent: agent.clone(), depth, has_children: !kids.is_empty() });
        if !collapsed.contains(id) {
            for kid in kids.into_iter().rev() {
                stack.push((kid, depth + 1));
            }
        }
    }
    // Orphan pass: agents whose `reports_to` chain forms a pure cycle never
    // reachable from any root (per phase 1, NOT per what phase 2 happened to
    // emit while a collapse was active). Rendered as their own top-level
    // rows rather than vanishing.
    for a in &dedup {
        if !reachable.contains(a.id.as_str()) {
            out.push(OrgNode { agent: (*a).clone(), depth: 0, has_children: false });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn agent(id: &str, role: &str, reports_to: &str) -> OrgAgentRow {
        OrgAgentRow {
            id: id.into(),
            display_name: id.into(),
            role: role.into(),
            department: String::new(),
            status: "active".into(),
            icon: None,
            reports_to: reports_to.into(),
        }
    }

    #[test]
    fn parse_org_agents_reads_reports_to() {
        let v = json!({ "agents": [{"name":"a1","display_name":"小杜","role":"main","status":"active","reports_to":""}] });
        let rows = parse_org_agents(&v);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].reports_to, "");
        assert_eq!(rows[0].role, "main");
    }

    #[test]
    fn flatten_tree_orders_main_root_first_and_indents_children() {
        let agents = vec![
            agent("worker1", "worker", "specialist1"),
            agent("specialist1", "specialist", "main1"),
            agent("main1", "main", ""),
        ];
        let nodes = flatten_tree(&agents, &HashSet::new());
        let ids: Vec<&str> = nodes.iter().map(|n| n.agent.id.as_str()).collect();
        assert_eq!(ids, vec!["main1", "specialist1", "worker1"]);
        assert_eq!(nodes[0].depth, 0);
        assert_eq!(nodes[1].depth, 1);
        assert_eq!(nodes[2].depth, 2);
        assert!(nodes[0].has_children);
        assert!(nodes[1].has_children);
        assert!(!nodes[2].has_children);
    }

    #[test]
    fn flatten_tree_collapsed_node_hides_children_but_keeps_disclosure() {
        let agents = vec![agent("child1", "specialist", "main1"), agent("main1", "main", "")];
        let mut collapsed = HashSet::new();
        collapsed.insert("main1".to_string());
        let nodes = flatten_tree(&agents, &collapsed);
        let ids: Vec<&str> = nodes.iter().map(|n| n.agent.id.as_str()).collect();
        assert_eq!(ids, vec!["main1"]);
        assert!(nodes[0].has_children);
    }

    #[test]
    fn flatten_tree_unknown_parent_becomes_a_root_not_dropped() {
        let agents = vec![agent("orphan", "worker", "does-not-exist")];
        let nodes = flatten_tree(&agents, &HashSet::new());
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].depth, 0);
    }

    #[test]
    fn flatten_tree_self_report_becomes_a_root_not_an_infinite_loop() {
        let agents = vec![agent("loopy", "worker", "loopy")];
        let nodes = flatten_tree(&agents, &HashSet::new());
        assert_eq!(nodes.len(), 1);
    }

    #[test]
    fn flatten_tree_pure_cycle_disconnected_from_any_root_still_renders_every_agent() {
        // a -> b -> a, neither ever reachable from a root.
        let agents = vec![agent("a", "specialist", "b"), agent("b", "specialist", "a")];
        let nodes = flatten_tree(&agents, &HashSet::new());
        let ids: HashSet<&str> = nodes.iter().map(|n| n.agent.id.as_str()).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains("a"));
        assert!(ids.contains("b"));
    }

    #[test]
    fn flatten_tree_duplicate_id_rows_keep_only_the_first() {
        let agents = vec![agent("dup", "worker", ""), agent("dup", "main", "")];
        let nodes = flatten_tree(&agents, &HashSet::new());
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].agent.role, "worker");
    }
}
