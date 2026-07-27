//! Deterministic `reports_to` topological sort (parents before children).
//!
//! Kept local to the expert module (rather than reusing the `migrate_from`
//! helper) so the two importers stay independently editable.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Outcome of a topological sort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopoOutcome {
    /// Agents ordered parents-before-children (safe creation order).
    Sorted(Vec<String>),
    /// A cycle exists; carries the ids still stuck. Caller creates all agents
    /// with an empty `reports_to` and marks the run PARTIAL.
    Cycle(Vec<String>),
}

/// Topologically sort `(id, reports_to)` nodes. A `reports_to` that points
/// outside the set (or is `None`) is treated as a root. Ties break in input
/// order for reproducibility.
pub fn topo_sort(nodes: &[(String, Option<String>)]) -> TopoOutcome {
    let ids: BTreeSet<&str> = nodes.iter().map(|(id, _)| id.as_str()).collect();
    let index_of: BTreeMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (id.as_str(), i))
        .collect();

    let mut indeg = vec![0usize; nodes.len()];
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];

    for (i, (_id, rt)) in nodes.iter().enumerate() {
        if let Some(parent) = rt
            && parent != &nodes[i].0
            && ids.contains(parent.as_str())
        {
            let p = index_of[parent.as_str()];
            indeg[i] += 1;
            children[p].push(i);
        }
    }

    let mut queue: VecDeque<usize> = (0..nodes.len()).filter(|&i| indeg[i] == 0).collect();
    let mut order: Vec<String> = Vec::with_capacity(nodes.len());
    while let Some(i) = queue.pop_front() {
        order.push(nodes[i].0.clone());
        for &c in &children[i] {
            indeg[c] -= 1;
            if indeg[c] == 0 {
                queue.push_back(c);
            }
        }
    }

    if order.len() == nodes.len() {
        TopoOutcome::Sorted(order)
    } else {
        let stuck: Vec<String> = nodes
            .iter()
            .enumerate()
            .filter(|(i, _)| indeg[*i] > 0)
            .map(|(_, (id, _))| id.clone())
            .collect();
        TopoOutcome::Cycle(stuck)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(id: &str, rt: Option<&str>) -> (String, Option<String>) {
        (id.to_string(), rt.map(|s| s.to_string()))
    }

    #[test]
    fn parents_precede_children() {
        let out = topo_sort(&[n("c", Some("a")), n("a", None), n("b", Some("a"))]);
        match out {
            TopoOutcome::Sorted(order) => {
                let pos = |x: &str| order.iter().position(|y| y == x).unwrap();
                assert!(pos("a") < pos("b"));
                assert!(pos("a") < pos("c"));
            }
            TopoOutcome::Cycle(_) => panic!("unexpected cycle"),
        }
    }

    #[test]
    fn detects_cycle() {
        let out = topo_sort(&[n("a", Some("b")), n("b", Some("a"))]);
        assert!(matches!(out, TopoOutcome::Cycle(_)));
    }
}
