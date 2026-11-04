//! Gateway-owned MCP capability records and canonical signing bytes (§6.1–§6.3).
//! No token appears in argv/env, JSON RPC tool arguments or persistent workspaces.
use serde::{Deserialize, Serialize};
use crate::{bounded::{List, Text}, ids::{EmployeeId, RunId, Sha256Digest}, ContractError};

pub const CAPABILITY_ENCODING_VERSION: u16 = 1;
pub const CAPABILITY_DOMAIN: &[u8] = b"duduclaw-mcp-capability\0";

/// Hash of a high-entropy capability; server storage never retains its bearer (§6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CapabilityId(pub Sha256Digest);

/// Human or background authority; an unknown principal cannot mint a capability (§6.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Principal {
    Human { user_id: Text<128>, generation: u64 },
    Background { tenant_id: Text<128>, generation: u64 },
}

/// Immutable lifetime binding; Shared never qualifies for personal credentials (§6.1.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case", deny_unknown_fields)]
pub enum EmployeeBinding { Personal(Text<128>), Shared(Text<128>) }

/// Execution lane only narrows the tool grants (§6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lane { Standard, Explore }

/// Resource identity, never a caller-chosen host path or fd integer (§6.3).
/// Workspace handles resolve to gateway-owned dirfds. Origin strings require
/// canonical URL-origin validation by D before minting; this type does not parse URLs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResourceGrant {
    Employee { id: EmployeeId },
    Workspace { handle: Text<128> },
    Mailbox { id: Text<128> },
    Task { id: Text<128> },
    DatabaseTable { database: Text<128>, table: Text<128> },
    Wiki { namespace: Text<128> },
    Connector { account: Text<128>, binding: EmployeeBinding },
    Recipient { id: Text<256> },
    UrlOrigin { origin: Text<2048> },
}

/// Persistable capability claims (§6.1). Hashes/nonces are metadata, not bearers.
/// Epoch changes invalidate all old records after gateway restart; policy and
/// principal generations must be checked live on every call, not just at mint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityRecord {
    pub id: CapabilityId,
    pub employee: EmployeeId,
    pub run: RunId,
    pub task: Option<Text<128>>,
    pub principal: Principal,
    pub employee_binding: EmployeeBinding,
    pub lane: Lane,
    pub tools: List<Text<128>, 256>,
    pub resources: List<ResourceGrant, 128>,
    pub policy_revision: u64,
    pub gateway_epoch: [u8; 16],
    pub issued_at: u64,
    pub not_before: u64,
    pub expires_at: u64,
    pub nonce: [u8; 32],
    pub jti: [u8; 16],
}

// Canonical encoding is independent of JSON and serde enum naming. Integers are
// unsigned big-endian; text = u32 byte length + exact UTF-8 (no normalization);
// optional task = u8 0/1 followed by text if present; enums use the tags below.
// Sets are lexicographically sorted encoded entries, duplicates rejected; count
// is u32. Fixed bytes are unprefixed. Field order is exactly canonical_bytes().
fn text_bytes(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(&(text.len() as u32).to_be_bytes());
    out.extend_from_slice(text.as_bytes());
}
fn binding_bytes(out: &mut Vec<u8>, binding: &EmployeeBinding) {
    match binding {
        EmployeeBinding::Personal(id) => { out.push(0); text_bytes(out, id.as_str()); }
        EmployeeBinding::Shared(id) => { out.push(1); text_bytes(out, id.as_str()); }
    }
}
fn resource_bytes(resource: &ResourceGrant) -> Vec<u8> {
    let mut out = Vec::new();
    match resource {
        ResourceGrant::Employee { id } => { out.push(0); out.extend_from_slice(&id.0); }
        ResourceGrant::Workspace { handle } => { out.push(1); text_bytes(&mut out, handle.as_str()); }
        ResourceGrant::Mailbox { id } => { out.push(2); text_bytes(&mut out, id.as_str()); }
        ResourceGrant::Task { id } => { out.push(3); text_bytes(&mut out, id.as_str()); }
        ResourceGrant::DatabaseTable { database, table } => {
            out.push(4); text_bytes(&mut out, database.as_str()); text_bytes(&mut out, table.as_str());
        }
        ResourceGrant::Wiki { namespace } => { out.push(5); text_bytes(&mut out, namespace.as_str()); }
        ResourceGrant::Connector { account, binding } => {
            out.push(6); text_bytes(&mut out, account.as_str()); binding_bytes(&mut out, binding);
        }
        ResourceGrant::Recipient { id } => { out.push(7); text_bytes(&mut out, id.as_str()); }
        ResourceGrant::UrlOrigin { origin } => { out.push(8); text_bytes(&mut out, origin.as_str()); }
    }
    out
}
fn set_bytes(out: &mut Vec<u8>, mut entries: Vec<Vec<u8>>) -> Result<(), ContractError> {
    entries.sort();
    if entries.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(ContractError::InvalidValue("duplicate capability grant"));
    }
    out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for entry in entries { out.extend_from_slice(&entry); }
    Ok(())
}

impl CapabilityRecord {
    /// Stable v1 bytes to MAC/sign. Times are Unix seconds. Tags and field order
    /// are protocol constants; do not replace this with JSON serialization (§6.1).
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ContractError> {
        if self.issued_at > self.not_before || self.not_before >= self.expires_at {
            return Err(ContractError::InvalidValue("capability validity interval"));
        }
        let mut out = CAPABILITY_DOMAIN.to_vec();
        out.extend_from_slice(&CAPABILITY_ENCODING_VERSION.to_be_bytes());
        out.extend_from_slice(&(self.id.0).0);
        out.extend_from_slice(&self.employee.0);
        out.extend_from_slice(&self.run.0);
        match &self.task {
            None => out.push(0),
            Some(task) => { out.push(1); text_bytes(&mut out, task.as_str()); }
        }
        match &self.principal {
            Principal::Human { user_id, generation } => {
                out.push(0); text_bytes(&mut out, user_id.as_str()); out.extend_from_slice(&generation.to_be_bytes());
            }
            Principal::Background { tenant_id, generation } => {
                out.push(1); text_bytes(&mut out, tenant_id.as_str()); out.extend_from_slice(&generation.to_be_bytes());
            }
        }
        binding_bytes(&mut out, &self.employee_binding);
        out.push(match self.lane { Lane::Standard => 0, Lane::Explore => 1 });
        let tools = self.tools.as_slice().iter().map(|tool| {
            let mut bytes = Vec::new(); text_bytes(&mut bytes, tool.as_str()); bytes
        }).collect();
        set_bytes(&mut out, tools)?;
        set_bytes(&mut out, self.resources.as_slice().iter().map(resource_bytes).collect())?;
        out.extend_from_slice(&self.policy_revision.to_be_bytes());
        out.extend_from_slice(&self.gateway_epoch);
        for value in [self.issued_at, self.not_before, self.expires_at] { out.extend_from_slice(&value.to_be_bytes()); }
        out.extend_from_slice(&self.nonce);
        out.extend_from_slice(&self.jti);
        Ok(out)
    }

    /// Checks the lifetime 1:1 identity condition only. D must also check live
    /// binding constraints, credential ownership, revocation and granted resources.
    pub fn personal_owner_matches(&self, credential_owner: &str) -> bool {
        match (&self.employee_binding, &self.principal) {
            (EmployeeBinding::Personal(owner), Principal::Human { user_id, .. }) =>
                owner.as_str() == credential_owner && user_id.as_str() == credential_owner,
            _ => false,
        }
    }
}

/// Immutable handler context constructed by a trusted verifier, never deserialized
/// from tool arguments (§6.3). The constructor is an implementer trust boundary,
/// not evidence that verification occurred; only D's verifier may call it.
#[derive(Debug, Clone)]
pub struct RequestContext { record: CapabilityRecord }
impl RequestContext {
    pub fn from_verified_record(record: CapabilityRecord) -> Self { Self { record } }
    pub fn record(&self) -> &CapabilityRecord { &self.record }
}

/// Host-owned connection binding; must never be populated from a frame (§6.1).
#[derive(Debug, Clone, Copy)]
pub struct ConnectionBinding {
    pub capability: CapabilityId,
    pub employee: EmployeeId,
    pub run: RunId,
    pub gateway_epoch: [u8; 16],
}

/// Opaque authenticator, not a bearer token; avoid accidental log exposure (§6.1).
pub struct CapabilityProof<'a>(pub &'a [u8]);

/// Non-sensitive rejection reasons (§4.4, §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum VerificationError {
    #[error("capability rejected")] Invalid,
    #[error("capability expired or not yet valid")] InvalidTime,
    #[error("capability revoked")] Revoked,
    #[error("capability connection mismatch")] BindingMismatch,
    #[error("capability verification unavailable")] Unavailable,
}

/// D implements MAC/key handling and constant-time verification (§6.1, §6.3).
/// Verify canonical_bytes, validity, live policy/principal generation, revocation,
/// run liveness and connection binding on every request. Restart invalidates epoch.
/// An unreadable store denies. Never take identity/root/cwd from tool arguments.
pub trait CapabilityVerifier: Send + Sync {
    fn verify(&self, record: &CapabilityRecord, proof: CapabilityProof<'_>,
        connection: &ConnectionBinding, now_unix_seconds: u64) -> Result<RequestContext, VerificationError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record() -> CapabilityRecord {
        CapabilityRecord {
            id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
            task: Some(Text::new("t".into()).unwrap()),
            principal: Principal::Human { user_id: Text::new("u".into()).unwrap(), generation: 4 },
            employee_binding: EmployeeBinding::Personal(Text::new("u".into()).unwrap()), lane: Lane::Explore,
            tools: List::new(vec![Text::new("x".into()).unwrap()]).unwrap(),
            resources: List::new(vec![ResourceGrant::Workspace { handle: Text::new("w".into()).unwrap() }]).unwrap(),
            policy_revision: 5, gateway_epoch: [6; 16], issued_at: 7, not_before: 8, expires_at: 9,
            nonce: [10; 32], jti: [11; 16],
        }
    }
    #[test]
    fn canonical_encoding_golden_bytes() {
        let actual: String = record().canonical_bytes().unwrap().iter().map(|b| format!("{b:02x}")).collect();
        let expected = concat!(
            "64756475636c61772d6d63702d6361706162696c69747900", "0001",
            "0101010101010101010101010101010101010101010101010101010101010101",
            "02020202020202020202020202020202", "03030303030303030303030303030303",
            "010000000174", "0000000001750000000000000004", "000000000175", "01",
            "000000010000000178", "00000001010000000177", "0000000000000005",
            "06060606060606060606060606060606", "000000000000000700000000000000080000000000000009",
            "0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a",
            "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b"
        );
        assert_eq!(actual, expected);
    }
    #[test]
    fn serde_round_trip_and_canonical_set_order() {
        let mut a = record();
        assert_eq!(serde_json::from_slice::<CapabilityRecord>(&serde_json::to_vec(&a).unwrap()).unwrap(), a);
        a.tools = List::new(vec![Text::new("a".into()).unwrap(), Text::new("b".into()).unwrap()]).unwrap();
        let mut b = a.clone();
        b.tools = List::new(vec![Text::new("b".into()).unwrap(), Text::new("a".into()).unwrap()]).unwrap();
        assert_eq!(a.canonical_bytes(), b.canonical_bytes());
        b.tools = List::new(vec![Text::new("a".into()).unwrap(), Text::new("a".into()).unwrap()]).unwrap();
        assert!(b.canonical_bytes().is_err());
        assert!(a.personal_owner_matches("u"));
        assert!(!a.personal_owner_matches("v"));
        a.employee_binding = EmployeeBinding::Shared(Text::new("u".into()).unwrap());
        assert!(!a.personal_owner_matches("u"));
        a.not_before = a.expires_at;
        assert!(a.canonical_bytes().is_err());
    }
    #[test]
    fn signed_bindings_change_the_encoding() {
        let a = record();
        let bytes = a.canonical_bytes().unwrap();
        let mut variants = vec![a.clone(); 7];
        variants[0].run = RunId([9; 16]);
        variants[1].employee = EmployeeId([9; 16]);
        variants[2].policy_revision += 1;
        variants[3].gateway_epoch = [9; 16];
        variants[4].nonce = [9; 32];
        variants[5].jti = [9; 16];
        variants[6].expires_at += 1;
        for changed in variants { assert_ne!(changed.canonical_bytes().unwrap(), bytes); }
    }
    #[test]
    fn canonical_resource_tags_have_golden_bytes() {
        let cases = [
            (ResourceGrant::Employee { id: EmployeeId([2; 16]) }, "0002020202020202020202020202020202"),
            (ResourceGrant::Workspace { handle: Text::new("w".into()).unwrap() }, "010000000177"),
            (ResourceGrant::Mailbox { id: Text::new("m".into()).unwrap() }, "02000000016d"),
            (ResourceGrant::Task { id: Text::new("t".into()).unwrap() }, "030000000174"),
            (ResourceGrant::DatabaseTable { database: Text::new("d".into()).unwrap(), table: Text::new("t".into()).unwrap() }, "0400000001640000000174"),
            (ResourceGrant::Wiki { namespace: Text::new("n".into()).unwrap() }, "05000000016e"),
            (ResourceGrant::Connector { account: Text::new("c".into()).unwrap(), binding: EmployeeBinding::Personal(Text::new("u".into()).unwrap()) }, "060000000163000000000175"),
            (ResourceGrant::Connector { account: Text::new("c".into()).unwrap(), binding: EmployeeBinding::Shared(Text::new("t".into()).unwrap()) }, "060000000163010000000174"),
            (ResourceGrant::Recipient { id: Text::new("r".into()).unwrap() }, "070000000172"),
            (ResourceGrant::UrlOrigin { origin: Text::new("https://e".into()).unwrap() }, "080000000968747470733a2f2f65"),
        ];
        for (resource, expected) in cases {
            let actual: String = resource_bytes(&resource).iter().map(|b| format!("{b:02x}")).collect();
            assert_eq!(actual, expected);
        }
    }
    #[test]
    fn background_capability_golden_bytes() {
        let mut value = record();
        value.task = None;
        value.principal = Principal::Background { tenant_id: Text::new("t".into()).unwrap(), generation: 4 };
        value.employee_binding = EmployeeBinding::Shared(Text::new("t".into()).unwrap());
        value.lane = Lane::Standard;
        let actual: String = value.canonical_bytes().unwrap().iter().map(|b| format!("{b:02x}")).collect();
        let expected = concat!(
            "64756475636c61772d6d63702d6361706162696c69747900", "0001",
            "0101010101010101010101010101010101010101010101010101010101010101",
            "02020202020202020202020202020202", "03030303030303030303030303030303",
            "00", "0100000001740000000000000004", "010000000174", "00",
            "000000010000000178", "00000001010000000177", "0000000000000005",
            "06060606060606060606060606060606", "000000000000000700000000000000080000000000000009",
            "0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a",
            "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b"
        );
        assert_eq!(actual, expected);
    }
}
