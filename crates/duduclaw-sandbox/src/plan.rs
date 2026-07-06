//! Pure, OS-agnostic Landlock *plan*.
//!
//! Splitting the plan from the Linux-only application keeps the decision logic
//! (which paths are read-only vs read-write, whether to deny network) unit-
//! testable on any host — including the macOS dev box where the real `landlock`
//! crate is not even compiled.

use crate::SandboxSpec;
use std::path::PathBuf;

/// A resolved plan for a Landlock ruleset, derived deterministically from a
/// [`SandboxSpec`]. The Linux module turns this into real `landlock` calls;
/// tests assert on it directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandlockPlan {
    /// Subtrees granted read-only filesystem access.
    pub read_only: Vec<PathBuf>,
    /// Subtrees granted read+write filesystem access.
    pub read_write: Vec<PathBuf>,
    /// When `true`, the ruleset handles (and therefore denies, since no port
    /// rule is added) outbound TCP bind/connect. When `false`, network is left
    /// unrestricted by Landlock (delegated to the container / egress layer).
    pub deny_network: bool,
}

impl LandlockPlan {
    /// Build a plan from a spec.
    ///
    /// `writable_paths` are granted read+write; `readable_paths` that are not
    /// already writable are granted read-only. `deny_network` is the inverse of
    /// `spec.allow_network`.
    pub fn from_spec(spec: &SandboxSpec) -> LandlockPlan {
        let read_write: Vec<PathBuf> = spec.writable_paths.clone();
        // A path that is writable is implicitly readable; do not list it twice.
        let read_only: Vec<PathBuf> = spec
            .readable_paths
            .iter()
            .filter(|p| !read_write.contains(p))
            .cloned()
            .collect();
        LandlockPlan {
            read_only,
            read_write,
            deny_network: !spec.allow_network,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SandboxLevel;
    use std::path::Path;

    #[test]
    fn read_only_level_yields_no_write_paths_and_reads_root() {
        let spec = SandboxSpec::from_level(SandboxLevel::ReadOnly, Path::new("/agents/bob"));
        let plan = LandlockPlan::from_spec(&spec);
        assert!(plan.read_write.is_empty());
        assert_eq!(plan.read_only, vec![PathBuf::from("/")]);
    }

    #[test]
    fn workspace_write_grants_rw_to_agent_dir() {
        let agent = Path::new("/agents/bob");
        let spec = SandboxSpec::from_level(SandboxLevel::WorkspaceWrite, agent);
        let plan = LandlockPlan::from_spec(&spec);
        assert!(plan.read_write.contains(&agent.to_path_buf()));
    }

    #[test]
    fn writable_path_is_not_duplicated_in_read_only() {
        // A path present in both readable and writable must appear only in RW.
        let agent = PathBuf::from("/agents/bob");
        let spec = SandboxSpec {
            readable_paths: vec![agent.clone(), PathBuf::from("/usr")],
            writable_paths: vec![agent.clone()],
            allow_network: true,
            unconfined: false,
        };
        let plan = LandlockPlan::from_spec(&spec);
        assert!(plan.read_write.contains(&agent));
        assert!(!plan.read_only.contains(&agent));
        assert!(plan.read_only.contains(&PathBuf::from("/usr")));
    }

    #[test]
    fn deny_network_is_inverse_of_allow_network() {
        let mut spec = SandboxSpec::from_level(SandboxLevel::WorkspaceWrite, Path::new("/a"));
        spec.allow_network = false;
        assert!(LandlockPlan::from_spec(&spec).deny_network);
        spec.allow_network = true;
        assert!(!LandlockPlan::from_spec(&spec).deny_network);
    }
}
