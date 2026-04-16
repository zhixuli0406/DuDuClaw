//! Dependency resolver — topological sort of hierarchical skill dependencies.
//!
//! Skills can declare `requires: [skill-a, skill-b]` in their frontmatter.
//! This module resolves transitive dependencies and detects cycles.
//!
//! Reference: SkillRL (2025-2026)

use std::collections::{HashMap, HashSet, VecDeque};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A node in the skill dependency graph.
#[derive(Debug, Clone)]
pub struct SkillNode {
    pub name: String,
    pub requires: Vec<String>,
}

/// A directed acyclic graph of skill dependencies.
#[derive(Debug, Clone)]
pub struct DependencyGraph {
    nodes: HashMap<String, SkillNode>,
}

/// Error when a dependency cycle is detected.
#[derive(Debug, Clone)]
pub struct CycleError {
    pub cycle: Vec<String>,
}

impl std::fmt::Display for CycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Dependency cycle: {}", self.cycle.join(" → "))
    }
}

// ---------------------------------------------------------------------------
// Graph construction & resolution
// ---------------------------------------------------------------------------

impl DependencyGraph {
    /// Build a dependency graph from skill nodes.
    pub fn new(skills: Vec<SkillNode>) -> Self {
        let nodes = skills.into_iter().map(|s| (s.name.clone(), s)).collect();
        Self { nodes }
    }

    /// Resolve dependencies for a set of requested skills.
    ///
    /// Returns a topologically sorted list (dependencies first) or a cycle error.
    pub fn resolve(&self, requested: &[String]) -> Result<Vec<String>, CycleError> {
        // Collect all transitive dependencies
        let mut to_visit: VecDeque<String> = requested.iter().cloned().collect();
        let mut needed: HashSet<String> = HashSet::new();

        while let Some(name) = to_visit.pop_front() {
            if needed.contains(&name) {
                continue;
            }
            needed.insert(name.clone());

            if let Some(node) = self.nodes.get(&name) {
                for dep in &node.requires {
                    if !needed.contains(dep) {
                        to_visit.push_back(dep.clone());
                    }
                }
            }
            // Missing dependencies are silently ignored (skill may not be installed)
        }

        // Topological sort (Kahn's algorithm)
        self.topological_sort(&needed)
    }

    /// Detect if there's a cycle in the graph.
    pub fn detect_cycle(&self) -> Option<Vec<String>> {
        // DFS-based cycle detection
        let mut visited = HashSet::new();
        let mut in_stack = HashSet::new();
        let mut path = Vec::new();

        for name in self.nodes.keys() {
            if !visited.contains(name) {
                if let Some(cycle) = self.dfs_cycle(name, &mut visited, &mut in_stack, &mut path) {
                    return Some(cycle);
                }
            }
        }
        None
    }

    // -- Internal helpers ---

    fn topological_sort(&self, needed: &HashSet<String>) -> Result<Vec<String>, CycleError> {
        // Build in-degree map (only for needed nodes)
        let mut in_degree: HashMap<String, usize> = HashMap::new();
        let mut adj: HashMap<String, Vec<String>> = HashMap::new();

        for name in needed {
            in_degree.entry(name.clone()).or_insert(0);
            if let Some(node) = self.nodes.get(name) {
                for dep in &node.requires {
                    if needed.contains(dep) {
                        adj.entry(dep.clone()).or_default().push(name.clone());
                        *in_degree.entry(name.clone()).or_insert(0) += 1;
                    }
                }
            }
        }

        // Kahn's algorithm
        let mut queue: VecDeque<String> = in_degree
            .iter()
            .filter(|(_, deg)| **deg == 0)
            .map(|(name, _)| name.clone())
            .collect();

        // Deterministic order for testing
        let mut queue_sorted: Vec<String> = queue.drain(..).collect();
        queue_sorted.sort();
        queue = queue_sorted.into_iter().collect();

        let mut result = Vec::new();

        while let Some(name) = queue.pop_front() {
            result.push(name.clone());

            if let Some(dependents) = adj.get(&name) {
                let mut next = Vec::new();
                for dep in dependents {
                    if let Some(deg) = in_degree.get_mut(dep) {
                        *deg -= 1;
                        if *deg == 0 {
                            next.push(dep.clone());
                        }
                    }
                }
                next.sort();
                for n in next {
                    queue.push_back(n);
                }
            }
        }

        if result.len() < needed.len() {
            // Cycle detected — use DFS to find the actual cycle path
            let remaining: Vec<String> = needed
                .iter()
                .filter(|n| !result.contains(n))
                .cloned()
                .collect();
            let cycle = if let Some(start) = remaining.first() {
                let mut visited = HashSet::new();
                let mut in_stack = HashSet::new();
                let mut path = Vec::new();
                self.dfs_cycle(start, &mut visited, &mut in_stack, &mut path)
                    .unwrap_or(remaining)
            } else {
                remaining
            };
            Err(CycleError { cycle })
        } else {
            Ok(result)
        }
    }

    fn dfs_cycle(
        &self,
        name: &str,
        visited: &mut HashSet<String>,
        in_stack: &mut HashSet<String>,
        path: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        visited.insert(name.to_string());
        in_stack.insert(name.to_string());
        path.push(name.to_string());

        if let Some(node) = self.nodes.get(name) {
            for dep in &node.requires {
                if !visited.contains(dep) {
                    if let Some(cycle) = self.dfs_cycle(dep, visited, in_stack, path) {
                        return Some(cycle);
                    }
                } else if in_stack.contains(dep) {
                    // Found cycle — extract it
                    let start = path.iter().position(|n| n == dep).unwrap_or(0);
                    let mut cycle: Vec<String> = path[start..].to_vec();
                    cycle.push(dep.clone()); // close the cycle
                    return Some(cycle);
                }
            }
        }

        path.pop();
        in_stack.remove(name);
        None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str, requires: &[&str]) -> SkillNode {
        SkillNode {
            name: name.to_string(),
            requires: requires.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn test_simple_dependency() {
        let graph = DependencyGraph::new(vec![
            node("a", &["b"]),
            node("b", &[]),
        ]);

        let resolved = graph.resolve(&["a".to_string()]).unwrap();
        assert_eq!(resolved, vec!["b", "a"]);
    }

    #[test]
    fn test_transitive_dependency() {
        let graph = DependencyGraph::new(vec![
            node("a", &["b"]),
            node("b", &["c"]),
            node("c", &[]),
        ]);

        let resolved = graph.resolve(&["a".to_string()]).unwrap();
        assert_eq!(resolved, vec!["c", "b", "a"]);
    }

    #[test]
    fn test_cycle_detected() {
        let graph = DependencyGraph::new(vec![
            node("a", &["b"]),
            node("b", &["a"]),
        ]);

        assert!(graph.resolve(&["a".to_string()]).is_err());
    }

    #[test]
    fn test_cycle_detection_dfs() {
        let graph = DependencyGraph::new(vec![
            node("a", &["b"]),
            node("b", &["c"]),
            node("c", &["a"]),
        ]);

        let cycle = graph.detect_cycle();
        assert!(cycle.is_some());
    }

    #[test]
    fn test_no_dependencies() {
        let graph = DependencyGraph::new(vec![
            node("a", &[]),
            node("b", &[]),
        ]);

        let resolved = graph.resolve(&["a".to_string(), "b".to_string()]).unwrap();
        assert_eq!(resolved.len(), 2);
        assert!(resolved.contains(&"a".to_string()));
        assert!(resolved.contains(&"b".to_string()));
    }

    #[test]
    fn test_shared_dependency_deduped() {
        let graph = DependencyGraph::new(vec![
            node("a", &["c"]),
            node("b", &["c"]),
            node("c", &[]),
        ]);

        let resolved = graph.resolve(&["a".to_string(), "b".to_string()]).unwrap();
        assert_eq!(resolved.len(), 3);
        // c should appear exactly once, before both a and b
        let c_pos = resolved.iter().position(|n| n == "c").unwrap();
        let a_pos = resolved.iter().position(|n| n == "a").unwrap();
        let b_pos = resolved.iter().position(|n| n == "b").unwrap();
        assert!(c_pos < a_pos);
        assert!(c_pos < b_pos);
    }

    #[test]
    fn test_missing_dependency_ignored() {
        let graph = DependencyGraph::new(vec![
            node("a", &["missing"]),
        ]);

        // "missing" is not in the graph — should resolve without error
        let resolved = graph.resolve(&["a".to_string()]).unwrap();
        assert!(resolved.contains(&"a".to_string()));
    }

    #[test]
    fn test_no_cycle_in_dag() {
        let graph = DependencyGraph::new(vec![
            node("a", &["b", "c"]),
            node("b", &["d"]),
            node("c", &["d"]),
            node("d", &[]),
        ]);

        assert!(graph.detect_cycle().is_none());
    }
}
