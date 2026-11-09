//! Fixed-capacity root-owned employee storage (design r7 §4.3, R4-M6/R5-M4).
//! B1 supplies admission, namespace privacy and absence of active mount clones.
pub mod accounting;
pub mod device;
pub mod metadata;
#[cfg(any(target_os = "linux", test))]
mod ext4;

use duduclaw_isolation::{execution::DiskQuota, ids::EmployeeId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("storage is unsupported on this host")] Unsupported,
    #[error("storage input is invalid")] Invalid,
    #[error("storage capacity reserve would be violated")] Capacity,
    #[error("backing storage cannot guarantee allocation")] UnsafeBacking,
    #[error("storage state requires operator recovery")] RecoveryRequired,
    #[error("storage is busy")] Busy,
    #[error("employee storage was not found")] NotFound,
    #[error("employee storage is quarantined")] Quarantined,
    #[error("quota shrink or inode-only resize is unsupported")] ResizeUnsupported,
    #[error("storage I/O failed")] Io,
}
pub type Result<T> = std::result::Result<T, Error>;

/// Minimum size supports ext4 with a journal. Exact inode count is read back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quota { pub bytes: u64, pub inodes: u64 }
impl Quota {
    pub fn validate(self) -> Result<Self> {
        if self.bytes < 64 * 1024 * 1024 || self.bytes > i64::MAX as u64
            || self.bytes % 4096 != 0 || self.inodes < 128 || self.inodes > u32::MAX as u64
            || self.inodes > self.bytes / 4096 {
            return Err(Error::Invalid);
        }
        Ok(self)
    }
}
impl TryFrom<&DiskQuota> for Quota {
    type Error = Error;
    fn try_from(value: &DiskQuota) -> Result<Self> {
        match value {
            DiskQuota::LoopFilesystem { bytes, inodes } => Self { bytes: *bytes, inodes: *inodes }.validate(),
            DiskQuota::ProjectQuota { .. } => Err(Error::Unsupported),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub capacity_bytes: u64,
    pub used_bytes: u64,
    pub capacity_inodes: u64,
    pub used_inodes: u64,
}

/// No descriptor here refers to an image or loop block device.
#[derive(Debug)]
pub struct MountAnchor { pub path: std::path::PathBuf, pub noexec: bool }

/// Root-local inventory for B1 crash reconciliation. Mount identity is read
/// from the kernel and checked against the registered image, never a pathname.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageStatus {
    pub employee: EmployeeId,
    pub uid: u32,
    pub quota: Quota,
    pub phase: metadata::Phase,
    pub mounted_device: Option<device::DeviceId>,
}

/// Root-local B1 seam. No method authorizes a gateway request. B1 must serialize
/// per-employee admission, drain runs/clones before resize/detach/destroy, and
/// call readiness at boot, before EVERY run and on its monitoring timer.
/// Implementations must return errors, never use an unbounded fallback directory.
pub trait EmployeeStorage: Send {
    fn readiness(&self) -> Result<()>;
    fn images(&self) -> Result<Vec<ImageStatus>> { Err(Error::Unsupported) }
    fn provision(&mut self, employee: EmployeeId, uid: u32, quota: Quota) -> Result<()>;
    fn attach(&mut self, employee: EmployeeId, noexec: bool) -> Result<MountAnchor>;
    fn detach(&mut self, employee: EmployeeId) -> Result<()>;
    fn destroy(&mut self, employee: EmployeeId) -> Result<()>;
    fn usage(&self, employee: EmployeeId) -> Result<Usage>;
    fn resize(&mut self, employee: EmployeeId, quota: Quota) -> Result<()>;
}

impl EmployeeStorage for Storage {
    fn readiness(&self) -> Result<()> { Storage::readiness(self) }
    fn images(&self) -> Result<Vec<ImageStatus>> { Storage::images(self) }
    fn provision(&mut self, employee: EmployeeId, uid: u32, quota: Quota) -> Result<()> { Storage::provision(self, employee, uid, quota) }
    fn attach(&mut self, employee: EmployeeId, noexec: bool) -> Result<MountAnchor> { Storage::attach(self, employee, noexec) }
    fn detach(&mut self, employee: EmployeeId) -> Result<()> { Storage::detach(self, employee) }
    fn destroy(&mut self, employee: EmployeeId) -> Result<()> { Storage::destroy(self, employee) }
    fn usage(&self, employee: EmployeeId) -> Result<Usage> { Storage::usage(self, employee) }
    fn resize(&mut self, employee: EmployeeId, quota: Quota) -> Result<()> { Storage::resize(self, employee, quota) }
}

/// Only monotonic byte/inode ceilings can grow in place. Inode count is a
/// ceiling, not a promise to allocate that many; kernel readback is mandatory.
#[cfg(any(target_os = "linux", test))]
fn growth_delta(current: Quota, target: Quota) -> Result<u64> {
    current.validate()?; target.validate()?;
    if current == target { return Ok(0); }
    if target.bytes <= current.bytes || target.inodes < current.inodes { return Err(Error::ResizeUnsupported); }
    Ok(target.bytes - current.bytes)
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::Storage;

#[cfg(not(target_os = "linux"))]
pub struct Storage;
#[cfg(not(target_os = "linux"))]
impl Storage {
    pub fn open(_: impl AsRef<std::path::Path>) -> Result<Self> { Err(Error::Unsupported) }
    pub fn provision(&mut self, _: EmployeeId, _: u32, _: Quota) -> Result<()> { Err(Error::Unsupported) }
    pub fn attach(&mut self, _: EmployeeId, _: bool) -> Result<MountAnchor> { Err(Error::Unsupported) }
    pub fn detach(&mut self, _: EmployeeId) -> Result<()> { Err(Error::Unsupported) }
    pub fn destroy(&mut self, _: EmployeeId) -> Result<()> { Err(Error::Unsupported) }
    pub fn usage(&self, _: EmployeeId) -> Result<Usage> { Err(Error::Unsupported) }
    pub fn readiness(&self) -> Result<()> { Err(Error::Unsupported) }
    pub fn images(&self) -> Result<Vec<ImageStatus>> { Err(Error::Unsupported) }
    pub fn resize(&mut self, _: EmployeeId, _: Quota) -> Result<()> { Err(Error::Unsupported) }
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn employee_key(employee: EmployeeId) -> String {
    employee.0.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quota_rejects_unbounded_and_project_quota() {
        assert!(Quota { bytes: 64 * 1024 * 1024, inodes: 1024 }.validate().is_ok());
        for quota in [Quota { bytes: 0, inodes: 1 }, Quota { bytes: u64::MAX, inodes: 1024 },
            Quota { bytes: 64 * 1024 * 1024 + 1, inodes: 1024 },
            Quota { bytes: 64 * 1024 * 1024, inodes: 0 },
            Quota { bytes: 64 * 1024 * 1024, inodes: u64::MAX }] {
            assert!(quota.validate().is_err());
        }
        assert_eq!(Quota::try_from(&DiskQuota::ProjectQuota { bytes: 1, inodes: 1 }), Err(Error::Unsupported));
        assert_eq!(employee_key(EmployeeId([255; 16])), "ff".repeat(16));
    }
    #[test]
    fn resize_policy_refuses_shrink_and_inode_only_changes() {
        let old = Quota { bytes: 128 * 1024 * 1024, inodes: 1024 };
        assert_eq!(growth_delta(old, old), Ok(0));
        assert_eq!(growth_delta(old, Quota { bytes: old.bytes * 2, inodes: 2048 }), Ok(old.bytes));
        for target in [Quota { bytes: old.bytes / 2, ..old }, Quota { inodes: 2048, ..old },
            Quota { bytes: old.bytes * 2, inodes: 512 }] {
            assert_eq!(growth_delta(old, target), Err(Error::ResizeUnsupported));
        }
        fn accepts_seam(_: &mut dyn EmployeeStorage) {}
        let _ = accepts_seam;
    }
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn unsupported_host_never_provisions() {
        assert!(matches!(Storage::open("/ignored"), Err(Error::Unsupported)));
        let storage: &mut dyn EmployeeStorage = &mut Storage;
        let employee = EmployeeId([1; 16]);
        let quota = Quota { bytes: 64 * 1024 * 1024, inodes: 1024 };
        assert_eq!(storage.readiness(), Err(Error::Unsupported));
        assert_eq!(storage.images(), Err(Error::Unsupported));
        assert_eq!(storage.provision(employee, 70000, quota), Err(Error::Unsupported));
        assert_eq!(storage.attach(employee, true).unwrap_err(), Error::Unsupported);
        assert_eq!(storage.detach(employee), Err(Error::Unsupported));
        assert_eq!(storage.destroy(employee), Err(Error::Unsupported));
        assert_eq!(storage.usage(employee), Err(Error::Unsupported));
        assert_eq!(storage.resize(employee, quota), Err(Error::Unsupported));
    }
}
