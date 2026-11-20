//! Root trampoline, native Linux isolation package B2 (design r7 §4).
//! This crate never accepts a gateway-selected execution policy.
pub mod error;
pub mod fd_plan;
pub mod state;
pub mod validation;
pub mod seccomp;
pub mod landlock;
pub mod status;
pub mod helper_policy;
pub mod helper_wire;
pub use error::{Error, Result};
#[cfg(target_os = "linux")]
pub mod linux;

/// Directories the trampoline requires inside spawnd's read-only `/etc`
/// projection, relative to its root, parents first; each entry extends the
/// previous one by exactly one component. spawnd publishes exactly these (root
/// 0755, each holding only the next one, the last one EMPTY) and its exact-shape
/// verifier accepts nothing else; `linux::mounts::build_root` attaches the host
/// CA certificates on [`ETC_CA_MOUNT_POINT`] and never creates anything inside
/// the projection. One list for both crates, so producer and consumer cannot drift.
pub const ETC_PROJECTION_DIRECTORIES: [&str; 2] = ["ssl", "ssl/certs"];
/// Mount point of the host CA certificate clone inside the `/etc` projection.
pub const ETC_CA_MOUNT_POINT: &str = ETC_PROJECTION_DIRECTORIES[ETC_PROJECTION_DIRECTORIES.len() - 1];

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn etc_projection_directories_are_a_chain_ending_at_the_ca_mount_point() {
        assert_eq!(ETC_CA_MOUNT_POINT, "ssl/certs");
        assert_eq!(ETC_PROJECTION_DIRECTORIES.last(), Some(&ETC_CA_MOUNT_POINT));
        let mut previous: Option<&str> = None;
        for path in ETC_PROJECTION_DIRECTORIES {
            assert!(!path.starts_with('/') && !path.ends_with('/'));
            assert!(path.split('/').all(|part| !part.is_empty() && part != "." && part != ".."));
            match previous {
                None => assert!(!path.contains('/')),
                Some(parent) => assert_eq!(path.rsplit_once('/').map(|(prefix, _)| prefix), Some(parent)),
            }
            previous = Some(path);
        }
    }
}
