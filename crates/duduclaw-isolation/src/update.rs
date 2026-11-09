//! Update-service contracts from design §8 and §17.7. No download, signature
//! verification, installation, or durability operation is performed here.

use serde::{Deserialize, Serialize};

use crate::ids::{RequestId, Sha256Digest};
use crate::ContractError;

pub const ROOT_SSHSIG_NAMESPACE: &str = "duduclaw-root-metadata-v1@dudustudio";
pub const RELEASE_SSHSIG_NAMESPACE: &str = "duduclaw-release-manifest-v1@dudustudio";
pub const RUNTIME_SSHSIG_NAMESPACE: &str = "duduclaw-runtime-manifest-v1@dudustudio";
pub const EMERGENCY_SSHSIG_NAMESPACE: &str = "duduclaw-emergency-v1@dudustudio";
pub const ROOT_SK_APPLICATION: &str = "ssh:duduclaw-root";
pub const TARGET_SK_APPLICATION: &str = "ssh:duduclaw-target";
pub const SSHSIG_HASH_ALGORITHM: &str = "sha512";
pub const SSHSIG_VERSION: u32 = 1;
pub const REQUIRED_SK_FLAGS: u8 = 0x01 | 0x04;
pub const MAX_ARTIFACTS: usize = 128;
pub const MAX_KEY_BLOB_BYTES: usize = 4096;

/// Stable release version (design §8, §13); prerelease/build strings are not RPC input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

/// Linux release architecture (design §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Architecture {
    X86_64,
    Aarch64,
}

/// Product identity included in signed manifests (design §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Product {
    Duduclaw,
    DuduclawPro,
}

/// Manifest OS, deliberately Linux-only for this release (design §0, §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatingSystem {
    Linux,
}

/// Protected update channel (design §8, §13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseChannel {
    Stable,
}

/// Fixed installer destinations; package F maps these to root-owned paths (§8).
/// An RPC caller never supplies a filesystem path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InstallTarget {
    Gateway,
    Spawnd,
    Trampoline,
    Egress,
    EgressForwarder,
    McpBridge,
    ParseWorker,
    Importer,
    Migration,
    UpdateSlot,
    RuntimeEntry { registry_id: String },
}

/// One signed artifact (design §8). Mode excludes setuid, setgid and world/group write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseArtifact {
    pub id: String,
    pub sha256: Sha256Digest,
    pub size_bytes: u64,
    pub mode: u16,
    pub install_target: InstallTarget,
}

/// Exact protocol versions of a release generation (design §8, §12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolCompatibility {
    pub gateway: u16,
    pub spawnd: u16,
    pub trampoline: u16,
    pub egress: u16,
    pub updated: u16,
    pub mcp_bridge: u16,
    pub parse_worker: u16,
    pub importer: u16,
}

/// Data migration bound to an artifact digest, not a caller-supplied executable (§8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataMigration {
    pub id: String,
    pub sha256: Sha256Digest,
    pub reversible: bool,
}

/// Signed release schema (design §8). Package F must verify canonical bytes,
/// root/target trust, expiry, all artifact digests, host architecture, and monotonic
/// version/serial before using it. Deserialization alone grants no trust.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifest {
    pub schema_version: u16,
    pub product: Product,
    pub version: ReleaseVersion,
    pub channel: ReleaseChannel,
    pub os: OperatingSystem,
    pub arch: Architecture,
    pub minimum_version: ReleaseVersion,
    pub serial: u64,
    pub created_at_unix: u64,
    pub expires_at_unix: u64,
    pub artifacts: Vec<ReleaseArtifact>,
    pub protocols: ProtocolCompatibility,
    pub migrations: Vec<DataMigration>,
}

fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && value != "." && value != ".."
}

impl ReleaseManifest {
    /// Structural checks only; no cryptographic, clock, installed-version or disk checks.
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.schema_version != 1 {
            return Err(ContractError::UnsupportedVersion);
        }
        if self.serial == 0 || self.created_at_unix >= self.expires_at_unix
            || self.minimum_version > self.version
        {
            return Err(ContractError::InvalidValue("release metadata"));
        }
        if self.artifacts.is_empty() || self.artifacts.len() > MAX_ARTIFACTS
            || self.migrations.len() > MAX_ARTIFACTS
        {
            return Err(ContractError::LimitExceeded("release artifacts"));
        }
        let versions = &self.protocols;
        if [versions.gateway, versions.spawnd, versions.trampoline, versions.egress,
            versions.updated, versions.mcp_bridge, versions.parse_worker, versions.importer]
            .into_iter().any(|v| v == 0)
        {
            return Err(ContractError::InvalidValue("protocol version"));
        }
        for (index, artifact) in self.artifacts.iter().enumerate() {
            if !identifier(&artifact.id) || artifact.size_bytes == 0
                || artifact.mode & !0o755 != 0 || artifact.mode & 0o400 == 0
            {
                return Err(ContractError::InvalidValue("artifact"));
            }
            if let InstallTarget::RuntimeEntry { registry_id } = &artifact.install_target {
                if !identifier(registry_id) {
                    return Err(ContractError::InvalidValue("runtime entry"));
                }
            }
            if self.artifacts.iter().take(index).any(|previous|
                previous.id == artifact.id || previous.install_target == artifact.install_target)
            {
                return Err(ContractError::InvalidValue("duplicate artifact"));
            }
        }
        for (index, migration) in self.migrations.iter().enumerate() {
            if !identifier(&migration.id)
                || self.migrations.iter().take(index).any(|previous| previous.id == migration.id)
            {
                return Err(ContractError::InvalidValue("migration id"));
            }
        }
        Ok(())
    }
}

/// Permitted public-key algorithms (design §8, R4-M7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SigningAlgorithm {
    Ed25519,
    SkEd25519,
    SkEcdsaSha2Nistp256,
}

/// Canonical SSH public blob and its SHA-256 identity (design §8).
/// Package F parses the blob and recomputes key_id; fields here are unverified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SigningKey {
    pub key_id: Sha256Digest,
    pub algorithm: SigningAlgorithm,
    pub public_blob: Vec<u8>,
    pub application: Option<String>,
}

impl SigningKey {
    fn validate(&self, application: &str, hardware_required: bool) -> Result<(), ContractError> {
        if self.public_blob.is_empty() || self.public_blob.len() > MAX_KEY_BLOB_BYTES {
            return Err(ContractError::LimitExceeded("public key"));
        }
        match self.algorithm {
            SigningAlgorithm::Ed25519 if !hardware_required && self.application.is_none() => Ok(()),
            SigningAlgorithm::SkEd25519 | SigningAlgorithm::SkEcdsaSha2Nistp256
                if self.application.as_deref() == Some(application) => Ok(()),
            _ => Err(ContractError::InvalidValue("signing key role")),
        }
    }
}

/// Root trust metadata (design §8): exactly three distinct root keys, threshold two,
/// and a separate hardware target key. Rotation requires BOTH old and new thresholds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootMetadata {
    pub schema_version: u16,
    pub version: u64,
    pub expires_at_unix: u64,
    pub root_threshold: u8,
    pub root_keys: Vec<SigningKey>,
    pub target_key: SigningKey,
    pub revoked_key_ids: Vec<Sha256Digest>,
    pub frozen: bool,
}

impl RootMetadata {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.schema_version != 1 {
            return Err(ContractError::UnsupportedVersion);
        }
        if self.version == 0 || self.expires_at_unix == 0
            || self.root_threshold != 2 || self.root_keys.len() != 3
            || self.revoked_key_ids.len() > MAX_ARTIFACTS
        {
            return Err(ContractError::InvalidValue("root threshold metadata"));
        }
        self.target_key.validate(TARGET_SK_APPLICATION, true)?;
        for (index, key) in self.root_keys.iter().enumerate() {
            key.validate(ROOT_SK_APPLICATION, false)?;
            if key.key_id == self.target_key.key_id || key.public_blob == self.target_key.public_blob
                || self.root_keys.iter().take(index).any(|other|
                    key.key_id == other.key_id || key.public_blob == other.public_blob)
            {
                return Err(ContractError::InvalidValue("duplicate signing key"));
            }
        }
        for (index, revoked) in self.revoked_key_ids.iter().enumerate() {
            if self.revoked_key_ids.iter().take(index).any(|id| id == revoked)
                || self.root_keys.iter().any(|key| &key.key_id == revoked)
                || &self.target_key.key_id == revoked
            {
                return Err(ContractError::InvalidValue("revoked signing key"));
            }
        }
        Ok(())
    }
}

/// Root-threshold signed, expiring rollback authorization for an already signed digest (§8).
/// It cannot authorize a new artifact; package F must look up the prior target signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmergencyAuthorization {
    pub schema_version: u16,
    pub serial: u64,
    pub previously_signed_manifest: Sha256Digest,
    pub expires_at_unix: u64,
}

/// Unprivileged update RPC (design §8); envelope uses the shared bounded frame codec.
/// Authenticate the fixed gateway uid/gid with SO_PEERCRED; reject ancillary fds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "version", content = "request", deny_unknown_fields)]
pub enum UpdateRequest {
    #[serde(rename = "1")]
    V1(UpdateRequestV1),
}

/// Gateway requests contain neither URLs, paths, argv nor trust material (design §8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum UpdateRequestV1 {
    Apply { version: ReleaseVersion, request_id: RequestId },
    Status { request_id: RequestId },
}

/// Bounded update response envelope (design §8, §4.4); no free-form internal errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "version", content = "response", deny_unknown_fields)]
pub enum UpdateResponse {
    #[serde(rename = "1")]
    V1(UpdateResponseV1),
}

/// Public update status, independent of root-owned diagnostic logs (design §8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum UpdateResponseV1 {
    Accepted { request_id: RequestId },
    Status { request_id: RequestId, state: UpdateStatus },
    Error { request_id: RequestId, code: UpdateErrorCode },
}

/// Stable caller-visible refusal codes (design §8, §4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateErrorCode {
    Unauthorized,
    InvalidRequest,
    Busy,
    UnknownRequest,
    SignatureRejected,
    Expired,
    RollbackRefused,
    Incompatible,
    MaintenanceRequired,
}

/// Non-sensitive transaction status (design §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateStatus {
    Queued,
    Downloading,
    Prepared,
    Activated,
    Committed,
    Failed,
    Maintenance,
}

/// Durable generation journal states (design §8). A transition is legal only after
/// the caller has verified the corresponding durable writes/readbacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateJournalState {
    Prepared,
    Activated,
    Committed,
}

impl UpdateJournalState {
    pub fn transition(self, next: Self) -> Result<Self, ContractError> {
        match (self, next) {
            (Self::Prepared, Self::Activated) | (Self::Activated, Self::Committed) => Ok(next),
            _ => Err(ContractError::IllegalTransition),
        }
    }
}

/// Updater slot identity (design §8); never an arbitrary path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateSlot {
    A,
    B,
}

/// Durable selector schema (design §8, R4-M9). `attempts` is fsynced BEFORE exec;
/// healthy is recorded only after the new root updater's real self-test succeeds.
/// While a trial is pending, `active == previous` names the known-good fallback;
/// the trial slot differs. On health, active becomes trial and trial/attempts clear.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotSelector {
    pub active: UpdateSlot,
    pub previous: UpdateSlot,
    pub trial_slot: Option<UpdateSlot>,
    pub attempts: u8,
}

impl SlotSelector {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.attempts > 1
            || (self.trial_slot.is_some() && self.previous != self.active)
            || self.trial_slot == Some(self.active)
            || (self.trial_slot.is_none() && self.attempts != 0)
        {
            return Err(ContractError::InvalidValue("slot selector"));
        }
        Ok(())
    }
}

/// A/B updater lifecycle (design §8, R4-M9). Maintenance recovery is an explicit
/// bootstrap procedure outside ordinary RPC; no transition out is exposed here.
/// This only validates the lifecycle order. F must atomically persist the matching
/// SlotSelector, verify slot digests, read health, and fsync before advancing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotState {
    Stable,
    Prepared,
    Trial,
    Healthy,
    RolledBack,
    Maintenance,
}

impl SlotState {
    pub fn transition(self, next: Self) -> Result<Self, ContractError> {
        match (self, next) {
            (Self::Stable, Self::Prepared)
            | (Self::Prepared, Self::Trial)
            | (Self::Trial, Self::Healthy)
            | (Self::Healthy, Self::Stable)
            | (Self::Trial, Self::RolledBack)
            | (Self::RolledBack, Self::Stable) => Ok(next),
            (state, Self::Maintenance) if state != Self::Maintenance => Ok(next),
            _ => Err(ContractError::IllegalTransition),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_manifest_round_trip_and_artifact_bounds() {
        let manifest = ReleaseManifest {
            schema_version: 1, product: Product::Duduclaw,
            version: ReleaseVersion { major: 1, minor: 80, patch: 0 },
            channel: ReleaseChannel::Stable, os: OperatingSystem::Linux,
            arch: Architecture::Aarch64,
            minimum_version: ReleaseVersion { major: 1, minor: 79, patch: 3 },
            serial: 1, created_at_unix: 100, expires_at_unix: 200,
            artifacts: vec![ReleaseArtifact { id: "gateway".into(), sha256: Sha256Digest([1; 32]),
                size_bytes: 1234, mode: 0o755, install_target: InstallTarget::Gateway }],
            protocols: ProtocolCompatibility { gateway: 1, spawnd: 1, trampoline: 1,
                egress: 1, updated: 1, mcp_bridge: 1, parse_worker: 1, importer: 1 },
            migrations: vec![],
        };
        assert!(manifest.validate().is_ok());
        assert_eq!(serde_json::from_str::<ReleaseManifest>(
            &serde_json::to_string(&manifest).unwrap()).unwrap(), manifest);
        let mut invalid = manifest.clone();
        invalid.artifacts.push(invalid.artifacts[0].clone());
        assert!(invalid.validate().is_err());
        invalid = manifest.clone();
        invalid.artifacts[0].mode = 0o4755;
        assert!(invalid.validate().is_err());
        invalid = manifest.clone();
        invalid.expires_at_unix = 100;
        assert!(invalid.validate().is_err());
        invalid = manifest;
        invalid.artifacts[0].install_target = InstallTarget::RuntimeEntry { registry_id: "../bin".into() };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn update_rpc_round_trip_and_rejects_authority_fields() {
        let value = UpdateRequest::V1(UpdateRequestV1::Apply {
            version: ReleaseVersion { major: 1, minor: 80, patch: 0 },
            request_id: RequestId([7; 16]),
        });
        let mut json = serde_json::to_value(&value).unwrap();
        assert_eq!(serde_json::from_value::<UpdateRequest>(json.clone()).unwrap(), value);
        json["request"]["path"] = "/tmp/untrusted".into();
        assert!(serde_json::from_value::<UpdateRequest>(json).is_err());
        assert!(serde_json::from_str::<UpdateRequest>(r#"{"version":"2","request":{}}"#).is_err());
    }

    #[test]
    fn update_state_transition_tables() {
        let states = [SlotState::Stable, SlotState::Prepared, SlotState::Trial,
            SlotState::Healthy, SlotState::RolledBack, SlotState::Maintenance];
        let allowed = [(0, 1), (1, 2), (2, 3), (3, 0), (2, 4), (4, 0),
            (0, 5), (1, 5), (2, 5), (3, 5), (4, 5)];
        for (a, from) in states.iter().enumerate() {
            for (b, to) in states.iter().enumerate() {
                assert_eq!(from.transition(*to).is_ok(), allowed.iter().any(|p| *p == (a, b)));
            }
        }
        let journal = [UpdateJournalState::Prepared, UpdateJournalState::Activated,
            UpdateJournalState::Committed];
        for (a, from) in journal.iter().enumerate() {
            for (b, to) in journal.iter().enumerate() {
                assert_eq!(from.transition(*to).is_ok(), b == a + 1);
            }
        }
    }

    #[test]
    fn slot_selector_refuses_invalid_trials() {
        let selector = SlotSelector { active: UpdateSlot::A, previous: UpdateSlot::A,
            trial_slot: Some(UpdateSlot::B), attempts: 1 };
        assert!(selector.validate().is_ok());
        assert!(SlotSelector { attempts: 2, ..selector.clone() }.validate().is_err());
        assert!(SlotSelector { trial_slot: Some(UpdateSlot::A), ..selector }.validate().is_err());
    }

    fn key(byte: u8, hardware: bool, application: &str) -> SigningKey {
        SigningKey { key_id: Sha256Digest([byte; 32]), public_blob: vec![byte; 32],
            algorithm: if hardware { SigningAlgorithm::SkEd25519 } else { SigningAlgorithm::Ed25519 },
            application: hardware.then(|| application.to_owned()) }
    }

    #[test]
    fn root_threshold_and_role_separation() {
        let root = RootMetadata { schema_version: 1, version: 1, expires_at_unix: 100,
            root_threshold: 2, root_keys: vec![key(1, true, ROOT_SK_APPLICATION),
                key(2, false, ""), key(3, false, "")],
            target_key: key(4, true, TARGET_SK_APPLICATION), revoked_key_ids: vec![], frozen: false };
        assert!(root.validate().is_ok());
        assert_eq!(serde_json::from_str::<RootMetadata>(&serde_json::to_string(&root).unwrap()).unwrap(), root);
        assert!(RootMetadata { root_threshold: 1, ..root.clone() }.validate().is_err());
        let mut duplicate = root.clone();
        duplicate.root_keys[1] = duplicate.root_keys[0].clone();
        assert!(duplicate.validate().is_err());
        let mut wrong_role = root;
        wrong_role.target_key.application = Some(ROOT_SK_APPLICATION.into());
        assert!(wrong_role.validate().is_err());
        assert_ne!(ROOT_SSHSIG_NAMESPACE, RELEASE_SSHSIG_NAMESPACE);
        assert_ne!(RUNTIME_SSHSIG_NAMESPACE, RELEASE_SSHSIG_NAMESPACE);
        assert_ne!(EMERGENCY_SSHSIG_NAMESPACE, ROOT_SSHSIG_NAMESPACE);
        assert_eq!(SSHSIG_HASH_ALGORITHM, "sha512");
    }
}
