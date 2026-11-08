//! Package H integration boundary. No host path or image fd crosses this API.
use duduclaw_isolation::{execution::{DiskQuota, RunHandle}, ids::{EmployeeId, RequestId, Sha256Digest}, ipc::ErrorCode};
use crate::Result;

#[cfg(any(target_os = "linux", test))]
mod adapter_state;
#[cfg(target_os = "linux")]
mod adapter;
#[cfg(target_os = "linux")]
pub use adapter::LoopStorage;

/// G must bind the complete approved proposal to a stable operation id. Retries
/// with the same id and different content must fail, including after a restart.
#[derive(Debug, Clone)]
pub struct ProvisionRequest {
    pub employee: EmployeeId,
    pub uid: u32,
    pub operation_id: RequestId,
    pub proposal_digest: Sha256Digest,
    pub quota: DiskQuota,
}
impl ProvisionRequest {
    pub fn validate(&self) -> Result<()> {
        let (bytes, inodes) = quota_limits(&self.quota);
        if !valid_employee_uid(self.uid) || bytes == 0 || inodes == 0 {
            return Err(ErrorCode::InvalidRequest);
        }
        Ok(())
    }
}
pub fn valid_employee_uid(uid: u32) -> bool {
    uid >= 1000 && !duduclaw_isolation::ids::PERMANENT_UID_EXCLUSIONS.iter().any(|range| uid >= range.first && uid <= range.last)
}
pub fn quota_limits(quota: &DiskQuota) -> (u64, u64) {
    match quota { DiskQuota::LoopFilesystem { bytes, inodes } | DiskQuota::ProjectQuota { bytes, inodes } => (*bytes, *inodes) }
}

/// H verifies the entire backing-device graph, hard quota and host reserve.
/// A successful provision does not itself authorize an attach or a new run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageUsage {
    pub employee: EmployeeId,
    pub bytes: u64,
    pub inodes: u64,
    pub limit_bytes: u64,
    pub limit_inodes: u64,
    pub hard_quota_verified: bool,
    pub device_graph_verified: bool,
    pub host_reserve_verified: bool,
}
impl StorageUsage {
    pub fn check(&self, employee: EmployeeId, quota: &DiskQuota) -> Result<()> {
        let (bytes, inodes) = quota_limits(quota);
        if self.employee != employee || bytes == 0 || inodes == 0
            || self.limit_bytes == 0 || self.limit_bytes > bytes || self.limit_inodes == 0 || self.limit_inodes > inodes
            || self.bytes > self.limit_bytes || self.inodes > self.limit_inodes
            || !self.hard_quota_verified || !self.device_graph_verified || !self.host_reserve_verified {
            return Err(ErrorCode::IsolationUnavailable);
        }
        Ok(())
    }
}

/// Host-side attachment identity; never sent to the gateway. The token is H's
/// durable generation, preventing an old cleanup from detaching a newer run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub employee: EmployeeId,
    pub run: RunHandle,
    pub token: RequestId,
    pub device_major: u32,
    pub device_minor: u32,
}
impl Attachment {
    /// Cleanup also accepts a durable attach intent with no observed device.
    pub fn check_identity(&self, employee: EmployeeId, run: RunHandle) -> Result<()> {
        if self.employee != employee || self.run != run { return Err(ErrorCode::IsolationUnavailable); }
        Ok(())
    }
    /// A launch additionally requires an actually observed mounted device.
    pub fn check(&self, employee: EmployeeId, run: RunHandle) -> Result<()> {
        self.check_identity(employee, run)?;
        if self.device_major == 0 { return Err(ErrorCode::IsolationUnavailable); }
        Ok(())
    }
}

/// H's adapter supplies detached private recursive mounts, never ordinary
/// directory fds. B1/B2 must read back mount identities and restrictive flags.
/// Images, loop devices and root anchor directories must never be handed over.
pub struct AttachedStorage {
    pub receipt: Attachment,
    #[cfg(target_os = "linux")]
    pub home: std::os::fd::OwnedFd,
    #[cfg(target_os = "linux")]
    pub work: std::os::fd::OwnedFd,
}

/// Implemented by LoopStorage using package H and a separate durable B1 journal.
/// All calls are bounded worker operations; never block the intrusion scanner.
/// Provision/destroy require G's administrator-approved idempotent operation.
/// Attach rechecks space/quota/device evidence. Detach is idempotent and only
/// occurs after inactive/empty/no-UID cleanup, including restart reconciliation.
/// Destroy never recycles the employee uid and refuses any active attachment.
pub trait Storage: Send {
    fn readiness(&mut self) -> Result<()>;
    fn provision(&mut self, request: &ProvisionRequest) -> Result<StorageUsage>;
    fn attach(&mut self, employee: EmployeeId, uid: u32, run: RunHandle, quota: &DiskQuota) -> Result<AttachedStorage>;
    fn detach(&mut self, attachment: &Attachment) -> Result<()>;
    fn destroy(&mut self, employee: EmployeeId, operation: RequestId, proposal_digest: Sha256Digest) -> Result<()>;
    fn usage(&mut self, employee: EmployeeId) -> Result<StorageUsage>;
    /// Durable attachments left by an earlier daemon. An unavailable inventory
    /// is an error, not an empty list. B1 reconciles every listed run first.
    fn attachments(&mut self) -> Result<Vec<Attachment>>;
    /// H3 (D-H1): `(dev, inode)` of the employee's root-owned work anchor, the
    /// stable `employee-work` identity. Only `work_anchor::AnchoredStorage`
    /// (the allocation/retire hook) has one; everything else fails closed.
    /// Absent anchor ⇒ `NotFound`. Never creates.
    fn work_anchor(&mut self, _employee: EmployeeId) -> Result<duduclaw_isolation::ipc::WorkAnchorIdentity> {
        Err(ErrorCode::WorkspaceBrokerUnavailable)
    }
}

pub struct UnavailableStorage;
impl Storage for UnavailableStorage {
    fn readiness(&mut self) -> Result<()> { Err(ErrorCode::IsolationUnavailable) }
    fn provision(&mut self, _: &ProvisionRequest) -> Result<StorageUsage> { Err(ErrorCode::IsolationUnavailable) }
    fn attach(&mut self, _: EmployeeId, _: u32, _: RunHandle, _: &DiskQuota) -> Result<AttachedStorage> { Err(ErrorCode::IsolationUnavailable) }
    fn detach(&mut self, _: &Attachment) -> Result<()> { Err(ErrorCode::IsolationUnavailable) }
    fn destroy(&mut self, _: EmployeeId, _: RequestId, _: Sha256Digest) -> Result<()> { Err(ErrorCode::IsolationUnavailable) }
    fn usage(&mut self, _: EmployeeId) -> Result<StorageUsage> { Err(ErrorCode::IsolationUnavailable) }
    fn attachments(&mut self) -> Result<Vec<Attachment>> { Err(ErrorCode::IsolationUnavailable) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn storage_usage_requires_identity_hard_limits_and_live_evidence() {
        let employee = EmployeeId([1; 16]);
        let quota = DiskQuota::LoopFilesystem { bytes: 1024, inodes: 32 };
        let valid = StorageUsage { employee, bytes: 512, inodes: 3, limit_bytes: 1024, limit_inodes: 32,
            hard_quota_verified: true, device_graph_verified: true, host_reserve_verified: true };
        assert!(valid.check(employee, &quota).is_ok());
        assert!(valid.check(EmployeeId([2; 16]), &quota).is_err());
        let mut bad = valid.clone(); bad.host_reserve_verified = false; assert!(bad.check(employee, &quota).is_err());
        let mut bad = valid.clone(); bad.limit_bytes += 1; assert!(bad.check(employee, &quota).is_err());
        let mut bad = valid.clone(); bad.bytes = 1025; assert!(bad.check(employee, &quota).is_err());
        let mut bad = valid.clone(); bad.device_graph_verified = false; assert!(bad.check(employee, &quota).is_err());
        let mut bad = valid; bad.hard_quota_verified = false; assert!(bad.check(employee, &quota).is_err());
    }
    #[test]
    fn storage_adapter_unmounted_intent_is_cleanup_only() {
        let employee = EmployeeId([1; 16]);
        let run = RunHandle(duduclaw_isolation::ids::RunId([2; 16]));
        let intent = Attachment { employee, run, token: RequestId([3; 16]), device_major: 0, device_minor: 0 };
        assert!(intent.check_identity(employee, run).is_ok());
        assert!(intent.check(employee, run).is_err());
        assert!(intent.check_identity(EmployeeId([4; 16]), run).is_err());
        assert!(intent.check_identity(employee, RunHandle(duduclaw_isolation::ids::RunId([5; 16]))).is_err());
    }
    #[test]
    fn unavailable_storage_never_returns_empty_success() {
        let mut storage = UnavailableStorage;
        assert!(storage.readiness().is_err());
        assert!(storage.attachments().is_err());
        assert!(storage.usage(EmployeeId([1; 16])).is_err());
    }
}
