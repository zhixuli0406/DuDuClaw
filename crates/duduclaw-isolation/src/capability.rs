//! Gateway-owned MCP capability records and canonical signing bytes (§6.1–§6.3).
//! No token appears in argv/env, JSON RPC tool arguments or persistent workspaces.
use serde::{Deserialize, Serialize};
use crate::{bounded::{List, Text}, ids::{EmployeeId, RunId, Sha256Digest}, ContractError};

pub const CAPABILITY_ENCODING_VERSION: u16 = 2;
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
    /// CONTRACT CHANGE v2: live employee authority for Service/Connector calls.
    /// This never grants a host path or an execution workspace.
    EmployeeScope { id: EmployeeId },
    /// v2 confinement for File/A/E inputs or outputs alongside employee scope.
    /// It never narrows or expands Service/Connector employee-scope authority.
    Confined { resource: Box<ResourceGrant> },
    Employee { id: EmployeeId },
    Workspace { handle: Text<128> },
    Mailbox { id: Text<128> },
    Task { id: Text<128> },
    DatabaseTable { database: Text<128>, table: Text<128> },
    Wiki { namespace: Text<128> },
    Connector { account: Text<128>, binding: EmployeeBinding },
    Recipient { id: Text<256> },
    UrlOrigin { origin: Text<2048> },
    /// CONTRACT CHANGE (D-H2, tag 11): URL authority for the web tools of an
    /// employee-scope run. Valid only alongside `EmployeeScope` for the record's
    /// own employee, at most once, never `Confined`. A set of 1..=16 entries
    /// (see [`PublicOrigin`]). It never weakens the host fetch: DNS answers are
    /// pinned, non-public addresses are refused and redirects are not followed.
    PublicOrigins { origins: List<PublicOrigin, PUBLIC_ORIGINS_MAX> },
}

/// Most entries in one `PublicOrigins` grant.
pub const PUBLIC_ORIGINS_MAX: usize = 16;

/// Hostname suffixes that never name a public origin (RFC 6761/6762/7686/8375
/// and common private-network names). The fetch layer still classifies every
/// resolved address; this only keeps such names out of signed authority.
const NON_PUBLIC_TLDS: &[&str] = &["localhost", "local", "localdomain", "internal", "intranet",
    "lan", "home", "corp", "arpa", "onion", "test", "invalid", "example"];

/// One `PublicOrigins` entry, validated on construction and on decode.
///
/// * `*` — any public HTTPS origin on the default port. The host fetch decides
///   "public" per resolved address, never from the name.
/// * `https://<hostname>` — exactly that origin: scheme `https` only, default
///   port only (no `:port`), no userinfo, path, query or fragment; the hostname
///   is the canonical ASCII form (lowercase LDH labels, IDNs as `xn--`), at
///   least two labels, no trailing dot, top-level label starting with a letter
///   (so no IPv4 literal or WHATWG numeric host), and never an IP literal.
///   It matches only that exact host: there are no subdomain wildcards.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct PublicOrigin(String);

impl PublicOrigin {
    pub const ANY: &'static str = "*";

    pub fn parse(value: &str) -> Result<Self, ContractError> {
        if value == Self::ANY { return Ok(Self(value.to_owned())); }
        let host = value.strip_prefix("https://")
            .ok_or(ContractError::InvalidValue("public origin must be https://<host> or *"))?;
        let labels: Vec<&str> = host.split('.').collect();
        let label_ok = |label: &str| (1..=63).contains(&label.len())
            && label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !label.starts_with('-') && !label.ends_with('-');
        let tld = labels.last().copied().unwrap_or("");
        if host.is_empty() || host.len() > 253 || labels.len() < 2 || !labels.iter().copied().all(label_ok)
            || !tld.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
            || NON_PUBLIC_TLDS.contains(&tld) {
            return Err(ContractError::InvalidValue("public origin host"));
        }
        Ok(Self(value.to_owned()))
    }
    pub fn any() -> Self { Self(Self::ANY.to_owned()) }
    pub fn as_str(&self) -> &str { &self.0 }
    pub fn is_any(&self) -> bool { self.0 == Self::ANY }
    /// The exact hostname, or `None` for `*`.
    pub fn host(&self) -> Option<&str> { self.0.strip_prefix("https://") }
    /// Whether this entry authorizes the exact origin `origin` (never `*`).
    pub fn permits(&self, origin: &PublicOrigin) -> bool {
        !origin.is_any() && (self.is_any() || self == origin)
    }
}

impl<'de> Deserialize<'de> for PublicOrigin {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// Persistable capability claims (§6.1). Hashes/nonces are metadata, not bearers.
/// Epoch changes invalidate all old records after gateway restart; policy and
/// principal generations must be checked live on every call, not just at mint.
/// Decoding refuses a `PublicOrigins` grant that is not valid for the record
/// (see [`CapabilityRecord::check_public_origins`]); signing re-checks it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "CapabilityRecordWire")]
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

/// Wire shape of [`CapabilityRecord`]; same fields, checked on conversion.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityRecordWire {
    id: CapabilityId,
    employee: EmployeeId,
    run: RunId,
    task: Option<Text<128>>,
    principal: Principal,
    employee_binding: EmployeeBinding,
    lane: Lane,
    tools: List<Text<128>, 256>,
    resources: List<ResourceGrant, 128>,
    policy_revision: u64,
    gateway_epoch: [u8; 16],
    issued_at: u64,
    not_before: u64,
    expires_at: u64,
    nonce: [u8; 32],
    jti: [u8; 16],
}

impl TryFrom<CapabilityRecordWire> for CapabilityRecord {
    type Error = ContractError;
    fn try_from(wire: CapabilityRecordWire) -> Result<Self, ContractError> {
        let CapabilityRecordWire { id, employee, run, task, principal, employee_binding, lane, tools,
            resources, policy_revision, gateway_epoch, issued_at, not_before, expires_at, nonce, jti } = wire;
        let record = Self { id, employee, run, task, principal, employee_binding, lane, tools, resources,
            policy_revision, gateway_epoch, issued_at, not_before, expires_at, nonce, jti };
        record.check_public_origins()?;
        Ok(record)
    }
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
        ResourceGrant::EmployeeScope { id } => { out.push(9); out.extend_from_slice(&id.0); }
        ResourceGrant::Confined { resource } => { out.push(10); out.extend(resource_bytes(resource)); }
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
        ResourceGrant::PublicOrigins { origins } => {
            // A set: u32 count, then text entries in encoded-byte order (length
            // prefix first), like every other set. Duplicates are refused by
            // check_public_origins before encoding.
            out.push(11);
            let mut entries: Vec<&str> = origins.as_slice().iter().map(PublicOrigin::as_str).collect();
            entries.sort_by(|a, b| (a.len(), a.as_bytes()).cmp(&(b.len(), b.as_bytes())));
            out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
            for entry in entries { text_bytes(&mut out, entry); }
        }
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
    pub fn employee_scope(&self) -> bool {
        self.resources.as_slice().contains(&ResourceGrant::EmployeeScope { id: self.employee })
    }

    /// Concrete grants consumed by File/A/E adapters. EmployeeScope remains a
    /// separate marker; confined resources never become ambient service grants.
    pub fn io_resources(&self) -> impl Iterator<Item = &ResourceGrant> {
        self.resources.as_slice().iter().map(|grant| match grant {
            ResourceGrant::Confined { resource } => resource.as_ref(), _ => grant,
        })
    }

    /// A broad service grant must never accidentally erase narrower service grants.
    /// Workspace and origin grants remain explicit for A/E confinement; they do
    /// not narrow the employee-scope authority of Service/Connector handlers.
    pub fn validate_resource_mode(&self) -> Result<(), ContractError> {
        self.check_public_origins()?;
        let scopes: Vec<_> = self.resources.as_slice().iter().filter_map(|grant| match grant {
            ResourceGrant::EmployeeScope { id } => Some(*id), _ => None,
        }).collect();
        if !scopes.is_empty() && (scopes != [self.employee] || self.task.is_some()
            || self.resources.as_slice().iter().any(|grant| !matches!(grant,
                ResourceGrant::EmployeeScope { .. } | ResourceGrant::Employee { .. } | ResourceGrant::Workspace { .. }
                | ResourceGrant::UrlOrigin { .. } | ResourceGrant::Confined { .. } | ResourceGrant::PublicOrigins { .. }))) {
            return Err(ContractError::InvalidValue("ambiguous employee-scope resource policy"));
        }
        for grant in self.resources.as_slice() {
            if let ResourceGrant::Confined { resource } = grant {
                if scopes.is_empty() || matches!(resource.as_ref(), ResourceGrant::Confined { .. }
                    | ResourceGrant::EmployeeScope { .. } | ResourceGrant::Employee { .. }
                    | ResourceGrant::PublicOrigins { .. }) {
                    return Err(ContractError::InvalidValue("invalid confined resource"));
                }
            }
        }
        Ok(())
    }

    /// The `PublicOrigins` rules, applied on decode and before signing: only in
    /// an employee-scope record (EmployeeScope for this record's own employee,
    /// no task), at most one such grant, never `Confined`, a non-empty set
    /// without duplicates, and `*` only as the sole entry.
    pub fn check_public_origins(&self) -> Result<(), ContractError> {
        let mut found = 0;
        for grant in self.resources.as_slice() {
            match grant {
                ResourceGrant::Confined { resource } if matches!(resource.as_ref(), ResourceGrant::PublicOrigins { .. }) =>
                    return Err(ContractError::InvalidValue("confined public origins")),
                ResourceGrant::PublicOrigins { origins } => {
                    found += 1;
                    let entries = origins.as_slice();
                    if entries.is_empty() || (entries.len() > 1 && entries.iter().any(PublicOrigin::is_any))
                        || entries.iter().enumerate().any(|(index, entry)| entries[..index].contains(entry)) {
                        return Err(ContractError::InvalidValue("public origins set"));
                    }
                }
                _ => {}
            }
        }
        if found > 1 || (found == 1 && (!self.employee_scope() || self.task.is_some()
            || self.resources.as_slice().iter().any(|grant|
                matches!(grant, ResourceGrant::EmployeeScope { id } if *id != self.employee)))) {
            return Err(ContractError::InvalidValue("public origins require own employee scope"));
        }
        Ok(())
    }

    /// Stable v2 bytes to MAC/sign. Times are Unix seconds. Tags and field order
    /// are protocol constants; do not replace this with JSON serialization (§6.1).
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ContractError> {
        self.validate_resource_mode()?;
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
/// Host-owned live verifier attached by the boundary, never supplied by MCP.
/// Implementations must compare this exact record with the active connection
/// and re-read run, principal and policy authority immediately before I/O.
pub trait LiveRequestVerifier: Send + Sync {
    fn verify_live(&self, record: &CapabilityRecord, now: u64) -> Result<(), VerificationError>;
}

#[derive(Clone)]
pub struct RequestContext {
    record: CapabilityRecord,
    live: Option<std::sync::Arc<dyn LiveRequestVerifier>>,
}
impl std::fmt::Debug for RequestContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestContext").field("record", &self.record)
            .field("live_authority_attached", &self.live.is_some()).finish()
    }
}
impl RequestContext {
    /// Record-only contexts cannot authorize irreversible I/O. This constructor
    /// remains for trusted policy evaluation and contract fixtures.
    pub fn from_verified_record(record: CapabilityRecord) -> Self { Self { record, live: None } }
    /// Trusted verifier integration seam, not a callable/deserializable RPC.
    pub fn with_live_verifier(record: CapabilityRecord, live: std::sync::Arc<dyn LiveRequestVerifier>) -> Self {
        Self { record, live: Some(live) }
    }
    pub fn record(&self) -> &CapabilityRecord { &self.record }
    pub fn verify_live(&self, now: u64) -> Result<(), VerificationError> {
        if now < self.record.not_before || now >= self.record.expires_at {
            return Err(VerificationError::InvalidTime);
        }
        self.live.as_ref().ok_or(VerificationError::Unavailable)?.verify_live(&self.record, now)
    }
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
            "64756475636c61772d6d63702d6361706162696c69747900", "0002",
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
    fn employee_scope_v2_is_mac_covered_and_cannot_shadow_narrow_grants() {
        let mut scoped = record();
        scoped.task = None;
        scoped.resources = List::new(vec![ResourceGrant::EmployeeScope { id: scoped.employee }]).unwrap();
        let bytes = scoped.canonical_bytes().unwrap();
        assert_eq!(&bytes[CAPABILITY_DOMAIN.len()..CAPABILITY_DOMAIN.len()+2], &[0, 2]);
        let mut explicit = scoped.clone();
        explicit.resources = List::new(vec![ResourceGrant::Employee { id: explicit.employee }]).unwrap();
        assert_ne!(bytes, explicit.canonical_bytes().unwrap());
        scoped.resources = List::new(vec![ResourceGrant::EmployeeScope { id: scoped.employee },
            ResourceGrant::Task { id: Text::new("task-a".into()).unwrap() }]).unwrap();
        assert!(scoped.canonical_bytes().is_err());
        scoped.resources = List::new(vec![ResourceGrant::EmployeeScope { id: EmployeeId([9; 16]) }]).unwrap();
        assert!(scoped.canonical_bytes().is_err());
    }

    #[test]
    fn confined_resource_encoding_rejects_ambiguous_or_nested_modes() {
        let confined = ResourceGrant::Confined { resource: Box::new(ResourceGrant::Wiki { namespace: Text::new("n".into()).unwrap() }) };
        let mut scoped = record();
        scoped.task = None;
        scoped.resources = List::new(vec![ResourceGrant::EmployeeScope { id: scoped.employee }, confined.clone()]).unwrap();
        let original = scoped.canonical_bytes().unwrap();
        scoped.resources = List::new(vec![ResourceGrant::EmployeeScope { id: scoped.employee },
            ResourceGrant::Confined { resource: Box::new(ResourceGrant::Wiki { namespace: Text::new("other".into()).unwrap() }) }]).unwrap();
        assert_ne!(original, scoped.canonical_bytes().unwrap());
        scoped.resources = List::new(vec![confined.clone()]).unwrap();
        assert!(scoped.canonical_bytes().is_err());
        scoped.resources = List::new(vec![ResourceGrant::EmployeeScope { id: scoped.employee },
            ResourceGrant::Confined { resource: Box::new(confined) }]).unwrap();
        assert!(scoped.canonical_bytes().is_err());
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
            (ResourceGrant::Confined { resource: Box::new(ResourceGrant::Wiki { namespace: Text::new("n".into()).unwrap() }) }, "0a05000000016e"),
            (ResourceGrant::EmployeeScope { id: EmployeeId([2; 16]) }, "0902020202020202020202020202020202"),
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
    fn origins(entries: &[&str]) -> ResourceGrant {
        ResourceGrant::PublicOrigins { origins: List::new(entries.iter()
            .map(|entry| PublicOrigin::parse(entry).unwrap()).collect()).unwrap() }
    }
    fn public_scoped(grant: ResourceGrant) -> CapabilityRecord {
        let mut scoped = record();
        scoped.task = None;
        scoped.resources = List::new(vec![ResourceGrant::EmployeeScope { id: scoped.employee }, grant]).unwrap();
        scoped
    }
    #[test]
    fn public_origins_requires_employee_scope() {
        let scoped = public_scoped(origins(&["*"]));
        scoped.canonical_bytes().unwrap();
        let json = serde_json::to_value(&scoped).unwrap();
        assert_eq!(serde_json::from_value::<CapabilityRecord>(json.clone()).unwrap(), scoped);
        // Decode refuses the grant without the own employee's scope.
        let mut unscoped = json.clone();
        unscoped["resources"].as_array_mut().unwrap().retain(|grant| grant["kind"] != "employee_scope");
        assert!(serde_json::from_value::<CapabilityRecord>(unscoped).is_err());
        let mut tasked = json.clone();
        tasked["task"] = serde_json::json!("t");
        assert!(serde_json::from_value::<CapabilityRecord>(tasked).is_err());
        let mut confined = json;
        confined["resources"][1] = serde_json::json!({"kind":"confined","resource":{"kind":"public_origins","origins":["*"]}});
        assert!(serde_json::from_value::<CapabilityRecord>(confined).is_err());
        // Constructed records are refused before signing, with the same rules.
        let mut bad = record();
        bad.resources = List::new(vec![origins(&["*"])]).unwrap();
        assert!(bad.canonical_bytes().is_err() && bad.validate_resource_mode().is_err());
        bad.task = None;
        bad.resources = List::new(vec![ResourceGrant::Employee { id: bad.employee }, origins(&["*"])]).unwrap();
        assert!(bad.canonical_bytes().is_err());
        bad.resources = List::new(vec![ResourceGrant::EmployeeScope { id: EmployeeId([9; 16]) }, origins(&["*"])]).unwrap();
        assert!(bad.canonical_bytes().is_err());
        bad.resources = List::new(vec![ResourceGrant::EmployeeScope { id: bad.employee },
            ResourceGrant::Confined { resource: Box::new(origins(&["*"])) }]).unwrap();
        assert!(bad.canonical_bytes().is_err());
        bad.resources = List::new(vec![ResourceGrant::EmployeeScope { id: bad.employee },
            origins(&["https://a.co"]), origins(&["https://b.co"])]).unwrap();
        assert!(bad.canonical_bytes().is_err(), "at most one PublicOrigins grant");
        for set in [&[][..], &["*", "https://a.co"][..], &["https://a.co", "https://a.co"][..]] {
            assert!(public_scoped(origins(set)).canonical_bytes().is_err(), "{set:?}");
        }
    }
    #[test]
    fn public_origins_mac_covered() {
        let cases = [
            (origins(&["*"]), "0b00000001000000012a"),
            (origins(&["https://b.co", "https://a.co"]), concat!("0b00000002",
                "0000000c68747470733a2f2f612e636f", "0000000c68747470733a2f2f622e636f")),
        ];
        for (grant, expected) in &cases {
            let actual: String = resource_bytes(grant).iter().map(|b| format!("{b:02x}")).collect();
            assert_eq!(&actual, expected);
            let bytes = public_scoped(grant.clone()).canonical_bytes().unwrap();
            let encoded = resource_bytes(grant);
            assert!(bytes.windows(encoded.len()).any(|window| window == encoded.as_slice()));
        }
        // Order inside the set is not authority; content is.
        let a = public_scoped(origins(&["https://a.co", "https://b.co"])).canonical_bytes().unwrap();
        assert_eq!(a, public_scoped(origins(&["https://b.co", "https://a.co"])).canonical_bytes().unwrap());
        for other in [&["https://a.co"][..], &["https://a.co", "https://c.co"][..], &["*"][..]] {
            assert_ne!(a, public_scoped(origins(other)).canonical_bytes().unwrap(), "{other:?}");
        }
        // Distinct from an exact UrlOrigin and from the scope alone.
        let url = ResourceGrant::UrlOrigin { origin: Text::new("https://a.co".into()).unwrap() };
        assert_ne!(resource_bytes(&url)[0], resource_bytes(&origins(&["https://a.co"]))[0]);
        let mut scope_only = public_scoped(origins(&["*"]));
        scope_only.resources = List::new(vec![ResourceGrant::EmployeeScope { id: scope_only.employee }]).unwrap();
        assert_ne!(scope_only.canonical_bytes().unwrap(), public_scoped(origins(&["*"])).canonical_bytes().unwrap());
    }
    #[test]
    fn public_origins_rejects_ip_literal_and_userinfo() {
        for valid in ["*", "https://example.com", "https://a.co", "https://xn--r8jz45g.jp", "https://api-1.example.org"] {
            assert!(PublicOrigin::parse(valid).is_ok(), "{valid}");
        }
        for invalid in ["", "**", "https://*.example.com", "*.example.com", "http://example.com", "example.com",
            "https://127.0.0.1", "https://1.2.3.4", "https://0x7f.1", "https://8.8.8.8", "https://[::1]",
            "https://[2001:db8::1]", "https://user@example.com", "https://user:pw@example.com",
            "https://example.com:443", "https://example.com:8443", "https://example.com/", "https://example.com/p",
            "https://example.com?q", "https://example.com#f", "https://EXAMPLE.com", "https://example.com.",
            "https://com", "https://localhost", "https://a.localhost", "https://metadata.google.internal",
            "https://printer.local", "https://router.home.arpa", "https://x.test", "https://-a.com",
            "https://a-.com", "https://a..com", "https://例え.jp", "https://ex ample.com", "https://a.co\n"] {
            assert!(PublicOrigin::parse(invalid).is_err(), "{invalid:?}");
            let grant = serde_json::json!({"kind":"public_origins","origins":[invalid]});
            assert!(serde_json::from_value::<ResourceGrant>(grant).is_err(), "{invalid:?}");
        }
        let any = PublicOrigin::any();
        let exact = PublicOrigin::parse("https://a.co").unwrap();
        assert!(any.permits(&exact) && exact.permits(&exact) && !any.permits(&any));
        assert!(!exact.permits(&PublicOrigin::parse("https://b.a.co").unwrap()));
        assert_eq!(exact.host(), Some("a.co"));
        assert_eq!(any.host(), None);
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
            "64756475636c61772d6d63702d6361706162696c69747900", "0002",
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
