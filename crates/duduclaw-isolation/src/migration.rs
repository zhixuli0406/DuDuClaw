//! Migration contracts from design §10 and §17.7 (R7-M3).
//! Deserialized records are untrusted proposals/content, never authority.

use serde::{Deserialize, Serialize};

use crate::ids::{EmployeeId, RequestId, Sha256Digest};
use crate::ContractError;

pub const MAX_IMPORT_RECORD_BYTES: usize = 64 * 1024;
pub const MAX_IMPORT_STREAM_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_IMPORT_RECORDS: u64 = 100_000;
pub const MAX_PROPOSAL_VALUE_BYTES: usize = 64 * 1024;
pub const MAX_PROPOSAL_FIELDS: usize = 128;
pub const DEFAULT_EMPLOYEE_PROPOSAL_LIMIT: u32 = 200;
pub const CONFIRMATION_STEP_UP_SECONDS: u64 = 5 * 60;
pub const SETUP_TOKEN_TTL_SECONDS: u64 = 30 * 60;

/// Root-owned migration progress (design §10.1). Failure retains the last completed
/// stage; retry repeats the current operation idempotently, never advances on error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationState {
    Planned,
    OldEnvironmentStopped,
    LegacyQuarantined,
    KeysRotated,
    TokenRevocationRecorded,
    EmployeeProposalsStaged,
    ContentImported,
    ConfirmationReady,
    IsolationVerified,
    AwaitingAdministrator,
    Complete,
}

impl MigrationState {
    /// This checks ordering only. Package G owns durable readbacks, token revocation,
    /// confirmation, compensating actions and the final zero-outstanding-items gate.
    pub fn transition(self, next: Self) -> Result<Self, ContractError> {
        match (self, next) {
            (Self::Planned, Self::OldEnvironmentStopped)
            | (Self::OldEnvironmentStopped, Self::LegacyQuarantined)
            | (Self::LegacyQuarantined, Self::KeysRotated)
            | (Self::KeysRotated, Self::TokenRevocationRecorded)
            | (Self::TokenRevocationRecorded, Self::EmployeeProposalsStaged)
            | (Self::EmployeeProposalsStaged, Self::ContentImported)
            | (Self::ContentImported, Self::ConfirmationReady)
            | (Self::ConfirmationReady, Self::IsolationVerified)
            | (Self::IsolationVerified, Self::AwaitingAdministrator)
            | (Self::AwaitingAdministrator, Self::Complete) => Ok(next),
            _ => Err(ContractError::IllegalTransition),
        }
    }
}

/// A bounded content record from a no-network importer (design §10.2).
/// `migration_id` is host-bound, checked against the active import connection.
/// The gateway stamps `legacy_import:<migration_id>` itself and assigns minimum
/// trust; imported source labels, identifiers or prose never confer authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportRecord {
    pub schema_version: u16,
    pub migration_id: RequestId,
    pub sequence: u64,
    pub employee: Option<EmployeeId>,
    pub source_record_id: String,
    pub content: ImportedContent,
}

/// Allowed content-only categories (design §10.2). Auth/session/approval/working-state
/// variants deliberately do not exist. Wiki frontmatter is inert text; audits and
/// task history are read-only archives, never inputs to wakeups or recent_actions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImportedContent {
    Conversation { text: String, recorded_at_unix: Option<u64> },
    ChannelHistory { text: String, recorded_at_unix: Option<u64> },
    Memory { text: String },
    WikiPage { title: String, text: String },
    TaskHistory { text: String },
    LegacyAuditArchive { text: String },
    Attachment { blob: ImportedBlob },
    WorkProduct { blob: ImportedBlob },
}

/// Host-owned blob reference (design §10.1, §10.2). An untrusted label is display
/// text, never a path. Package G resolves the handle only against its validated,
/// read-only source-fd registry, recomputes the digest and enforces employee quota.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedBlob {
    pub source_handle: RequestId,
    pub display_name: String,
    pub size_bytes: u64,
    pub sha256: Sha256Digest,
}

impl ImportRecord {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.schema_version != 1 {
            return Err(ContractError::UnsupportedVersion);
        }
        if self.source_record_id.is_empty() || self.source_record_id.len() > 512 {
            return Err(ContractError::InvalidValue("source record id"));
        }
        match &self.content {
            ImportedContent::Attachment { blob } | ImportedContent::WorkProduct { blob } => {
                if blob.display_name.is_empty() || blob.display_name.len() > 1024 {
                    return Err(ContractError::InvalidValue("blob display name"));
                }
            }
            ImportedContent::Conversation { text, .. }
            | ImportedContent::ChannelHistory { text, .. }
            | ImportedContent::Memory { text }
            | ImportedContent::TaskHistory { text }
            | ImportedContent::LegacyAuditArchive { text } => {
                if text.len() > MAX_IMPORT_RECORD_BYTES {
                    return Err(ContractError::LimitExceeded("import text"));
                }
            }
            ImportedContent::WikiPage { title, text } => {
                if title.len() > 1024 || text.len() > MAX_IMPORT_RECORD_BYTES {
                    return Err(ContractError::LimitExceeded("wiki import"));
                }
            }
        }
        let encoded = serde_json::to_vec(self).map_err(|_| ContractError::InvalidFrame)?;
        if encoded.len() > MAX_IMPORT_RECORD_BYTES {
            return Err(ContractError::LimitExceeded("import record"));
        }
        Ok(())
    }

    /// Call on one JSONL record without its newline, after bounding the line reader.
    /// Enforces the byte cap BEFORE deserializing; package G also caps total stream.
    pub fn decode(line: &[u8]) -> Result<Self, ContractError> {
        if line.len() > MAX_IMPORT_RECORD_BYTES {
            return Err(ContractError::LimitExceeded("import record"));
        }
        let record: Self = serde_json::from_slice(line).map_err(|_| ContractError::InvalidFrame)?;
        record.validate()?;
        Ok(record)
    }
}

/// Content-stream accounting (design §10.2). `observe` is transactional on failure;
/// the transport must bound each line before allocation, even on rejected records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImportBudget {
    bytes: u64,
    records: u64,
}

impl ImportBudget {
    pub fn observe(&mut self, line_bytes: usize) -> Result<(), ContractError> {
        if line_bytes > MAX_IMPORT_RECORD_BYTES {
            return Err(ContractError::LimitExceeded("import record"));
        }
        let bytes = self.bytes.checked_add(line_bytes as u64)
            .ok_or(ContractError::LimitExceeded("import stream"))?;
        let records = self.records.checked_add(1)
            .ok_or(ContractError::LimitExceeded("import records"))?;
        if bytes > MAX_IMPORT_STREAM_BYTES || records > MAX_IMPORT_RECORDS {
            return Err(ContractError::LimitExceeded("import stream"));
        }
        self.bytes = bytes;
        self.records = records;
        Ok(())
    }
}

/// Authority categories requiring a fresh validated write (design §10.3).
/// `LegacyApproval` is an informational rejected item, never re-approvable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationCategory {
    User,
    Role,
    Department,
    Employee,
    EmployeeInstructions,
    UserAgentBinding,
    ChannelBinding,
    LegacyApproval,
    McpServer,
    Skill,
    Hook,
    Policy,
    Connector,
    DataSourceAuthorization,
    Budget,
    ConfigurationSection,
    Schedule,
}

/// Persisted confirmation/apply lifecycle (design §10.3, R7-M3).
/// Rejecting requires disabling and reading back partial effects before Rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationStatus {
    Pending,
    Approved,
    Applying,
    Applied,
    Failed,
    Rejecting,
    Rejected,
    ExpiredRejected,
}

impl ConfirmationStatus {
    pub fn transition(self, next: Self) -> Result<Self, ContractError> {
        match (self, next) {
            (Self::Pending, Self::Approved)
            | (Self::Pending, Self::Rejected)
            | (Self::Pending, Self::ExpiredRejected)
            | (Self::Approved, Self::Applying)
            | (Self::Approved, Self::Rejecting)
            | (Self::Applying, Self::Applied)
            | (Self::Applying, Self::Failed)
            | (Self::Failed, Self::Applying)
            | (Self::Failed, Self::Rejecting)
            | (Self::Rejecting, Self::Rejected) => Ok(next),
            _ => Err(ContractError::IllegalTransition),
        }
    }
}

/// Proposal identity and revision, together forming operation_id (design §10.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalOperationId {
    pub proposal_id: RequestId,
    pub revision: u64,
}

/// Every apply/retry RPC binds BOTH operation id and full payload digest (§17.7, R7-M3).
/// The digest covers normalized full values, using keyed HMACs for secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalBinding {
    pub operation_id: ProposalOperationId,
    pub proposal_digest: Sha256Digest,
}

/// UI-safe proposal values (design §10.3, R6-M6). Secret originals stay in a
/// separate protected store; this schema has no plaintext-secret field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProposalValue {
    Text { value: String },
    SecretFingerprint { keyed_fingerprint: Sha256Digest, needs_rotation: bool },
}

/// Named, category-specific proposal field (design §10.3). Package G must use an
/// exact field schema for each category, rejecting unknown fields on re-entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalField {
    pub name: String,
    pub value: ProposalValue,
}

/// Pending-confirmation record (design §10.3). `high_risk` is computed by the
/// gateway from category AND complete payload; never trusted from importer input.
/// Approved payloads are immutable; edits create a new revision and digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmationItem {
    pub binding: ProposalBinding,
    pub category: ConfirmationCategory,
    pub status: ConfirmationStatus,
    pub high_risk: bool,
    pub fields: Vec<ProposalField>,
}

impl ConfirmationItem {
    /// Structural checks only. Category validation, secret-field classification,
    /// step-up, CSRF/session binding and atomic approval live in package G.
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.binding.operation_id.revision == 0 {
            return Err(ContractError::InvalidValue("proposal revision"));
        }
        if self.category == ConfirmationCategory::LegacyApproval
            && self.status != ConfirmationStatus::ExpiredRejected
        {
            return Err(ContractError::InvalidValue("legacy approval"));
        }
        if self.fields.len() > MAX_PROPOSAL_FIELDS {
            return Err(ContractError::LimitExceeded("proposal fields"));
        }
        for (index, field) in self.fields.iter().enumerate() {
            if field.name.is_empty() || field.name.len() > 128
                || self.fields.iter().take(index).any(|other| field.name == other.name)
            {
                return Err(ContractError::InvalidValue("proposal field"));
            }
            if let ProposalValue::Text { value } = &field.value {
                if value.len() > MAX_PROPOSAL_VALUE_BYTES {
                    return Err(ContractError::LimitExceeded("proposal value"));
                }
            }
        }
        Ok(())
    }

    /// Validate the item before and after transition; old approvals stay rejected.
    pub fn transition(&mut self, next: ConfirmationStatus) -> Result<(), ContractError> {
        self.validate()?;
        let status = self.status.transition(next)?;
        if self.category == ConfirmationCategory::LegacyApproval {
            return Err(ContractError::IllegalTransition);
        }
        self.status = status;
        Ok(())
    }
}

/// Required apply progress for EVERY authority category (design §17.7, R7-M3).
/// Package G supplies per-category staging, publication, readback and disable
/// compensation; a successful DB approval is not successful application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalApplyStep {
    Staged,
    Published,
    ReadBack,
    Disabled,
}

/// Durable apply receipt: retries with another digest MUST be refused (§17.7, R7-M3).
/// Errors contain opaque codes only, never secret values or command contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalApplyReceipt {
    pub binding: ProposalBinding,
    pub category: ConfirmationCategory,
    pub step: ProposalApplyStep,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_transition_table_rejects_skips_and_rewind() {
        let states = [MigrationState::Planned, MigrationState::OldEnvironmentStopped,
            MigrationState::LegacyQuarantined, MigrationState::KeysRotated,
            MigrationState::TokenRevocationRecorded, MigrationState::EmployeeProposalsStaged,
            MigrationState::ContentImported, MigrationState::ConfirmationReady,
            MigrationState::IsolationVerified, MigrationState::AwaitingAdministrator,
            MigrationState::Complete];
        for (a, from) in states.iter().enumerate() {
            for (b, to) in states.iter().enumerate() {
                assert_eq!(from.transition(*to).is_ok(), b == a + 1);
            }
        }
    }

    #[test]
    fn confirmation_transition_table_requires_compensation() {
        let states = [ConfirmationStatus::Pending, ConfirmationStatus::Approved,
            ConfirmationStatus::Applying, ConfirmationStatus::Applied,
            ConfirmationStatus::Failed, ConfirmationStatus::Rejecting,
            ConfirmationStatus::Rejected, ConfirmationStatus::ExpiredRejected];
        let allowed = [(0, 1), (0, 6), (0, 7), (1, 2), (1, 5), (2, 3), (2, 4),
            (4, 2), (4, 5), (5, 6)];
        for (a, from) in states.iter().enumerate() {
            for (b, to) in states.iter().enumerate() {
                assert_eq!(from.transition(*to).is_ok(), allowed.iter().any(|p| *p == (a, b)));
            }
        }
    }

    #[test]
    fn import_record_round_trip_and_limits() {
        let record = ImportRecord { schema_version: 1, migration_id: RequestId([1; 16]),
            sequence: 0, employee: None, source_record_id: "row:1".into(),
            content: ImportedContent::Memory { text: "舊記憶".into() } };
        let bytes = serde_json::to_vec(&record).unwrap();
        assert_eq!(ImportRecord::decode(&bytes).unwrap(), record);
        let mut json = serde_json::to_value(&record).unwrap();
        json["content"]["kind"] = "approval".into();
        assert!(serde_json::from_value::<ImportRecord>(json).is_err());
        assert!(ImportRecord::decode(&vec![b' '; MAX_IMPORT_RECORD_BYTES + 1]).is_err());
        let mut budget = ImportBudget::default();
        assert!(budget.observe(MAX_IMPORT_RECORD_BYTES + 1).is_err());
        assert_eq!(budget, ImportBudget::default());
        for _ in 0..(MAX_IMPORT_STREAM_BYTES / MAX_IMPORT_RECORD_BYTES as u64) {
            budget.observe(MAX_IMPORT_RECORD_BYTES).unwrap();
        }
        assert!(budget.observe(1).is_err());
    }

    #[test]
    fn legacy_approvals_cannot_be_revived() {
        let mut item = ConfirmationItem { binding: ProposalBinding {
            operation_id: ProposalOperationId { proposal_id: RequestId([1; 16]), revision: 1 },
            proposal_digest: Sha256Digest([2; 32]) }, category: ConfirmationCategory::LegacyApproval,
            status: ConfirmationStatus::ExpiredRejected, high_risk: true, fields: vec![] };
        assert!(item.validate().is_ok());
        assert!(item.transition(ConfirmationStatus::Approved).is_err());
        item.status = ConfirmationStatus::Pending;
        assert!(item.validate().is_err());
    }

    #[test]
    fn proposal_round_trip_and_full_value_bound() {
        let mut item = ConfirmationItem { binding: ProposalBinding {
            operation_id: ProposalOperationId { proposal_id: RequestId([1; 16]), revision: 1 },
            proposal_digest: Sha256Digest([2; 32]) }, category: ConfirmationCategory::Hook,
            status: ConfirmationStatus::Pending, high_risk: true, fields: vec![ProposalField {
                name: "command".into(), value: ProposalValue::Text { value: "review me".into() } }] };
        assert_eq!(serde_json::from_str::<ConfirmationItem>(
            &serde_json::to_string(&item).unwrap()).unwrap(), item);
        item.fields[0].value = ProposalValue::Text { value: "x".repeat(MAX_PROPOSAL_VALUE_BYTES + 1) };
        assert!(item.validate().is_err());
    }
}
