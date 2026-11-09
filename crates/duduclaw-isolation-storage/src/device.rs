//! Complete backing graph snapshots. Every dependency must be known and visited.
use std::collections::{BTreeMap, BTreeSet};
use serde::{Deserialize, Serialize};
use crate::{Error, Result};

pub const MAX_DEVICES: usize = 256;
pub const MAX_DEVICE_DEPTH: usize = 64;
pub const EXT4_MAGIC: u64 = 0xef53;
pub const XFS_MAGIC: u64 = 0x58465342;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceId { pub major: u32, pub minor: u32 }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "targets", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeviceKind { Physical, Partition, Md, DeviceMapper(Vec<String>) }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceNode {
    pub id: DeviceId,
    pub kind: DeviceKind,
    pub dependencies: Vec<DeviceId>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackingGraph {
    pub filesystem: u64,
    pub root: DeviceId,
    pub nodes: Vec<DeviceNode>,
}
impl BackingGraph {
    pub fn validate(&self) -> Result<()> {
        if !matches!(self.filesystem, EXT4_MAGIC | XFS_MAGIC)
            || self.nodes.is_empty() || self.nodes.len() > MAX_DEVICES {
            return Err(Error::UnsafeBacking);
        }
        let mut map = BTreeMap::new();
        for node in &self.nodes {
            if map.insert(node.id, node).is_some() || node.dependencies.len() > MAX_DEVICES
                || node.dependencies.iter().collect::<BTreeSet<_>>().len() != node.dependencies.len() {
                return Err(Error::UnsafeBacking);
            }
            match &node.kind {
                DeviceKind::Physical if node.dependencies.is_empty() => {},
                DeviceKind::Partition if node.dependencies.len() == 1 => {},
                DeviceKind::Md if !node.dependencies.is_empty() => {},
                DeviceKind::DeviceMapper(targets) if !node.dependencies.is_empty()
                    && !targets.is_empty() && targets.len() <= MAX_DEVICES
                    && targets.iter().all(|target| matches!(target.as_str(), "linear" | "crypt" | "striped" | "raid")) => {},
                _ => return Err(Error::UnsafeBacking),
            }
        }
        fn visit(id: DeviceId, map: &BTreeMap<DeviceId, &DeviceNode>, active: &mut BTreeSet<DeviceId>,
            done: &mut BTreeSet<DeviceId>, depth: usize) -> Result<()> {
            if depth > MAX_DEVICE_DEPTH || active.contains(&id) { return Err(Error::UnsafeBacking); }
            if done.contains(&id) { return Ok(()); }
            let node = map.get(&id).ok_or(Error::UnsafeBacking)?;
            active.insert(id);
            for dependency in &node.dependencies { visit(*dependency, map, active, done, depth + 1)?; }
            active.remove(&id);
            done.insert(id);
            Ok(())
        }
        let mut done = BTreeSet::new();
        visit(self.root, &map, &mut BTreeSet::new(), &mut done, 0)?;
        if done.len() != map.len() { return Err(Error::UnsafeBacking); }
        Ok(())
    }
    pub fn canonicalize(&mut self) {
        self.nodes.sort_by_key(|node| node.id);
        for node in &mut self.nodes { node.dependencies.sort(); }
    }
}

/// Only the target column is retained. Never persist crypt keys or table arguments.
pub fn parse_dm_targets(table: &str) -> Result<Vec<String>> {
    if table.len() > 64 * 1024 { return Err(Error::UnsafeBacking); }
    let mut targets = Vec::new();
    let mut next = 0u64;
    for line in table.lines() {
        let mut columns = line.split_ascii_whitespace();
        let start = columns.next().and_then(|v| v.parse::<u64>().ok()).ok_or(Error::UnsafeBacking)?;
        let length = columns.next().and_then(|v| v.parse::<u64>().ok()).ok_or(Error::UnsafeBacking)?;
        let target = columns.next().ok_or(Error::UnsafeBacking)?;
        if start != next || length == 0 || !matches!(target, "linear" | "crypt" | "striped" | "raid")
            || columns.next().is_none() || targets.len() == MAX_DEVICES {
            return Err(Error::UnsafeBacking);
        }
        next = start.checked_add(length).ok_or(Error::UnsafeBacking)?;
        targets.push(target.to_owned());
    }
    if targets.is_empty() { return Err(Error::UnsafeBacking); }
    Ok(targets)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    fn id(n: u32) -> DeviceId { DeviceId { major: 8, minor: n } }
    pub(crate) fn graph() -> BackingGraph {
        BackingGraph { filesystem: EXT4_MAGIC, root: id(2), nodes: vec![
            DeviceNode { id: id(2), kind: DeviceKind::DeviceMapper(vec!["crypt".into()]), dependencies: vec![id(1)] },
            DeviceNode { id: id(1), kind: DeviceKind::Partition, dependencies: vec![id(0)] },
            DeviceNode { id: id(0), kind: DeviceKind::Physical, dependencies: vec![] },
        ] }
    }
    #[test]
    fn graph_checks_every_layer_under_crypt() {
        assert!(graph().validate().is_ok());
        for forbidden in ["thin", "thin-pool", "snapshot", "cache", "era", "unknown"] {
            let mut graph = graph();
            graph.nodes[1].kind = DeviceKind::DeviceMapper(vec![forbidden.into()]);
            assert_eq!(graph.validate(), Err(Error::UnsafeBacking));
        }
    }
    #[test]
    fn graph_refuses_cycles_missing_nodes_and_unsupported_fs() {
        let mut g = graph();
        g.nodes[1].dependencies = vec![id(2)];
        assert!(g.validate().is_err());
        g = graph(); g.nodes.pop(); assert!(g.validate().is_err());
        for fs in [0x9123683e, 0x794c7630, 0x6969, 0x01021994, 0] {
            g = graph(); g.filesystem = fs; assert!(g.validate().is_err());
        }
        g = graph(); g.nodes.push(g.nodes[0].clone()); assert!(g.validate().is_err());
    }
    #[test]
    fn complete_canonical_graph_detects_dependency_and_target_changes() {
        let mut first = graph(); first.canonicalize();
        let mut reordered = graph(); reordered.nodes.reverse(); reordered.canonicalize();
        assert_eq!(first, reordered);
        let mut changed = first.clone();
        let root = changed.root;
        changed.nodes.iter_mut().find(|node| node.id == root).unwrap().kind = DeviceKind::DeviceMapper(vec!["linear".into()]);
        changed.validate().unwrap(); assert_ne!(first, changed);
        let mut changed = first.clone();
        changed.nodes.iter_mut().find(|node| node.id == id(1)).unwrap().dependencies = vec![id(3)];
        changed.nodes.iter_mut().find(|node| node.id == id(0)).unwrap().id = id(3);
        changed.canonicalize(); changed.validate().unwrap(); assert_ne!(first, changed);
    }
    #[test]
    fn dm_table_is_contiguous_allowlist_and_never_retains_keys() {
        assert_eq!(parse_dm_targets("0 100 crypt aes secret 0 8:1 0"), Ok(vec!["crypt".into()]));
        for value in ["", "0 10 thin 1", "1 10 linear 8:1 0", "0 0 linear 8:1 0",
            "0 10 linear 8:1 0\n11 10 linear 8:2 0", "0 10 crypt"] {
            assert!(parse_dm_targets(value).is_err());
        }
    }
}
