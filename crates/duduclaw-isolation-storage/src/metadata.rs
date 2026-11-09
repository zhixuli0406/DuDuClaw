//! Versioned root metadata, never populated from employee-controlled paths.
use std::collections::BTreeSet;
use serde::{Deserialize, Serialize};
use duduclaw_isolation::ids::EmployeeId;
use crate::{device::BackingGraph, Error, Quota, Result};

pub const MAX_METADATA_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_IMAGES: usize = 4096;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase { Creating, Ready, Resizing, Deleting, Quarantined }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Image {
    pub employee: EmployeeId,
    pub uid: u32,
    pub quota: Quota,
    pub phase: Phase,
    /// Actual regular-file identity, set after durable allocation/formatting.
    pub inode: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub version: u16,
    pub backing: BackingGraph,
    pub images: Vec<Image>,
}
impl Registry {
    pub fn validate(&self) -> Result<()> {
        self.backing.validate()?;
        if self.version != 1 || self.images.len() > MAX_IMAGES { return Err(Error::RecoveryRequired); }
        let mut employees = BTreeSet::new();
        let mut uids = BTreeSet::new();
        for image in &self.images {
            image.quota.validate()?;
            if !(duduclaw_isolation::ids::EMPLOYEE_UID_MIN..=duduclaw_isolation::ids::EMPLOYEE_UID_MAX).contains(&image.uid)
                || !employees.insert(image.employee) || !uids.insert(image.uid)
                || (matches!(image.phase, Phase::Ready | Phase::Resizing) && image.inode.is_none()) || image.inode == Some(0) {
                return Err(Error::RecoveryRequired);
            }
        }
        Ok(())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_METADATA_BYTES { return Err(Error::RecoveryRequired); }
        let state: Self = serde_json::from_slice(bytes).map_err(|_| Error::RecoveryRequired)?;
        state.validate()?;
        Ok(state)
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| Error::RecoveryRequired)?;
        if bytes.len() > MAX_METADATA_BYTES { return Err(Error::RecoveryRequired); }
        Ok(bytes)
    }
    pub fn image(&self, employee: EmployeeId) -> Result<&Image> {
        self.images.iter().find(|image| image.employee == employee).ok_or(Error::NotFound)
    }
    pub fn charge(&self) -> impl Iterator<Item = u64> + '_ { self.images.iter().map(|image| image.quota.bytes) }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn registry() -> Registry {
        Registry { version: 1, backing: crate::device::tests::graph(), images: vec![Image {
            employee: EmployeeId([1; 16]), uid: 70000,
            quota: Quota { bytes: 64 * 1024 * 1024, inodes: 1024 }, phase: Phase::Creating, inode: None,
        }] }
    }
    #[test]
    fn metadata_all_crash_phases_remain_charged() {
        for phase in [Phase::Creating, Phase::Ready, Phase::Resizing, Phase::Deleting, Phase::Quarantined] {
            let mut state = registry(); state.images[0].phase = phase; state.images[0].inode = Some(42);
            let recovered = Registry::decode(&state.encode().unwrap()).unwrap();
            assert_eq!(recovered, state);
            assert_eq!(recovered.charge().sum::<u64>(), 64 * 1024 * 1024);
        }
    }
    #[test]
    fn metadata_refuses_truncation_unknown_fields_and_duplicate_ids() {
        let state = registry(); let bytes = state.encode().unwrap();
        for end in 0..bytes.len() { assert!(Registry::decode(&bytes[..end]).is_err()); }
        let mut json = serde_json::to_value(&state).unwrap(); json["extra"] = true.into();
        assert!(Registry::decode(&serde_json::to_vec(&json).unwrap()).is_err());
        let mut state = state; state.images.push(state.images[0].clone()); assert!(state.encode().is_err());
        assert!(Registry::decode(&vec![0; MAX_METADATA_BYTES + 1]).is_err());
    }
    #[test]
    fn metadata_never_promotes_incomplete_image() {
        let mut state = registry(); state.images[0].phase = Phase::Ready;
        assert_eq!(state.validate(), Err(Error::RecoveryRequired));
    }
}
