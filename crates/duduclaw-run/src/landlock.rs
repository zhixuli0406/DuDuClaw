//! Filesystem policy shared by namespace init and all runtime children (§4.2, D8).
//! Persisted directories never permit creation or linking of sockets/FIFOs/devices.
//! Landlock ABI 1..7 does not restrict connecting to existing pathname sockets;
//! spawnd's single-run admission and special-file-free import remain mandatory.

pub const EXECUTE: u64 = 1 << 0;
pub const WRITE_FILE: u64 = 1 << 1;
pub const READ_FILE: u64 = 1 << 2;
pub const READ_DIR: u64 = 1 << 3;
pub const REMOVE_DIR: u64 = 1 << 4;
pub const REMOVE_FILE: u64 = 1 << 5;
pub const MAKE_CHAR: u64 = 1 << 6;
pub const MAKE_DIR: u64 = 1 << 7;
pub const MAKE_REG: u64 = 1 << 8;
pub const MAKE_SOCK: u64 = 1 << 9;
pub const MAKE_FIFO: u64 = 1 << 10;
pub const MAKE_BLOCK: u64 = 1 << 11;
pub const MAKE_SYM: u64 = 1 << 12;
pub const REFER: u64 = 1 << 13;
pub const TRUNCATE: u64 = 1 << 14;
pub const IOCTL_DEV: u64 = 1 << 15;
pub const MIN_ABI: u32 = 1;
pub const MAX_KNOWN_ABI: u32 = 7;
const READ: u64 = READ_FILE | READ_DIR;
const CREATE_ORDINARY: u64 = MAKE_DIR | MAKE_REG | MAKE_SYM;
const WRITE_ORDINARY: u64 = WRITE_FILE | REMOVE_DIR | REMOVE_FILE | CREATE_ORDINARY | REFER | TRUNCATE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathClass {
    /// System libraries/binaries and the root-published runtime registry.
    ReadExecute,
    /// Minimal /etc, configuration projections and /proc.
    ReadOnly,
    /// Employee home and work. Mount NOEXEC can independently narrow execution.
    Persistent,
    /// Per-run /tmp, /run and /dev/shm, never shared between runs.
    Ephemeral,
    /// Existing /dev nodes and devpts; never permits mknod or regular-file creation.
    Devices,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("required Landlock ABI unavailable")] UnsupportedAbi,
    #[error("invalid Landlock policy")] InvalidPolicy,
    #[error("Landlock enforcement failed")] Enforcement,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy { abi: u32, pub handled: u64 }
impl Policy {
    /// All filesystem rights understood by the pinned landlock 0.4.5 dependency.
    /// Newer kernels enforce the same known ruleset. Unknown additional rights
    /// are not claimed as enforced; the minimum required ABI remains mandatory.
    pub fn for_abi(detected_abi: u32) -> Result<Self, PolicyError> {
        if detected_abi < MIN_ABI { return Err(PolicyError::UnsupportedAbi); }
        let abi = detected_abi.min(MAX_KNOWN_ABI);
        let handled = match abi {
            1 => (1 << 13) - 1,
            2 => (1 << 14) - 1,
            3 | 4 => (1 << 15) - 1,
            5..=7 => (1 << 16) - 1,
            _ => return Err(PolicyError::UnsupportedAbi),
        };
        Ok(Self { abi, handled })
    }
    /// ABI of the selected policy, not necessarily the kernel's detected ABI.
    pub fn abi(&self) -> u32 { self.abi }
    pub fn rights(&self, class: PathClass) -> u64 {
        let rights = match class {
            PathClass::ReadExecute => READ | EXECUTE,
            PathClass::ReadOnly => READ,
            PathClass::Persistent => READ | EXECUTE | WRITE_ORDINARY,
            PathClass::Ephemeral => READ | EXECUTE | WRITE_ORDINARY | MAKE_SOCK | MAKE_FIFO,
            PathClass::Devices => READ | WRITE_FILE | IOCTL_DEV,
        };
        rights & self.handled
    }
}

/// Probe the kernel API, never infer ABI from distribution or uname.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub fn probe_abi() -> Result<u32, PolicyError> {
    // SAFETY: the VERSION query explicitly requires a null attr pointer and size 0.
    let version = unsafe { libc::syscall(libc::SYS_landlock_create_ruleset,
        std::ptr::null::<u8>(), 0usize, 1u32) };
    let abi = u32::try_from(version).map_err(|_| PolicyError::UnsupportedAbi)?;
    Policy::for_abi(abi)?;
    Ok(abi)
}

/// Caller supplies already validated mount-root O_PATH descriptors, after pivot.
/// The complete required root set is selected by the trampoline, never by runtime
/// input. Borrowed descriptors stay owned by the caller and must close before exec.
#[cfg(target_os = "linux")]
pub fn restrict(policy: &Policy,
    roots: &[(std::os::fd::BorrowedFd<'_>, PathClass)]) -> Result<(), PolicyError> {
    use ::landlock::{AccessFs, BitFlags, CompatLevel, Compatible, PathBeneath,
        Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus};
    if roots.is_empty() || roots.len() > 32 || *policy != Policy::for_abi(policy.abi)?
        || *policy != Policy::for_abi(probe_abi()?)? {
        return Err(PolicyError::InvalidPolicy);
    }
    let handled = BitFlags::<AccessFs>::from_bits(policy.handled)
        .map_err(|_| PolicyError::InvalidPolicy)?;
    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(handled).map_err(|_| PolicyError::Enforcement)?
        .create().map_err(|_| PolicyError::Enforcement)?;
    for (fd, class) in roots {
        let rights = BitFlags::<AccessFs>::from_bits(policy.rights(*class))
            .map_err(|_| PolicyError::InvalidPolicy)?;
        ruleset = ruleset.add_rule(PathBeneath::new(*fd, rights))
            .map_err(|_| PolicyError::Enforcement)?;
    }
    let status = ruleset.restrict_self().map_err(|_| PolicyError::Enforcement)?;
    if status.ruleset != RulesetStatus::FullyEnforced || !status.no_new_privs {
        return Err(PolicyError::Enforcement);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn landlock_abi_derivation_handles_every_supported_fs_right() {
        let expected = [(1,0x1fff),(2,0x3fff),(3,0x7fff),(4,0x7fff),(5,0xffff),(6,0xffff),(7,0xffff)];
        for (abi,handled) in expected {
            assert_eq!(Policy::for_abi(abi).unwrap().handled,handled);
        }
        assert!(Policy::for_abi(0).is_err());
    }
    #[test]
    fn landlock_newer_kernel_uses_exact_abi_seven_policy() {
        let known = Policy::for_abi(MAX_KNOWN_ABI).unwrap();
        for abi in [8, 9, 12, u32::MAX] {
            let policy = Policy::for_abi(abi).unwrap();
            assert_eq!(policy, known);
            assert_eq!(policy.abi(), MAX_KNOWN_ABI);
            for class in [PathClass::ReadOnly, PathClass::ReadExecute, PathClass::Persistent,
                PathClass::Ephemeral, PathClass::Devices] {
                assert_eq!(policy.rights(class), known.rights(class));
            }
        }
    }
    #[test]
    fn landlock_d8_persistent_socket_fifo_and_device_creation_are_denied() {
        for abi in 1..=12 {
            let policy=Policy::for_abi(abi).unwrap();
            let persistent=policy.rights(PathClass::Persistent);
            assert_eq!(persistent & (MAKE_SOCK|MAKE_FIFO|MAKE_CHAR|MAKE_BLOCK|IOCTL_DEV),0);
            assert_eq!(policy.handled & (MAKE_SOCK|MAKE_FIFO|MAKE_CHAR|MAKE_BLOCK),MAKE_SOCK|MAKE_FIFO|MAKE_CHAR|MAKE_BLOCK);
            assert_eq!(persistent & (READ|EXECUTE|WRITE_FILE|CREATE_ORDINARY),READ|EXECUTE|WRITE_FILE|CREATE_ORDINARY);
            assert_eq!(persistent & REFER,if abi>=2 { REFER } else { 0 });
            assert_eq!(persistent & TRUNCATE,if abi>=3 { TRUNCATE } else { 0 });
        }
    }
    #[test]
    fn landlock_ephemeral_and_readonly_rules_are_separate() {
        for abi in 1..=12 {
            let policy=Policy::for_abi(abi).unwrap();
            assert_eq!(policy.rights(PathClass::ReadOnly),READ);
            assert_eq!(policy.rights(PathClass::ReadExecute),READ|EXECUTE);
            assert_eq!(policy.rights(PathClass::Ephemeral)&(MAKE_SOCK|MAKE_FIFO),MAKE_SOCK|MAKE_FIFO);
            assert_eq!(policy.rights(PathClass::Ephemeral)&(MAKE_CHAR|MAKE_BLOCK),0);
            assert_eq!(policy.rights(PathClass::Devices),READ|WRITE_FILE|if abi>=5 { IOCTL_DEV } else { 0 });
            for class in [PathClass::ReadOnly,PathClass::ReadExecute,PathClass::Persistent,PathClass::Ephemeral,PathClass::Devices] {
                assert_eq!(policy.rights(class)&!policy.handled,0);
            }
        }
    }
    #[cfg(target_os="linux")]
    #[test]
    fn landlock_pure_rights_match_pinned_crate_uapi() {
        use ::landlock::{Access,AccessFs,ABI};
        for (version,abi) in [(1,ABI::V1),(2,ABI::V2),(3,ABI::V3),(4,ABI::V4),(5,ABI::V5),(6,ABI::V6),(7,ABI::V7)] {
            assert_eq!(Policy::for_abi(version).unwrap().handled,AccessFs::from_all(abi).bits());
        }
    }
}
