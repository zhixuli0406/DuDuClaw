//! Root-local operation ledger. This is not part of gateway wire protocol v1.
use duduclaw_isolation::{ids::{EmployeeId, RequestId, RunId, Sha256Digest}, ipc::ErrorCode};
use duduclaw_isolation_storage::{ImageStatus, Quota};
use serde::{Deserialize, Serialize};
use crate::Result;

pub(super) const MAX_EMPLOYEES: usize = 4096;
pub(super) const MAX_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase { Provisioning, Ready, Destroying, Destroyed }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Operation { pub id: RequestId, pub digest: Sha256Digest }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Lease { pub run: RunId, pub token: RequestId }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Employee {
    pub employee: EmployeeId,
    pub uid: u32,
    pub quota: Quota,
    pub provision: Operation,
    pub destroy: Option<Operation>,
    pub phase: Phase,
    /// Persisted before H attach, including operations whose reply was lost.
    pub lease: Option<Lease>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Journal { pub version: u16, pub employees: Vec<Employee> }

impl Journal {
    pub fn empty() -> Self { Self { version: 1, employees: vec![] } }
    pub fn validate(&self) -> Result<()> {
        let mut employees = std::collections::BTreeSet::new();
        let mut uids = std::collections::BTreeSet::new();
        let mut operations = std::collections::BTreeSet::new();
        let mut runs = std::collections::BTreeSet::new();
        if self.version != 1 || self.employees.len() > MAX_EMPLOYEES { return Err(ErrorCode::IsolationUnavailable); }
        for entry in &self.employees {
            if !employees.insert(entry.employee) || !uids.insert(entry.uid)
                || !(70_000..=99_999).contains(&entry.uid) || entry.quota.validate().is_err()
                || !operations.insert(entry.provision.id)
                || entry.destroy.as_ref().is_some_and(|op| !operations.insert(op.id))
                || matches!(entry.phase, Phase::Destroying | Phase::Destroyed) != entry.destroy.is_some()
                || (entry.phase != Phase::Ready && entry.lease.is_some())
                || entry.lease.as_ref().is_some_and(|lease| !runs.insert(lease.run)) {
                return Err(ErrorCode::IsolationUnavailable);
            }
        }
        Ok(())
    }
    pub fn entry(&self, employee: EmployeeId) -> Result<&Employee> {
        self.employees.iter().find(|entry| entry.employee == employee).ok_or(ErrorCode::NotFound)
    }
    pub fn entry_mut(&mut self, employee: EmployeeId) -> Result<&mut Employee> {
        self.employees.iter_mut().find(|entry| entry.employee == employee).ok_or(ErrorCode::NotFound)
    }
    /// Unknown images are never reconstructed as authorized employees. An
    /// absent Ready image is also a loss of evidence, not an empty allocation.
    pub fn reconcile(&self, images: &[ImageStatus]) -> Result<()> {
        self.validate()?;
        let mut seen = std::collections::BTreeSet::new();
        for image in images {
            let entry = self.entry(image.employee).map_err(|_| ErrorCode::IsolationUnavailable)?;
            if !seen.insert(image.employee) || entry.uid != image.uid || entry.quota != image.quota
                || entry.phase == Phase::Destroyed
                || (entry.phase == Phase::Ready && entry.lease.is_none() && image.mounted_device.is_some()) {
                return Err(ErrorCode::IsolationUnavailable);
            }
        }
        if self.employees.iter().any(|entry| entry.phase == Phase::Ready && !seen.contains(&entry.employee)) {
            return Err(ErrorCode::IsolationUnavailable);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_isolation_storage::metadata::Phase as ImagePhase;
    pub(super) fn entry() -> Employee {
        Employee { employee: EmployeeId([1; 16]), uid: 70_000,
            quota: Quota { bytes: 64 * 1024 * 1024, inodes: 1024 },
            provision: Operation { id: RequestId([2; 16]), digest: Sha256Digest([3; 32]) },
            destroy: None, phase: Phase::Ready, lease: None }
    }
    fn image(entry: &Employee) -> ImageStatus {
        ImageStatus { employee: entry.employee, uid: entry.uid, quota: entry.quota,
            phase: ImagePhase::Ready, mounted_device: None }
    }
    #[test]
    fn storage_adapter_rejects_unknown_missing_or_unleased_images() {
        let entry = entry(); let image = image(&entry);
        let journal = Journal { version: 1, employees: vec![entry] };
        assert!(journal.reconcile(std::slice::from_ref(&image)).is_ok());
        assert!(journal.reconcile(&[]).is_err());
        assert!(Journal::empty().reconcile(std::slice::from_ref(&image)).is_err());
        let mut bad = image.clone(); bad.uid += 1;
        assert!(journal.reconcile(&[bad]).is_err());
        let mut bad = image; bad.mounted_device = Some(duduclaw_isolation_storage::device::DeviceId { major: 7, minor: 4 });
        assert!(journal.reconcile(&[bad]).is_err());
    }
    #[test]
    fn storage_adapter_persists_intent_before_attach_and_preserves_tombstones() {
        let mut entry = entry(); entry.lease = Some(Lease { run: RunId([4; 16]), token: RequestId([5; 16]) });
        let journal = Journal { version: 1, employees: vec![entry.clone()] };
        let restored: Journal = serde_json::from_slice(&serde_json::to_vec(&journal).unwrap()).unwrap();
        assert_eq!(restored.entry(entry.employee).unwrap().lease, entry.lease);
        assert!(restored.reconcile(&[image(&entry)]).is_ok());
        entry.phase = Phase::Destroyed;
        entry.destroy = Some(Operation { id: RequestId([6; 16]), digest: Sha256Digest([7; 32]) });
        assert!(Journal { version: 1, employees: vec![entry.clone()] }.validate().is_err());
        entry.lease = None;
        let destroyed = Journal { version: 1, employees: vec![entry.clone()] };
        assert!(destroyed.reconcile(&[]).is_ok());
        assert!(destroyed.reconcile(&[image(&entry)]).is_err());
        let mut recycled = entry; recycled.employee = EmployeeId([9; 16]);
        let mut duplicated = destroyed; duplicated.employees.push(recycled);
        assert!(duplicated.validate().is_err());
    }
    #[test]
    fn storage_adapter_rejects_duplicate_operations_and_unknown_schema() {
        let mut entry = entry(); entry.phase = Phase::Destroying;
        entry.destroy = Some(entry.provision.clone());
        assert!(Journal { version: 1, employees: vec![entry] }.validate().is_err());
        let mut json = serde_json::to_value(Journal::empty()).unwrap(); json["extra"] = true.into();
        assert!(serde_json::from_value::<Journal>(json).is_err());
    }
}
