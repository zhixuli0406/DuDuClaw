//! Root-issued import boot plan. This is a local Package G protocol, not an
//! isolation wire-format change. It contains normalized proposals, never old files.
use std::{collections::BTreeSet, path::{Component, PathBuf}};
use serde::{Deserialize, Serialize};
use crate::{Error, import_receiver::ImportGrant};
use crate::contract::{ConfirmationCategory, ProposalField};
use crate::ids::RequestId;

pub const IMPORT_MANIFEST: &str = "/run/duduclaw-migrate/manifest.json";
pub const IMPORT_ONLY_ENV: &str = "DUDUCLAW_MIGRATION_IMPORT_ONLY";
pub const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_AUTHORITY_RECORD_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportBootDecision { Normal, ImportOnly }

/// Decide before any ordinary startup side effect. The Linux caller first loads
/// and authenticates the root-owned manifest; this function performs no I/O.
pub fn boot_decision(requested: Option<&std::ffi::OsStr>, manifest: Option<&ImportManifest>,
    home: &std::path::Path) -> Result<ImportBootDecision, Error> {
    if let Some(plan) = manifest { plan.validate()?; }
    match (requested, manifest) {
        (None, None) => Ok(ImportBootDecision::Normal),
        (None, Some(plan)) if plan.home != home || plan.completed => Ok(ImportBootDecision::Normal),
        (Some(value), Some(plan)) if value == "1" && plan.home == home && !plan.completed => Ok(ImportBootDecision::ImportOnly),
        _ => Err(Error::Authentication),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestGrant {
    pub grant: ImportGrant,
    /// None archives content without creating an employee, UID, disk or active
    /// memory/wiki namespace. Root may only name an already confirmed employee.
    pub target_agent: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingProposal {
    pub migration_id: RequestId,
    pub source_id: String,
    pub sequence: u64,
    pub category: ConfirmationCategory,
    pub fields: Vec<ProposalField>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportManifest {
    pub schema_version: u16,
    pub migration_id: RequestId,
    pub home: PathBuf,
    pub gateway_uid: u32,
    pub grants: Vec<ManifestGrant>,
    pub pending_proposals: Vec<PendingProposal>,
    pub completed: bool,
}
impl ImportManifest {
    pub fn validate(&self) -> Result<(), Error> {
        if self.schema_version != 1 || self.gateway_uid == 0 || !self.home.is_absolute()
            || self.home.components().any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
            || self.grants.len() > 100_000 || self.pending_proposals.len() > 2048 {
            return Err(Error::Invalid);
        }
        let mut sources = BTreeSet::new();
        for item in &self.grants {
            item.grant.validate()?;
            if item.grant.migration_id != self.migration_id
                || !sources.insert(item.grant.source_id.clone()) {
                return Err(Error::Invalid);
            }
            // Import runs before employee confirmation. Materialization belongs
            // to the confirmed allocation path, not this privileged boot plan.
            if item.target_agent.is_some() { return Err(Error::Invalid); }
        }
        let mut operations = BTreeSet::new();
        for item in &self.pending_proposals {
            if item.migration_id != self.migration_id || item.source_id.is_empty() || item.source_id.len() > 400
                || item.fields.len() > crate::contract::MAX_PROPOSAL_FIELDS
                || item.fields.iter().any(|field| field.name.len() > 128 || match &field.value {
                    crate::contract::ProposalValue::Text {value} => value.len() > crate::contract::MAX_PROPOSAL_VALUE_BYTES,
                    crate::contract::ProposalValue::SecretFingerprint {..} => false,
                })
                || !operations.insert((item.source_id.clone(), item.sequence)) {
                return Err(Error::Invalid);
            }
        }
        Ok(())
    }
    /// Completion is the only permitted mutation while an import process runs.
    pub fn same_plan(&self, other: &Self) -> bool {
        let mut expected = self.clone(); expected.completed = other.completed;
        expected == *other
    }
}

/// Stage only root-authenticated, bounded proposals. No product writer or
/// allocator is reachable here; approval still requires fresh product validation.
/// Keep this shared with the gateway so portable extraction tests exercise the
/// same deterministic identities and inert staging path as import-only boot.
pub fn stage_pending_proposals(store: &mut crate::confirmation::ConfirmationStore,
    proposals: &[PendingProposal]) -> Result<(), Error> {
    use crate::confirmation::{ConfirmationError, ProposalValidator};
    use crate::contract::{ConfirmationItem, ProposalOperationId, ProposalValue};
    use sha2::{Digest, Sha256};
    struct Inert;
    impl ProposalValidator for Inert {
        fn validate_fresh(&self, item: &ConfirmationItem) -> Result<(), ConfirmationError> {
            item.validate().map_err(|_| ConfirmationError::Invalid)?;
            if item.fields.iter().any(|field| matches!(field.value, ProposalValue::SecretFingerprint { .. })) {
                return Err(ConfirmationError::Invalid);
            }
            Ok(())
        }
    }
    if proposals.len() > 2048 { return Err(Error::Limit); }
    let mut identities = BTreeSet::new();
    let migration = proposals.first().map(|p| p.migration_id);
    // Reject mixed manifests/duplicate records before writing any of the batch.
    for proposal in proposals {
        if Some(proposal.migration_id) != migration || proposal.source_id.is_empty()
            || proposal.source_id.len() > 400
            || !identities.insert((&proposal.source_id, proposal.sequence)) {
            return Err(Error::Invalid);
        }
    }
    for proposal in proposals {
        let bytes = serde_json::to_vec(&(proposal.migration_id, &proposal.source_id, proposal.sequence))?;
        let hash = Sha256::digest(bytes);
        let mut id = [0u8; 16]; id.copy_from_slice(&hash[..16]);
        store.stage_proposal(ProposalOperationId { proposal_id: RequestId(id), revision: 1 },
            proposal.category, proposal.fields.clone(), &Inert).map_err(|_| Error::Invalid)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ids::{EmployeeId, Sha256Digest}, import_receiver::ContentClass};
    fn plan() -> ImportManifest {
        ImportManifest {schema_version:1,migration_id:RequestId([1;16]),home:PathBuf::from("/var/lib/duduclaw/.duduclaw"),gateway_uid:991,
            grants:vec![ManifestGrant {grant:ImportGrant {migration_id:RequestId([1;16]),employee:Some(EmployeeId([2;16])),source_id:"memory".into(),
                class:ContentClass::Memory,stream_bytes:0,records:0,stream_sha256:Sha256Digest([3;32])},target_agent:None}],pending_proposals:vec![],completed:false}
    }
    #[test]
    fn import_manifest_never_allocates_unconfirmed_employee() {
        let mut p=plan();p.validate().unwrap();p.grants[0].target_agent=Some("old-admin".into());assert!(p.validate().is_err());
    }
    #[test]
    fn import_manifest_only_completion_may_change_after_boot() {
        let p=plan();let mut done=p.clone();done.completed=true;assert!(p.same_plan(&done));
        done.grants.clear();assert!(!p.same_plan(&done));
        let mut duplicate=p.clone();duplicate.grants.push(p.grants[0].clone());assert!(duplicate.validate().is_err());
    }
    #[test]
    fn import_boot_explicit_request_never_falls_back_to_normal_startup() {
        use std::ffi::OsStr;
        let plan=plan(); let home=plan.home.as_path();
        assert_eq!(boot_decision(None,None,home),Ok(ImportBootDecision::Normal));
        assert!(boot_decision(None,Some(&plan),home).is_err());
        assert_eq!(boot_decision(Some(OsStr::new("1")),Some(&plan),home),Ok(ImportBootDecision::ImportOnly));
        for value in ["", "0", "true", "invalid", "1"] {
            assert!(boot_decision(Some(OsStr::new(value)),None,home).is_err());
            if value!="1" {assert!(boot_decision(Some(OsStr::new(value)),Some(&plan),home).is_err());}
        }
        let other=std::path::Path::new("/var/lib/another-home");
        assert!(boot_decision(Some(OsStr::new("1")),Some(&plan),other).is_err());
        let mut complete=plan.clone();complete.completed=true;
        assert_eq!(boot_decision(None,Some(&complete),home),Ok(ImportBootDecision::Normal));
        assert!(boot_decision(Some(OsStr::new("1")),Some(&complete),home).is_err());
    }
}
