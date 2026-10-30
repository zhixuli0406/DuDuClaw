//! H1 raw-row/control core. It owns logical Google metadata in the existing
//! token file; it is not a second token vault. Top-level client/config/callback
//! writers are NOT wired by this first source-only phase.
use super::{McpOAuthToken, TOKEN_FILE};
use crate::google_identity::home_source::{self, ObservedSource, SourceBinding, SourceGuard};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::Path,
};

#[path = "home_publication.rs"]
mod publication;
#[path = "home_revoke_presence.rs"]
mod revoke_presence;
pub(super) use revoke_presence::read as read_revoke_presence;
pub(super) use revoke_presence::read_access_token as read_revoke_access_token;
#[path = "home_raw_array.rs"]
pub(super) mod raw_array;
#[cfg(test)]
#[path = "home_revoke_presence_tests.rs"]
mod revoke_presence_tests;
#[cfg(test)]
#[path = "home_store_tests.rs"]
mod tests;
use publication::{ConnectProof, PublicationOp};
pub(crate) use publication::{
    Planned, PublicationInspection, publication_transaction, publication_transaction_checked,
};
pub(crate) fn ensure_publication_unblocked(g: &SourceGuard) -> Result<()> {
    publication::ensure_unblocked(g.home())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Error {
    Invalid,
    Stale,
    NotReady,
    PendingWiring,
    Io,
    PublicationBlocked,
    NotPublished,
    PublishedDurabilityUnconfirmed,
    CommittedCleanupUnconfirmed,
}
pub(crate) type Result<T> = std::result::Result<T, Error>;
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Invalid => "Google Home metadata is invalid.",
            Self::Stale => "Google Home authority changed.",
            Self::NotReady => "Google Home source is not ready.",
            Self::PendingWiring => "Google Home source writer integration is pending.",
            Self::Io => "Google Home metadata storage failed.",
            Self::PublicationBlocked => "Google Home publication requires explicit recovery.",
            Self::NotPublished => "Google Home change was not published; inspect its state before retrying.",
            Self::PublishedDurabilityUnconfirmed => "Google Home publication durability is unconfirmed; do not repeat the operation.",
            Self::CommittedCleanupUnconfirmed => "Google Home data was committed but cleanup is unconfirmed; inspect its state and do not repeat the operation.",
        })
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RawDigest([u8; 32]);
impl RawDigest {
    pub(crate) fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }
}
impl std::fmt::Debug for RawDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RawDigest(..)")
    }
}
fn digest(value: &Value) -> Result<RawDigest> {
    let bytes = zeroize::Zeroizing::new(serde_json::to_vec(value).map_err(|_| Error::Invalid)?);
    Ok(RawDigest::of(&bytes))
}
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn valid_id(s: &str) -> bool {
    uuid::Uuid::parse_str(s).is_ok_and(|u| u.to_string() == s)
}
pub(crate) fn normalized_scopes(mut scopes: Vec<String>) -> Result<Vec<String>> {
    if scopes.len() > 128
        || scopes
            .iter()
            .any(|s| s.is_empty() || s.len() > 2048 || s.chars().any(char::is_control))
    {
        return Err(Error::Invalid);
    }
    scopes.sort();
    scopes.dedup();
    Ok(scopes)
}
/// A deterministic witness for the existing native any-of policy. This is
/// nonsecret metadata, not a bearer restriction or a selection authority.
fn singleton_native_scope(tool: &str, actual: Vec<String>) -> Result<String> {
    if tool == "google_status"
        || duduclaw_core::google_identity::native_google_scope(tool).is_none()
    {
        return Err(Error::Invalid);
    }
    normalized_scopes(actual)?
        .into_iter()
        .find(|scope| crate::google_personal::scopes::permitted(tool, std::slice::from_ref(scope)))
        .ok_or(Error::Invalid)
}

/// Select from the actual row while the SourceOwner holds its source/config/
/// client locks. The caller fixes this witness in its one original receipt.
pub(crate) fn native_scope_witness(
    g: &SourceGuard,
    source: &ObservedSource,
    tool: &str,
) -> Result<String> {
    transaction(g, |rows| {
        let raw = rows.google().ok_or(Error::NotReady)?;
        let scopes: Vec<String> =
            serde_json::from_value(raw.get("scopes").cloned().ok_or(Error::Invalid)?)
                .map_err(|_| Error::Invalid)?;
        let witness = singleton_native_scope(tool, scopes)?;
        rows.require_oauth_scopes(std::slice::from_ref(&witness), source, host_time_now(g))?;
        Ok(witness)
    })
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Credential {
    pub(crate) credential_id: String,
    pub(crate) logical_epoch: u64,
    pub(crate) token_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) google_data_consent: Option<crate::google_data_consent::Evidence>,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum State {
    Ready,
    Pending,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Pending,
    Exchanging,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TicketRecord {
    ticket_id: String,
    state_hash: RawDigest,
    generation: String,
    canonical_home: RawDigest,
    client_stamp: RawDigest,
    source_stamp: RawDigest,
    expires_at: u64,
    phase: Phase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    google_data_consent: Option<crate::google_data_consent::Evidence>,
}
impl TicketRecord {
    fn matches_source(&self, source: &SourceBinding) -> bool {
        &self.canonical_home == source.home_stamp()
            && Some(&self.client_stamp) == source.client_stamp()
            && &self.source_stamp == source.source_stamp()
    }
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MutationKind {
    ConnectStart,
    Config,
    ConnectFinish,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PayloadExpectation {
    digest: RawDigest,
    credential_id: String,
    logical_epoch: u64,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Mutation {
    mutation_id: String,
    kind: MutationKind,
    expected_before: SourceBinding,
    expected_after: SourceBinding,
    payload: Option<PayloadExpectation>,
    /// The existing Pending row is the cross-file barrier. Only nonsecret
    /// closed file kind and before/after digests are persisted here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_write: Option<SourceFileProof>,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SourceFileKind {
    Client,
    Config,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceFileProof {
    kind: SourceFileKind,
    before_digest: RawDigest,
    after_digest: RawDigest,
}
pub(super) fn read_client_snapshot(g: &SourceGuard) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>> {
    publication::read_source_file(g.home(), SourceFileKind::Client)
}
pub(super) fn prepare_client_write(
    before: Option<&[u8]>,
    after: Vec<u8>,
) -> Result<publication::PreparedSourceWrite> {
    publication::PreparedSourceWrite::client(before, after)
}
#[cfg(test)]
pub(crate) fn record_source_fence(g: &SourceGuard) {
    test_probe::source_fence(g.home());
}
pub(crate) fn read_config_snapshot(g: &SourceGuard) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>> {
    publication::read_source_file(g.home(), SourceFileKind::Config)
}
pub(crate) fn prepare_config_write(
    before: Option<&[u8]>,
    after: Vec<u8>,
) -> Result<publication::PreparedSourceWrite> {
    publication::PreparedSourceWrite::config(before, after)
}
pub(crate) fn publish_unrelated_source(
    g: &SourceGuard,
    write: publication::PreparedSourceWrite,
) -> Result<()> {
    publication::ensure_unblocked(g.home())?;
    write.publish(g.home())
}
pub(crate) fn wall_time_now(home: &Path) -> u64 {
    publication::host_now(home)
}
pub(crate) fn host_time_now(g: &SourceGuard) -> u64 {
    publication::host_now(g.home())
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Control {
    schema: u32,
    source_generation: String,
    state: State,
    credential: Option<Credential>,
    active_connect: Option<TicketRecord>,
    pending_mutation: Option<Mutation>,
}
impl Control {
    fn new(payload: bool) -> Self {
        Self {
            schema: 1,
            source_generation: id(),
            state: State::Ready,
            credential: payload.then(|| Credential {
                credential_id: id(),
                logical_epoch: 1,
                token_revision: 1,
                google_data_consent: None,
            }),
            active_connect: None,
            pending_mutation: None,
        }
    }
    fn valid(&self) -> bool {
        self.schema == 1
            && valid_id(&self.source_generation)
            && self.credential.as_ref().is_none_or(|c| {
                valid_id(&c.credential_id)
                    && c.logical_epoch > 0
                    && c.token_revision > 0
                    && c.google_data_consent
                        .as_ref()
                        .is_none_or(crate::google_data_consent::Evidence::is_well_formed)
            })
            && self.active_connect.as_ref().is_none_or(|t| {
                valid_id(&t.ticket_id)
                    && t.generation == self.source_generation
                    && t.expires_at > 0
                    && t.google_data_consent
                        .as_ref()
                        .is_none_or(crate::google_data_consent::Evidence::is_well_formed)
            })
            && self.pending_mutation.as_ref().is_none_or(|m| {
                valid_id(&m.mutation_id)
                    && m.payload
                        .as_ref()
                        .is_none_or(|p| valid_id(&p.credential_id) && p.logical_epoch > 0)
            })
            && match self.state {
                State::Ready => self.pending_mutation.is_none(),
                State::Pending => self.pending_mutation.is_some(),
            }
    }
    fn ready(&self) -> Result<()> {
        if self.state == State::Ready && self.pending_mutation.is_none() {
            Ok(())
        } else {
            Err(Error::NotReady)
        }
    }
}
pub(crate) struct Metadata {
    pub(crate) source_generation: String,
    pub(crate) credential: Option<Credential>,
}
pub(crate) struct StartConnect {
    generation: String,
    mutation_id: String,
    ticket: TicketRecord,
}
#[derive(Clone)]
pub(crate) struct GoogleConnectTicket {
    record: TicketRecord,
}
impl std::fmt::Debug for GoogleConnectTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GoogleConnectTicket(..)")
    }
}
/// NonClone, nonDebug, nonSerialize: future exchange may resolve ONLY this
/// captured raw client, never PendingOAuth.config or a newer client read.
pub(crate) struct ClaimedGoogleConnect {
    record: TicketRecord,
    _raw_client: home_source::RawClientSnapshot,
}
impl ClaimedGoogleConnect {
    // Only an opaque reference; lexical SourceOwner I/O is the sole opener.
    pub(crate) fn captured_client(&self) -> &home_source::RawClientSnapshot {
        &self._raw_client
    }
}
/// Encrypted candidate plus original precrypto byte-CAS snapshot. No token
/// getter, authority constructor or durable schema is added by this RAM data.
pub(crate) struct PreparedGoogleCallback {
    payload: PreparedGooglePayload,
    snapshot: StoreSnapshot,
}
impl PreparedGoogleCallback {
    pub(super) fn payload(&self) -> &PreparedGooglePayload {
        &self.payload
    }
    pub(super) fn require_original(&self, rows: &Rows) -> Result<()> {
        rows.require_snapshot(&self.snapshot)
    }
    pub(super) fn into_payload(self) -> PreparedGooglePayload {
        self.payload
    }
}
pub(crate) fn encrypted_callback_candidate(
    token: McpOAuthToken,
    snapshot: StoreSnapshot,
) -> Result<PreparedGoogleCallback> {
    Ok(PreparedGoogleCallback {
        payload: PreparedGooglePayload::from_raw(
            serde_json::to_value(token).map_err(|_| Error::Invalid)?,
        )?,
        snapshot,
    })
}
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RefreshIdentity {
    credential: Credential,
    generation: String,
    binding: SourceBinding,
}
pub(crate) struct ChangeTicket {
    generation: String,
    mutation_id: String,
    claimed_ticket: Option<String>,
}
/// Already encrypted material prepared by the H1 secret-writing layer. It
/// cannot carry google_home or be constructed by E1/consumers/model params.
pub(crate) struct PreparedGooglePayload(Value);
impl PreparedGooglePayload {
    pub(super) fn from_raw(raw: Value) -> Result<Self> {
        let object = raw.as_object().ok_or(Error::Invalid)?;
        if object.keys().any(|k| !TOKEN_FIELDS.contains(&k.as_str())) {
            return Err(Error::Invalid);
        }
        let token = token_view(&raw)?;
        if token.provider_id != "google"
            || !token.access_token.starts_with(super::ENC_PREFIX)
            || token
                .refresh_token
                .as_ref()
                .is_some_and(|v| !v.starts_with(super::ENC_PREFIX))
        {
            return Err(Error::Invalid);
        }
        Ok(Self(
            serde_json::to_value(token).map_err(|_| Error::Invalid)?,
        ))
    }
}
const TOKEN_FIELDS: [&str; 5] = [
    "provider_id",
    "access_token",
    "refresh_token",
    "expires_at",
    "scopes",
];
fn token_view(raw: &Value) -> Result<McpOAuthToken> {
    let token: McpOAuthToken = serde_json::from_value(raw.clone()).map_err(|_| Error::Invalid)?;
    if token.provider_id.is_empty()
        || token.provider_id.len() > 128
        || token.access_token.len() > 1024 * 1024
        || token
            .refresh_token
            .as_ref()
            .is_some_and(|s| s.len() > 1024 * 1024)
    {
        return Err(Error::Invalid);
    }
    normalized_scopes(token.scopes.clone())?;
    Ok(token)
}
pub(crate) struct Rows {
    rows: Vec<Value>,
    dirty: bool,
    original_rows_digest: RawDigest,
    original: raw_array::RawArray,
}
impl Rows {
    fn read(home: &Path) -> Result<Self> {
        let original = raw_array::RawArray::read(&home.join(TOKEN_FILE))?;
        let rows = original.values().to_vec();
        let original_rows_digest = digest(&Value::Array(rows.clone()))?;
        let this = Self {
            rows,
            dirty: false,
            original_rows_digest,
            original,
        };
        if let Some(raw) = this.google() {
            let control = this.control()?;
            if let Some(c) = &control {
                if !c.valid() || (c.credential.is_some() != raw.get("access_token").is_some()) {
                    return Err(Error::Invalid);
                }
            }
            if raw.get("access_token").is_some() {
                token_view(raw)?;
            } else if control.is_none() {
                return Err(Error::Invalid);
            }
        }
        Ok(this)
    }
    fn same_original(&self, other: &Self) -> bool {
        self.original_rows_digest == other.original_rows_digest
            && self.original.same_bytes(&other.original)
    }
    fn google(&self) -> Option<&Value> {
        self.rows.iter().find(|r| r["provider_id"] == "google")
    }
    fn google_mut(&mut self) -> &mut Value {
        let index = match self.rows.iter().position(|r| r["provider_id"] == "google") {
            Some(i) => i,
            None => {
                self.rows.push(json!({"provider_id":"google"}));
                self.rows.len() - 1
            }
        };
        &mut self.rows[index]
    }
    fn control(&self) -> Result<Option<Control>> {
        self.google()
            .and_then(|r| r.get("google_home"))
            .map(|c| serde_json::from_value(c.clone()).map_err(|_| Error::Invalid))
            .transpose()
    }
    fn put(&mut self, c: &Control) -> Result<()> {
        if !c.valid() {
            return Err(Error::Invalid);
        }
        self.google_mut()["google_home"] = serde_json::to_value(c).map_err(|_| Error::Invalid)?;
        self.dirty = true;
        Ok(())
    }
    fn ready_control(&self) -> Result<Control> {
        let c = self.control()?.ok_or(Error::NotReady)?;
        c.ready()?;
        Ok(c)
    }
    pub(crate) fn metadata(&self) -> Result<Metadata> {
        let c = self.ready_control()?;
        Ok(Metadata {
            source_generation: c.source_generation,
            credential: c.credential,
        })
    }
    pub(crate) fn prepare(&mut self, _source: &ObservedSource) -> Result<Planned<Metadata>> {
        let before = self.google().cloned();
        if self.control()?.is_none() {
            let payload = self
                .google()
                .is_some_and(|r| r.get("access_token").is_some());
            self.put(&Control::new(payload))?;
        }
        let value = self.metadata()?;
        Planned::new(
            value,
            before.as_ref(),
            self.google(),
            Some(_source),
            PublicationOp::MetadataMigration
                .with_existing_tickets(before.as_ref(), self.google())?,
        )
    }
    pub(crate) fn require_oauth_scopes(
        &self,
        required: &[String],
        source: &ObservedSource,
        now: u64,
    ) -> Result<()> {
        let control = self.ready_control()?;
        crate::google_data_consent::require_current(
            control
                .credential
                .as_ref()
                .and_then(|credential| credential.google_data_consent.as_ref()),
        )
        .map_err(|_| Error::NotReady)?;
        let raw = self
            .google()
            .filter(|r| r.get("access_token").is_some())
            .ok_or(Error::NotReady)?;
        let token = token_view(raw)?;
        if token.access_token.is_empty()
            || (home_oauth_expired(token.expires_at, now)
                && token.refresh_token.as_ref().is_none_or(String::is_empty))
        {
            return Err(Error::NotReady);
        }
        // Existing unexpired access may be selected without a client file;
        // refresh eligibility requires the captured, statically valid client.
        if home_oauth_expired(token.expires_at, now) {
            source.require_client()?;
        }
        let actual = normalized_scopes(token.scopes)?;
        if required.iter().any(|s| !actual.contains(s)) {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    /// Observed-client compatibility primitive. The desired-client lifecycle
    /// uses begin_connect_to with its held payload and exact target binding.
    #[cfg(test)]
    pub(super) fn begin_connect(
        &mut self,
        source: &ObservedSource,
        state_hash: RawDigest,
        expires_at: u64,
        now: u64,
    ) -> Result<Planned<StartConnect>> {
        source.require_client()?;
        self.begin_connect_to(
            source,
            source.binding().clone(),
            None,
            state_hash,
            expires_at,
            now,
            crate::google_data_consent::Evidence::issue("test-admin")
                .map_err(|_| Error::Invalid)?,
        )
    }
    /// Desired client is an H1-computed target binding, never a caller stamp.
    /// Its payload stays only in RAM; the existing Pending mutation contains
    /// the closed file-kind and exact source-file before/after digests.
    pub(super) fn begin_connect_to(
        &mut self,
        source: &ObservedSource,
        desired: SourceBinding,
        write: Option<publication::PreparedSourceWrite>,
        state_hash: RawDigest,
        expires_at: u64,
        now: u64,
        google_data_consent: crate::google_data_consent::Evidence,
    ) -> Result<Planned<StartConnect>> {
        crate::google_data_consent::require_current(Some(&google_data_consent))
            .map_err(|_| Error::Invalid)?;
        let before = self.google().cloned();
        if desired.home_stamp() != source.binding().home_stamp()
            || desired.source_stamp() != source.binding().source_stamp()
            || desired.client_stamp().is_none()
            || (desired != *source.binding() && write.is_none())
        {
            return Err(Error::Invalid);
        }
        if expires_at <= now || expires_at > now.saturating_add(super::PENDING_TTL_SECS) {
            return Err(Error::Invalid);
        }
        let mut c = self.control()?.unwrap_or_else(|| {
            Control::new(
                self.google()
                    .is_some_and(|r| r.get("access_token").is_some()),
            )
        });
        // An unfinished config/callback mutation needs its own reconciliation;
        // a fresh OAuth start cannot silently clear that durable barrier.
        if c.state == State::Pending
            && c.pending_mutation
                .as_ref()
                .is_none_or(|m| m.kind != MutationKind::ConnectStart)
        {
            return Err(Error::NotReady);
        }
        // Latest start is permitted to supersede a previous pending start.
        c.source_generation = id();
        c.active_connect = None;
        c.state = State::Pending;
        let mutation_id = id();
        c.pending_mutation = Some(Mutation {
            mutation_id: mutation_id.clone(),
            kind: MutationKind::ConnectStart,
            expected_before: source.binding().clone(),
            expected_after: desired.clone(),
            payload: None,
            source_write: write.as_ref().map(|w| w.proof().clone()),
        });
        let ticket = TicketRecord {
            ticket_id: id(),
            state_hash,
            generation: c.source_generation.clone(),
            canonical_home: desired.home_stamp().clone(),
            client_stamp: desired.client_stamp().cloned().ok_or(Error::Invalid)?,
            source_stamp: desired.source_stamp().clone(),
            expires_at,
            phase: Phase::Pending,
            google_data_consent: Some(google_data_consent),
        };
        let result = StartConnect {
            generation: c.source_generation.clone(),
            mutation_id,
            ticket,
        };
        // Retain the complete pending ticket as nonsecret proof. Pending
        // remains unclaimable/unexposed; only publish_start can issue it.
        c.active_connect = Some(result.ticket.clone());
        let op = PublicationOp::ConnectStart {
            proof: ConnectProof::pending(result.ticket.clone(), result.mutation_id.clone()),
        };
        self.put(&c)?;
        Planned::new_source_transition(
            result,
            before.as_ref(),
            self.google(),
            source,
            desired,
            write,
            op,
        )
    }
    pub(super) fn publish_start(
        &mut self,
        start: &StartConnect,
        source: &ObservedSource,
        now: u64,
    ) -> Result<Planned<GoogleConnectTicket>> {
        let before = self.google().cloned();
        source.require_client()?;
        let mut c = self.control()?.ok_or(Error::Stale)?;
        let m = c.pending_mutation.as_ref().ok_or(Error::Stale)?;
        if c.state != State::Pending
            || c.source_generation != start.generation
            || m.mutation_id != start.mutation_id
            || m.kind != MutationKind::ConnectStart
            || c.active_connect.as_ref() != Some(&start.ticket)
            || m.expected_after != *source.binding()
            || !start.ticket.matches_source(source.binding())
            || start.ticket.expires_at <= now
        {
            return Err(Error::Stale);
        }
        c.state = State::Ready;
        c.pending_mutation = None;
        c.active_connect = Some(start.ticket.clone());
        self.put(&c)?;
        Planned::new(
            GoogleConnectTicket {
                record: start.ticket.clone(),
            },
            before.as_ref(),
            self.google(),
            Some(source),
            PublicationOp::ConnectPublishStart {
                proof: ConnectProof::pending(start.ticket.clone(), start.mutation_id.clone()),
            },
        )
    }
    pub(super) fn validate_pending_connect(
        &self,
        ticket: &GoogleConnectTicket,
        source: &ObservedSource,
        state_hash: &RawDigest,
        now: u64,
    ) -> Result<()> {
        let c = self.ready_control()?;
        let expected = &ticket.record;
        if c.active_connect.as_ref() == Some(expected)
            && expected.phase == Phase::Pending
            && expected.generation == c.source_generation
            && &expected.state_hash == state_hash
            && expected.expires_at > now
            && expected.matches_source(source.binding())
        {
            Ok(())
        } else {
            Err(Error::Stale)
        }
    }
    pub(super) fn claim(
        &mut self,
        ticket: &GoogleConnectTicket,
        source: &mut ObservedSource,
        now: u64,
    ) -> Result<Planned<ClaimedGoogleConnect>> {
        let before = self.google().cloned();
        let mut c = self.ready_control()?;
        let expected = &ticket.record;
        if c.active_connect.as_ref() != Some(expected)
            || expected.phase != Phase::Pending
            || expected.generation != c.source_generation
            || expected.expires_at <= now
            || !expected.matches_source(source.binding())
        {
            return Err(Error::Stale);
        }
        let raw_client = source.take_raw_client()?;
        let mut record = expected.clone();
        record.phase = Phase::Exchanging;
        c.active_connect = Some(record.clone());
        self.put(&c)?;
        let op = PublicationOp::TicketClaim {
            proof: ConnectProof::ticket(record.clone()),
        };
        Planned::new(
            ClaimedGoogleConnect {
                record,
                _raw_client: raw_client,
            },
            before.as_ref(),
            self.google(),
            Some(source),
            op,
        )
    }
    pub(super) fn validate_claim(
        &self,
        claim: &ClaimedGoogleConnect,
        source: &ObservedSource,
        now: u64,
    ) -> Result<()> {
        let c = self.ready_control()?;
        if c.active_connect.as_ref() == Some(&claim.record)
            && c.source_generation == claim.record.generation
            && claim.record.phase == Phase::Exchanging
            && claim.record.expires_at > now
            && claim.record.matches_source(source.binding())
        {
            Ok(())
        } else {
            Err(Error::Stale)
        }
    }
    pub(super) fn refresh_identity(&self, source: &ObservedSource) -> Result<RefreshIdentity> {
        source.require_client()?;
        self.oauth_identity(source)
    }
    fn oauth_identity(&self, source: &ObservedSource) -> Result<RefreshIdentity> {
        let c = self.ready_control()?;
        crate::google_data_consent::require_current(
            c.credential
                .as_ref()
                .and_then(|credential| credential.google_data_consent.as_ref()),
        )
        .map_err(|_| Error::NotReady)?;
        Ok(RefreshIdentity {
            credential: c.credential.ok_or(Error::Stale)?,
            generation: c.source_generation,
            binding: source.binding().clone(),
        })
    }
    pub(super) fn finish_refresh(
        &mut self,
        expected: &RefreshIdentity,
        source: &ObservedSource,
        payload: PreparedGooglePayload,
    ) -> Result<Planned<()>> {
        let before = self.google().cloned();
        if self.refresh_identity(source)? != *expected {
            return Err(Error::Stale);
        }
        let actual_scopes =
            normalized_scopes(token_view(self.google().ok_or(Error::Stale)?)?.scopes)?;
        let new_scopes = normalized_scopes(token_view(&payload.0)?.scopes)?;
        let mut c = self.ready_control()?;
        let credential = c.credential.as_mut().ok_or(Error::Stale)?;
        credential.token_revision = credential
            .token_revision
            .checked_add(1)
            .ok_or(Error::Invalid)?;
        if actual_scopes != new_scopes {
            credential.logical_epoch = credential
                .logical_epoch
                .checked_add(1)
                .ok_or(Error::Invalid)?;
            c.source_generation = id();
            c.active_connect = None;
        }
        self.set_payload(&payload.0)?;
        self.put(&c)?;
        Planned::new(
            (),
            before.as_ref(),
            self.google(),
            Some(source),
            PublicationOp::Refresh.with_existing_tickets(before.as_ref(), self.google())?,
        )
    }
    fn set_payload(&mut self, payload: &Value) -> Result<()> {
        token_view(payload)?;
        let row = self.google_mut();
        for name in TOKEN_FIELDS {
            row[name] = payload.get(name).cloned().unwrap_or(Value::Null);
        }
        self.dirty = true;
        Ok(())
    }
    pub(super) fn revoke(&mut self) -> Result<Planned<()>> {
        let before = self.google().cloned();
        let mut c = self.control()?.unwrap_or_else(|| Control::new(false));
        c.source_generation = id();
        c.state = State::Ready;
        c.credential = None;
        c.active_connect = None;
        c.pending_mutation = None;
        *self.google_mut() = json!({"provider_id":"google"});
        self.put(&c)?;
        Planned::new(
            (),
            before.as_ref(),
            self.google(),
            None,
            PublicationOp::Revoke.with_existing_tickets(before.as_ref(), self.google())?,
        )
    }
    /// Cross-file source/config writers fence FIRST, then release short locks
    /// before any grant/network wait. Generic changes invalidate all tickets.
    pub(super) fn fence_change(
        &mut self,
        source: &ObservedSource,
        after: SourceBinding,
    ) -> Result<Planned<ChangeTicket>> {
        self.fence_source_change(source, after, None)
    }
    pub(crate) fn fence_source_change(
        &mut self,
        source: &ObservedSource,
        after: SourceBinding,
        write: Option<publication::PreparedSourceWrite>,
    ) -> Result<Planned<ChangeTicket>> {
        let before = self.google().cloned();
        let mut c = self.control()?.unwrap_or_else(|| {
            Control::new(
                self.google()
                    .is_some_and(|r| r.get("access_token").is_some()),
            )
        });
        // Generic config changes cannot erase the unresolved callback
        // barrier, even after its ticket expires. Only its own finish or an
        // explicit revoke/approved reconciliation may resolve staged payload.
        if c.state == State::Pending
            && c.pending_mutation
                .as_ref()
                .is_some_and(|m| m.kind == MutationKind::ConnectFinish)
        {
            return Err(Error::NotReady);
        }
        c.source_generation = id();
        c.active_connect = None;
        c.state = State::Pending;
        if let Some(v) = c.credential.as_mut() {
            v.logical_epoch = v.logical_epoch.checked_add(1).ok_or(Error::Invalid)?;
        }
        let mutation_id = id();
        c.pending_mutation = Some(Mutation {
            mutation_id: mutation_id.clone(),
            kind: MutationKind::Config,
            expected_before: source.binding().clone(),
            expected_after: after.clone(),
            payload: None,
            source_write: write.as_ref().map(|w| w.proof().clone()),
        });
        let result = ChangeTicket {
            generation: c.source_generation.clone(),
            mutation_id,
            claimed_ticket: None,
        };
        self.put(&c)?;
        let op =
            PublicationOp::ConfigFence.with_existing_tickets(before.as_ref(), self.google())?;
        if write.is_some() {
            Planned::new_source_transition(
                result,
                before.as_ref(),
                self.google(),
                source,
                after,
                write,
                op,
            )
        } else {
            Planned::new(result, before.as_ref(), self.google(), Some(source), op)
        }
    }
    /// Callback-only exception preserves exactly its exchanged ticket. A
    /// durable pending row precedes both encrypted payload and master writes.
    pub(super) fn begin_claimed_finish(
        &mut self,
        claim: &ClaimedGoogleConnect,
        source: &ObservedSource,
        payload: &PreparedGooglePayload,
        write: Option<&publication::PreparedSourceWrite>,
        now: u64,
    ) -> Result<Planned<ChangeTicket>> {
        let before = self.google().cloned();
        self.validate_claim(claim, source, now)?;
        if write.is_some_and(|w| w.proof().kind != SourceFileKind::Config)
            || (source.binding() != source.oauth_binding() && write.is_none())
        {
            return Err(Error::Invalid);
        }
        let mut c = self.ready_control()?;
        let epoch = c.credential.as_ref().map_or(Ok(1), |v| {
            v.logical_epoch.checked_add(1).ok_or(Error::Invalid)
        })?;
        let mutation_id = id();
        c.state = State::Pending;
        c.pending_mutation = Some(Mutation {
            mutation_id: mutation_id.clone(),
            kind: MutationKind::ConnectFinish,
            expected_before: source.binding().clone(),
            expected_after: source.oauth_binding().clone(),
            payload: Some(PayloadExpectation {
                digest: digest(&payload.0)?,
                credential_id: id(),
                logical_epoch: epoch,
            }),
            source_write: write.map(|w| w.proof().clone()),
        });
        let result = ChangeTicket {
            generation: c.source_generation.clone(),
            mutation_id,
            claimed_ticket: Some(claim.record.ticket_id.clone()),
        };
        self.put(&c)?;
        let op = PublicationOp::BeginClaimedFinish {
            proof: ConnectProof::pending(claim.record.clone(), result.mutation_id.clone()),
        };
        Planned::new(result, before.as_ref(), self.google(), Some(source), op)
    }
    fn pending_for(&self, t: &ChangeTicket) -> Result<Control> {
        let c = self.control()?.ok_or(Error::Stale)?;
        if c.state != State::Pending
            || c.source_generation != t.generation
            || c.pending_mutation
                .as_ref()
                .is_none_or(|m| m.mutation_id != t.mutation_id)
        {
            return Err(Error::Stale);
        }
        Ok(c)
    }
    pub(super) fn publish_claimed_payload(
        &mut self,
        t: &ChangeTicket,
        claim: &ClaimedGoogleConnect,
        source: &ObservedSource,
        payload: PreparedGooglePayload,
        write: Option<publication::PreparedSourceWrite>,
        now: u64,
    ) -> Result<Planned<()>> {
        let before = self.google().cloned();
        let mut c = self.pending_for(t)?;
        let m = c.pending_mutation.as_ref().ok_or(Error::Stale)?;
        if m.kind != MutationKind::ConnectFinish
            || m.expected_before != *source.binding()
            || c.active_connect.as_ref() != Some(&claim.record)
            || claim.record.expires_at <= now
            || t.claimed_ticket.as_deref() != Some(claim.record.ticket_id.as_str())
        {
            return Err(Error::Stale);
        }
        if m.source_write.as_ref() != write.as_ref().map(|w| w.proof()) {
            return Err(Error::Stale);
        }
        let target = m.expected_after.clone();
        let expected = m.payload.as_ref().ok_or(Error::Stale)?;
        if digest(&payload.0)? != expected.digest {
            return Err(Error::Stale);
        }
        c.credential = Some(Credential {
            credential_id: expected.credential_id.clone(),
            logical_epoch: expected.logical_epoch,
            token_revision: 1,
            google_data_consent: claim.record.google_data_consent.clone(),
        });
        self.set_payload(&payload.0)?;
        self.put(&c)?;
        let op = PublicationOp::ClaimedPayload {
            proof: ConnectProof::pending(claim.record.clone(), t.mutation_id.clone()),
        };
        if write.is_some() {
            Planned::new_source_transition(
                (),
                before.as_ref(),
                self.google(),
                source,
                target,
                write,
                op,
            )
        } else {
            Planned::new((), before.as_ref(), self.google(), Some(source), op)
        }
    }
    pub(crate) fn finish_change(
        &mut self,
        t: &ChangeTicket,
        source: &ObservedSource,
        now: u64,
    ) -> Result<Planned<()>> {
        let before = self.google().cloned();
        let mut c = self.pending_for(t)?;
        let m = c.pending_mutation.as_ref().ok_or(Error::Stale)?;
        if m.expected_after != *source.binding() {
            return Err(Error::Stale);
        }
        if m.kind == MutationKind::ConnectFinish {
            let ticket = c.active_connect.as_ref().ok_or(Error::Stale)?;
            let expected = m.payload.as_ref().ok_or(Error::Stale)?;
            let raw = self.google().ok_or(Error::Stale)?;
            let mut payload = serde_json::Map::new();
            for name in TOKEN_FIELDS {
                payload.insert(name.into(), raw.get(name).cloned().unwrap_or(Value::Null));
            }
            if t.claimed_ticket.as_deref() != Some(ticket.ticket_id.as_str())
                || ticket.phase != Phase::Exchanging
                || ticket.expires_at <= now
                || c.credential
                    .as_ref()
                    .is_none_or(|v| v.credential_id != expected.credential_id)
                || digest(&Value::Object(payload))? != expected.digest
            {
                return Err(Error::Stale);
            }
        } else if t.claimed_ticket.is_some() {
            return Err(Error::Stale);
        }
        let op = if m.kind == MutationKind::ConnectFinish {
            PublicationOp::ConnectFinish {
                proof: ConnectProof::pending(
                    c.active_connect.clone().ok_or(Error::Stale)?,
                    m.mutation_id.clone(),
                ),
            }
        } else {
            PublicationOp::ConfigFinish
        };
        if m.kind == MutationKind::ConnectFinish {
            // Replacement publishes a new source generation only after the
            // original ticket completed every pending payload/config CAS.
            c.source_generation = id();
        }
        c.state = State::Ready;
        c.pending_mutation = None;
        c.active_connect = None;
        self.put(&c)?;
        Planned::new((), before.as_ref(), self.google(), Some(source), op)
    }
}
/// One atomic token-store transaction. Source/config locks are owned by the
/// caller, and no closure may acquire a config/grant lock or await a provider.
pub(crate) fn transaction<T>(
    guard: &SourceGuard,
    f: impl FnOnce(&mut Rows) -> Result<T>,
) -> Result<T> {
    duduclaw_core::with_file_lock(&guard.home().join(TOKEN_FILE), || {
        Ok((|| {
            publication::ensure_unblocked(guard.home())?;
            let mut rows = Rows::read(guard.home())?;
            let google_before = rows.google().cloned();
            let result = f(&mut rows)?;
            if rows.google() != google_before.as_ref() {
                return Err(Error::Invalid);
            }
            if rows.dirty {
                // Preserve the original semantic CAS AND compare actual owned
                // file bytes; semantic Value hashes alone do not bind ranges.
                if !Rows::read(guard.home())?.same_original(&rows) {
                    return Err(Error::Stale);
                }
                let prepared = rows.prepare_write()?;
                atomic_write(guard.home(), &prepared).map_err(|e| match e {
                    DataWriteFailure::BeforeRename => Error::NotPublished,
                    DataWriteFailure::AfterRename => Error::PublishedDurabilityUnconfirmed,
                })?;
            }
            Ok(result)
        })())
    })
    .map_err(|_| Error::Io)?
}
#[derive(Clone, Copy)]
enum DataWriteFailure {
    BeforeRename,
    AfterRename,
}
/// Owned actual raw emitter output. It carries no authority and is not a
/// second store: prepared before the gate, consumed unchanged by data write.
struct PreparedTokenWrite(zeroize::Zeroizing<Vec<u8>>);
impl Rows {
    fn prepare_write(&self) -> Result<PreparedTokenWrite> {
        Ok(PreparedTokenWrite(self.original.render(&self.rows)?))
    }
}
fn atomic_write(
    home: &Path,
    prepared: &PreparedTokenWrite,
) -> std::result::Result<(), DataWriteFailure> {
    use publication::{Fault, hit};
    fn before<E>(_: E) -> DataWriteFailure {
        DataWriteFailure::BeforeRename
    }
    struct Temp(Option<std::path::PathBuf>);
    impl Drop for Temp {
        fn drop(&mut self) {
            if let Some(p) = self.0.take() {
                let _ = std::fs::remove_file(p);
            }
        }
    }
    let path = home.join(format!(".mcp-oauth-{}.tmp", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    hit(home, Fault::DataOpen).map_err(before)?;
    let mut file = options.open(&path).map_err(before)?;
    let mut temp = Temp(Some(path.clone()));
    hit(home, Fault::DataWrite).map_err(before)?;
    file.write_all(&prepared.0).map_err(before)?;
    hit(home, Fault::DataFileSync).map_err(before)?;
    file.sync_all().map_err(before)?;
    hit(home, Fault::DataRename).map_err(before)?;
    std::fs::rename(&path, home.join(TOKEN_FILE)).map_err(before)?;
    temp.0 = None;
    hit(home, Fault::DataDirSync).map_err(|_| DataWriteFailure::AfterRename)?;
    publication::sync_dir(home).map_err(|_| DataWriteFailure::AfterRename)
}
pub(crate) fn revoke_google(home: &Path) -> Result<()> {
    home_source::with_source_guard(home, |g| publication_transaction(g, Rows::revoke))
}
/// Ordinary raw views never migrate or decrypt. Google visibility is token
/// authority; it is deliberately separate from saved-client visibility.
fn google_public_visible(row: &Value) -> bool {
    let Some(payload) = row.get("access_token") else {
        return false;
    };
    if !payload.is_string() {
        return false;
    }
    match row.get("google_home") {
        None => false,
        Some(value) => serde_json::from_value::<Control>(value.clone()).is_ok_and(|c| {
            c.valid()
                && c.state == State::Ready
                && c.credential.as_ref().is_some_and(|credential| {
                    crate::google_data_consent::require_current(
                        credential.google_data_consent.as_ref(),
                    )
                    .is_ok()
                })
        }),
    }
}
struct PublicTokenRead {
    original: raw_array::RawArray,
    tokens: Vec<McpOAuthToken>,
}
fn read_public_snapshot(home: &Path, provider: Option<&str>) -> Result<PublicTokenRead> {
    if provider.is_some_and(|p| !raw_array::valid_provider(p)) {
        return Err(Error::Invalid);
    }
    home_source::with_source_guard(home, |g| {
        duduclaw_core::with_file_lock(&g.home().join(TOKEN_FILE), || {
            Ok((|| {
                let original = raw_array::RawArray::read(&g.home().join(TOKEN_FILE))?;
                let blocked = publication::blocked(g.home());
                let mut tokens = Vec::new();
                for row in original.values() {
                    if provider.is_some_and(|p| row["provider_id"] != p) {
                        continue;
                    }
                    if row["provider_id"] == "google" && (blocked || !google_public_visible(row)) {
                        continue;
                    }
                    tokens.push(token_view(row)?);
                }
                Ok(PublicTokenRead { original, tokens })
            })())
        })
        .map_err(|_| Error::Io)?
    })
}
pub(super) fn load_public(home: &Path) -> Result<Vec<McpOAuthToken>> {
    Ok(read_public_snapshot(home, None)?.tokens)
}
pub(super) fn load_decoded_public(
    home: &Path,
    provider: Option<&str>,
) -> Result<Vec<McpOAuthToken>> {
    let captured = read_public_snapshot(home, provider)?;
    // Crypto is over captured bytes only and holds no source/token lock.
    let decoded = captured
        .tokens
        .iter()
        .filter_map(|token| super::decode_public_token(home, token))
        .collect();
    #[cfg(test)]
    test_probe::preparation_boundary(home);
    home_source::with_source_guard(home, |g| {
        duduclaw_core::with_file_lock(&g.home().join(TOKEN_FILE), || {
            Ok((|| {
                let actual = raw_array::RawArray::read(&g.home().join(TOKEN_FILE))?;
                if !actual.same_bytes(&captured.original)
                    || (captured.tokens.iter().any(|t| t.provider_id == "google")
                        && publication::blocked(g.home()))
                {
                    return Err(Error::Stale);
                }
                Ok(decoded)
            })())
        })
        .map_err(|_| Error::Io)?
    })
}
fn encoded_token(home: &Path, t: &McpOAuthToken) -> Result<Value> {
    let value = McpOAuthToken {
        provider_id: t.provider_id.clone(),
        access_token: super::encrypt_field(&t.access_token, home),
        refresh_token: t
            .refresh_token
            .as_ref()
            .map(|s| super::encrypt_field(s, home)),
        expires_at: t.expires_at,
        scopes: t.scopes.clone(),
    };
    serde_json::to_value(value).map_err(|_| Error::Invalid)
}
fn token_candidate(t: &McpOAuthToken, encrypted_bound: bool) -> Result<Value> {
    let mut value = serde_json::to_value(t).map_err(|_| Error::Invalid)?;
    if encrypted_bound {
        value["access_token"] = json!(super::encrypted_size_placeholder(&t.access_token)?);
        if let Some(refresh) = &t.refresh_token {
            value["refresh_token"] = json!(super::encrypted_size_placeholder(refresh)?);
        }
    }
    token_view(&value)?;
    Ok(value)
}
fn merged_token_rows(old: &[Value], incoming: &[Value], replace: bool) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    for row in old {
        let next = incoming
            .iter()
            .find(|v| v["provider_id"] == row["provider_id"]);
        if row["provider_id"] == "google" || !replace || next.is_some() {
            let mut value = row.clone();
            if let Some(next) = next {
                for name in TOKEN_FIELDS {
                    value[name] = next[name].clone();
                }
            }
            rows.push(value);
        }
    }
    for row in incoming {
        if !old
            .iter()
            .any(|old| old["provider_id"] == row["provider_id"])
        {
            rows.push(row.clone());
        }
    }
    Ok(rows)
}
fn mutate_public_tokens(home: &Path, tokens: &[McpOAuthToken], replace: bool) -> Result<()> {
    if tokens.iter().any(|t| t.provider_id == "google") {
        return Err(Error::PendingWiring);
    }
    if tokens.len() > raw_array::MAX_ROWS {
        return Err(Error::Invalid);
    }
    let mut ids = std::collections::HashSet::new();
    for t in tokens {
        if !raw_array::valid_provider(&t.provider_id) || !ids.insert(&t.provider_id) {
            return Err(Error::Invalid);
        }
        token_view(&serde_json::to_value(t).map_err(|_| Error::Invalid)?)?;
    }
    let captured = home_source::with_source_guard(home, |g| {
        transaction(g, |r| {
            // Check combined raw document limits BEFORE any target crypto, for
            // both the known AES/base64 output and legacy plaintext fallback.
            for encrypted in [false, true] {
                let incoming = tokens
                    .iter()
                    .map(|t| token_candidate(t, encrypted))
                    .collect::<Result<Vec<_>>>()?;
                r.original
                    .render(&merged_token_rows(&r.rows, &incoming, replace)?)?;
            }
            Ok(StoreSnapshot::of(r))
        })
    })?;
    let encoded = tokens
        .iter()
        .map(|t| encoded_token(home, t))
        .collect::<Result<Vec<_>>>()?;
    #[cfg(test)]
    test_probe::preparation_boundary(home);
    home_source::with_source_guard(home, |g| {
        transaction(g, |r| {
            r.require_snapshot(&captured)?;
            let next = merged_token_rows(&r.rows, &encoded, replace)?;
            r.dirty = next != r.rows;
            r.rows = next;
            Ok(())
        })
    })
}
/// Whole public save reconciles only non-Google rows. Omitting Google is not
/// permission to erase its opaque control/cipher; a Google DTO has no ticket.
pub(super) fn save_public(home: &Path, tokens: &[McpOAuthToken]) -> Result<()> {
    mutate_public_tokens(home, tokens, true)
}
pub(super) fn upsert_public(home: &Path, token: McpOAuthToken) -> Result<()> {
    mutate_public_tokens(home, &[token], false)
}
pub(super) fn remove_public(home: &Path, provider: &str) -> Result<()> {
    if !raw_array::valid_provider(provider) {
        return Err(Error::Invalid);
    }
    if provider == "google" {
        return revoke_google(home);
    }
    home_source::with_source_guard(home, |g| {
        transaction(g, |r| {
            let before = r.rows.len();
            r.rows.retain(|v| v["provider_id"] != provider);
            r.dirty = r.rows.len() != before;
            Ok(())
        })
    })
}
/// Opaque exact file+semantic snapshot used across unlocked target crypto.
/// This grants no write authority: the normal/typed transaction still checks
/// its original Google equality or sealed publication producer independently.
pub(crate) struct StoreSnapshot {
    semantic: RawDigest,
    bytes: Option<RawDigest>,
}
impl StoreSnapshot {
    fn of(rows: &Rows) -> Self {
        Self {
            semantic: rows.original_rows_digest.clone(),
            bytes: rows.original.byte_digest(),
        }
    }
}
impl Rows {
    pub(super) fn require_snapshot(&self, expected: &StoreSnapshot) -> Result<()> {
        if self.original_rows_digest == expected.semantic
            && self.original.byte_digest() == expected.bytes
        {
            Ok(())
        } else {
            Err(Error::Stale)
        }
    }
}
/// Nonsecret RAM observation, never a bearer or a selection authority.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct OAuthMaterialMetadata {
    pub(crate) credential: Credential,
    generation: String,
    binding: SourceBinding,
    payload: RawDigest,
    pub(crate) expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub(crate) scopes: Vec<String>,
}
/// The same Home clock and sixty-second grace are used by selection,
/// resolution and effect-time validation. Legacy None expiry remains usable.
pub(crate) fn home_oauth_expired(
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    now: u64,
) -> bool {
    expires_at.is_some_and(|expiry| {
        i64::try_from(now)
            .ok()
            .and_then(|n| chrono::DateTime::<chrono::Utc>::from_timestamp(n, 0))
            .and_then(|clock| clock.checked_add_signed(chrono::Duration::seconds(60)))
            .is_none_or(|grace| grace >= expiry)
    })
}
fn payload_digest(raw: &Value) -> Result<RawDigest> {
    let mut projection = serde_json::Map::new();
    for field in TOKEN_FIELDS {
        projection.insert(field.into(), raw.get(field).cloned().unwrap_or(Value::Null));
    }
    digest(&Value::Object(projection))
}
impl Rows {
    fn oauth_material_metadata(&self, source: &ObservedSource) -> Result<OAuthMaterialMetadata> {
        let identity = self.oauth_identity(source)?;
        let raw = self.google().ok_or(Error::NotReady)?;
        // Parse only nonsecret fields here; validators never copy/open access
        // or refresh strings into a token DTO.
        let expires_at =
            serde_json::from_value(raw.get("expires_at").cloned().unwrap_or(Value::Null))
                .map_err(|_| Error::Invalid)?;
        let scopes: Vec<String> =
            serde_json::from_value(raw["scopes"].clone()).map_err(|_| Error::Invalid)?;
        Ok(OAuthMaterialMetadata {
            credential: identity.credential,
            generation: identity.generation,
            binding: identity.binding,
            payload: payload_digest(raw)?,
            expires_at,
            scopes: normalized_scopes(scopes)?,
        })
    }
}
pub(crate) fn observe_home_oauth(
    g: &SourceGuard,
    source: &ObservedSource,
) -> Result<OAuthMaterialMetadata> {
    transaction(g, |r| r.oauth_material_metadata(source))
}
pub(crate) fn capture_home_oauth(
    g: &SourceGuard,
    source: &ObservedSource,
) -> Result<home_source::HomeOAuthCapture> {
    transaction(g, |r| {
        let row = r.oauth_material_metadata(source)?;
        let identity = r.oauth_identity(source)?;
        let original = StoreSnapshot::of(r);
        let token = token_view(r.google().ok_or(Error::NotReady)?)?;
        Ok(home_source::capture_from_store(
            token, row, identity, original,
        ))
    })
}
pub(crate) fn require_home_oauth_capture(
    g: &SourceGuard,
    source: &ObservedSource,
    original: &StoreSnapshot,
    identity: &RefreshIdentity,
) -> Result<()> {
    transaction(g, |r| {
        r.require_snapshot(original)?;
        if r.oauth_identity(source)? == *identity {
            Ok(())
        } else {
            Err(Error::Stale)
        }
    })
}
pub(crate) fn prepare_refresh_payload(token: McpOAuthToken) -> Result<PreparedGooglePayload> {
    PreparedGooglePayload::from_raw(serde_json::to_value(token).map_err(|_| Error::Invalid)?)
}
/// Only the exact captured input and encrypted provider result determine the
/// expected post-row. Scope rotation is publishable but grants no old proof.
pub(crate) fn expected_refresh_metadata(
    old: &OAuthMaterialMetadata,
    payload: &PreparedGooglePayload,
) -> Result<Option<OAuthMaterialMetadata>> {
    let token = token_view(&payload.0)?;
    let scopes = normalized_scopes(token.scopes)?;
    if scopes != old.scopes {
        return Ok(None);
    }
    let mut expected = old.clone();
    expected.credential.token_revision = expected
        .credential
        .token_revision
        .checked_add(1)
        .ok_or(Error::Invalid)?;
    expected.payload = payload_digest(&payload.0)?;
    expected.expires_at = token.expires_at;
    Ok(Some(expected))
}
pub(crate) fn publish_home_refresh(
    g: &SourceGuard,
    source: &ObservedSource,
    original: &StoreSnapshot,
    identity: &RefreshIdentity,
    payload: PreparedGooglePayload,
) -> Result<()> {
    publication_transaction(g, |r| {
        r.require_snapshot(original)?;
        r.finish_refresh(identity, source, payload)
    })
}
pub(crate) fn preflight_refresh_payload(
    g: &SourceGuard,
    source: &ObservedSource,
    original: &StoreSnapshot,
    identity: &RefreshIdentity,
    access_len: usize,
    refresh_len: Option<usize>,
    retained_refresh_cipher_len: Option<usize>,
    expires_at: chrono::DateTime<chrono::Utc>,
    scopes: &[String],
) -> Result<()> {
    fn bound(len: usize) -> Result<String> {
        if len == 0 || len > 1024 * 1024 {
            return Err(Error::Invalid);
        }
        let placeholder = super::encrypted_size_placeholder(&"A".repeat(len))?;
        if placeholder.len() > 1024 * 1024 {
            return Err(Error::Invalid);
        }
        Ok(placeholder)
    }
    let refresh = match (refresh_len, retained_refresh_cipher_len) {
        (Some(len), None) => Some(bound(len)?),
        (None, Some(len)) if len > 0 && len <= 1024 * 1024 => Some("A".repeat(len)),
        (None, None) => None,
        _ => return Err(Error::Invalid),
    };
    let scopes = normalized_scopes(scopes.to_vec())?;
    let candidate = json!({"provider_id":"google","access_token":bound(access_len)?,
        "refresh_token":refresh,"expires_at":expires_at,"scopes":scopes});
    transaction(g, |r| {
        r.require_snapshot(original)?;
        if r.refresh_identity(source)? != *identity {
            return Err(Error::Stale);
        }
        let actual = r.oauth_material_metadata(source)?;
        let mut control = r.ready_control()?;
        let credential = control.credential.as_mut().ok_or(Error::Stale)?;
        credential.token_revision = credential
            .token_revision
            .checked_add(1)
            .ok_or(Error::Invalid)?;
        if actual.scopes != scopes {
            credential.logical_epoch = credential
                .logical_epoch
                .checked_add(1)
                .ok_or(Error::Invalid)?;
            // The replacement UUID has exactly the same encoded length.
            // Retaining this captured UUID is a capacity projection only.
            control.active_connect = None;
        }
        let mut rows = r.rows.clone();
        let google = rows
            .iter_mut()
            .find(|v| v["provider_id"] == "google")
            .ok_or(Error::Stale)?;
        for field in TOKEN_FIELDS {
            google[field] = candidate[field].clone();
        }
        google["google_home"] = serde_json::to_value(control).map_err(|_| Error::Invalid)?;
        // Actual opaque foreign emitter, actual unknown Google fields and
        // actual counter widths; no callback reserve, UUID or fake producer.
        let _bounded = r.original.render(&rows)?;
        Ok(())
    })
}
/// Ordinary error classification without capturing token material/revision.
pub(crate) fn ordinary_home_oauth_presence(
    g: &SourceGuard,
    _source: &ObservedSource,
) -> Result<(bool, bool, bool)> {
    transaction(g, |r| {
        if let Some(control) = r.control()? {
            control.ready()?;
        }
        let Some(raw) = r.google().filter(|v| v.get("access_token").is_some()) else {
            return Ok((false, false, false));
        };
        let expiry = serde_json::from_value(raw.get("expires_at").cloned().unwrap_or(Value::Null))
            .map_err(|_| Error::Invalid)?;
        Ok((
            true,
            home_oauth_expired(expiry, host_time_now(g)),
            raw["refresh_token"].as_str().is_some_and(|v| !v.is_empty()),
        ))
    })
}
#[cfg(test)]
pub(crate) mod oauth_testing {
    use super::*;
    pub(crate) enum Fault {
        GateWrite,
        DataRename,
        CleanupDirSync,
    }
    pub(crate) struct FaultGuard {
        _guard: publication::FaultGuard,
    }
    impl FaultGuard {
        pub(crate) fn at(home: &Path, fault: Fault) -> Self {
            let fault = match fault {
                Fault::GateWrite => publication::Fault::GateWrite,
                Fault::DataRename => publication::Fault::DataRename,
                Fault::CleanupDirSync => publication::Fault::CleanupDirSync,
            };
            Self {
                _guard: publication::FaultGuard::at(home, fault),
            }
        }
    }
    pub(crate) struct ClockGuard {
        _guard: publication::ClockGuard,
    }
    impl ClockGuard {
        pub(crate) fn fixed(now: u64) -> Self {
            Self {
                _guard: publication::ClockGuard::fixed_if_unset(now),
            }
        }
    }
}
/// Conservative reservation for the entire schema-1 Google control FIELD in
/// Pending Config and Ready: fixed UUIDs, scalar counters, at most two source
/// bindings and one Client digest proof. It is deliberately generous; none of
/// these control fields contains unbounded client bytes or token payloads.
/// This can reject a near-full store that might fit a smaller actual control.
const CLIENT_CONTROL_RESERVE_BYTES: usize = 64 * 1024;
// Independent schema-1 ConnectStart/Ready bound: at most twelve fixed
// 32-byte digest arrays (two bindings, file proof, complete ticket), fixed
// UUIDs, u64 counters, closed enums/labels. No secret/client/scope bytes.
// 64 KiB exceeds that fixed control shape; actual emission is still checked
// before the gate by prepare_write. Projection creates no ticket/authority.
const CONNECT_START_CONTROL_RESERVE_BYTES: usize = 64 * 1024;
// Independent ConnectFinish/Ready fixed-shape reservation, including the
// thirteenth digest for payload expectation and held Config proof.
const CALLBACK_CONTROL_RESERVE_BYTES: usize = 64 * 1024;
impl Rows {
    fn ensure_control_capacity(&self, reserve: usize) -> Result<()> {
        if self.google().is_none() && self.rows.len() >= raw_array::MAX_ROWS {
            return Err(Error::Invalid);
        }
        let foreign: Vec<_> = self
            .rows
            .iter()
            .filter(|r| r["provider_id"] != "google")
            .cloned()
            .collect();
        // Retained foreign rows use the SAME actual opaque emitter. Internal
        // JSON whitespace must not disappear into a semantic Value estimate.
        let retained = self.original.render(&foreign)?;
        let payload_size = if let Some(google) = self.google() {
            let mut payload = google.clone();
            payload
                .as_object_mut()
                .ok_or(Error::Invalid)?
                .remove("google_home");
            zeroize::Zeroizing::new(
                serde_json::to_vec_pretty(&payload).map_err(|_| Error::Invalid)?,
            )
            .len()
        } else {
            // Size only of a new provider-only row, not a fabricated control,
            // receipt, ChangeTicket, id or publication producer.
            br#"{"provider_id":"google"}"#.len()
        };
        let projected = retained
            .len()
            .checked_add(payload_size)
            .and_then(|n| n.checked_add(reserve))
            .and_then(|n| n.checked_add(32))
            .ok_or(Error::Invalid)?;
        if projected > raw_array::MAX_BYTES {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}
pub(crate) fn callback_control_snapshot(
    g: &SourceGuard,
    source: &ObservedSource,
    claim: &ClaimedGoogleConnect,
) -> Result<StoreSnapshot> {
    transaction(g, |r| {
        r.validate_claim(claim, source, host_time_now(g))?;
        r.ensure_control_capacity(CALLBACK_CONTROL_RESERVE_BYTES)?;
        Ok(StoreSnapshot::of(r))
    })
}
pub(crate) fn validate_callback_snapshot(
    g: &SourceGuard,
    source: &ObservedSource,
    claim: &ClaimedGoogleConnect,
    original: &StoreSnapshot,
) -> Result<()> {
    transaction(g, |r| {
        r.require_snapshot(original)?;
        r.validate_claim(claim, source, host_time_now(g))
    })
}
pub(crate) fn preflight_callback_payload(
    g: &SourceGuard,
    source: &ObservedSource,
    claim: &ClaimedGoogleConnect,
    original: &StoreSnapshot,
    access_len: usize,
    refresh_len: Option<usize>,
    expires_at: chrono::DateTime<chrono::Utc>,
    scopes: &[String],
) -> Result<()> {
    fn bound(len: usize) -> Result<String> {
        if len == 0 || len > 1024 * 1024 {
            return Err(Error::Invalid);
        }
        let encoded = super::encrypted_size_placeholder(&"A".repeat(len))?;
        if encoded.len() > 1024 * 1024 {
            return Err(Error::Invalid);
        }
        Ok(encoded)
    }
    let candidate = json!({"provider_id":"google","access_token":bound(access_len)?,
        "refresh_token":refresh_len.map(bound).transpose()?,"expires_at":expires_at,"scopes":normalized_scopes(scopes.to_vec())?});
    transaction(g, |r| {
        r.require_snapshot(original)?;
        r.validate_claim(claim, source, host_time_now(g))?;
        // Pending ConnectFinish adds at most13 fixed digests (the12 start
        // shape plus payload expectation), UUIDs/counters and closed labels.
        // Bound BOTH old-payload Pending and new-payload Pending/Ready. The
        // actual unchanged foreign-row emitter keeps internal whitespace.
        r.ensure_control_capacity(CALLBACK_CONTROL_RESERVE_BYTES)?;
        let mut google = r.google().cloned().ok_or(Error::Stale)?;
        for field in TOKEN_FIELDS {
            google[field] = candidate[field].clone();
        }
        google
            .as_object_mut()
            .ok_or(Error::Invalid)?
            .remove("google_home");
        let foreign: Vec<_> = r
            .rows
            .iter()
            .filter(|row| row["provider_id"] != "google")
            .cloned()
            .collect();
        let retained = r.original.render(&foreign)?;
        let payload = zeroize::Zeroizing::new(
            serde_json::to_vec_pretty(&google).map_err(|_| Error::Invalid)?,
        );
        if retained
            .len()
            .checked_add(payload.len())
            .and_then(|n| n.checked_add(CALLBACK_CONTROL_RESERVE_BYTES + 32))
            .is_none_or(|n| n > raw_array::MAX_BYTES)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    })
}
pub(super) fn connect_start_snapshot(g: &SourceGuard) -> Result<StoreSnapshot> {
    transaction(g, |r| {
        if r.control()?.is_some_and(|c| {
            c.state == State::Pending
                && c.pending_mutation
                    .as_ref()
                    .is_none_or(|m| m.kind != MutationKind::ConnectStart)
        }) {
            return Err(Error::NotReady);
        }
        r.ensure_control_capacity(CONNECT_START_CONTROL_RESERVE_BYTES)?;
        Ok(StoreSnapshot::of(r))
    })
}
pub(super) fn client_mutation_snapshot(g: &SourceGuard) -> Result<StoreSnapshot> {
    transaction(g, |r| {
        if r.control()?.is_some_and(|c| {
            c.state == State::Pending
                && c.pending_mutation
                    .as_ref()
                    .is_some_and(|m| m.kind == MutationKind::ConnectFinish)
        }) {
            return Err(Error::NotReady);
        }
        r.ensure_control_capacity(CLIENT_CONTROL_RESERVE_BYTES)?;
        Ok(StoreSnapshot::of(r))
    })
}
/// A saved client is usable by Personal even without a Home token, credential
/// or enabled master. Only publication/unresolved-source state gates it.
pub(super) fn google_client_readable(g: &SourceGuard) -> Result<()> {
    publication::ensure_unblocked(g.home())?;
    transaction(g, |r| {
        if r.control()?.is_some_and(|c| c.state != State::Ready) {
            return Err(Error::NotReady);
        }
        Ok(())
    })
}
#[cfg(test)]
pub(crate) mod test_probe {
    use std::{
        collections::HashMap,
        path::{Path, PathBuf},
        sync::{
            Arc, Mutex, OnceLock,
            atomic::{AtomicUsize, Ordering},
        },
    };
    pub(crate) struct StartGateFaultGuard {
        _inner: super::publication::FaultGuard,
    }
    pub(crate) fn fail_start_gate(home: &Path) -> StartGateFaultGuard {
        StartGateFaultGuard {
            _inner: super::publication::FaultGuard::at(home, super::publication::Fault::GateWrite),
        }
    }
    struct Counters {
        client: String,
        crypto: AtomicUsize,
        provider: AtomicUsize,
        source_fences: AtomicUsize,
        source_payloads: AtomicUsize,
        google_crypto: AtomicUsize,
    }
    fn probes() -> &'static Mutex<HashMap<PathBuf, Arc<Counters>>> {
        static P: OnceLock<Mutex<HashMap<PathBuf, Arc<Counters>>>> = OnceLock::new();
        P.get_or_init(Default::default)
    }
    thread_local! {static BOUNDARY:std::cell::RefCell<Option<(PathBuf,String,Box<dyn FnOnce()>)>>=const {std::cell::RefCell::new(None)};}
    /// Observer runs only at the real unlocked preparation/crypto boundary;
    /// it never returns a fabricated crypto/CAS/persistence result.
    pub(super) struct BoundaryGuard {
        id: String,
    }
    impl BoundaryGuard {
        pub(super) fn at(home: &Path, hook: impl FnOnce() + 'static) -> Self {
            let home = home.canonicalize().unwrap();
            let id = uuid::Uuid::new_v4().to_string();
            BOUNDARY.with(|s| {
                assert!(s.borrow().is_none());
                *s.borrow_mut() = Some((home, id.clone(), Box::new(hook)));
            });
            Self { id }
        }
    }
    impl Drop for BoundaryGuard {
        fn drop(&mut self) {
            BOUNDARY.with(|s| {
                let owned = s.borrow().as_ref().is_some_and(|(_, id, _)| id == &self.id);
                if owned {
                    s.borrow_mut().take();
                }
            });
        }
    }
    pub(crate) fn preparation_boundary(home: &Path) {
        let Ok(home) = home.canonicalize() else {
            return;
        };
        let hook = BOUNDARY.with(|s| {
            let matches = s.borrow().as_ref().is_some_and(|(h, _, _)| *h == home);
            if matches {
                s.borrow_mut().take().map(|(_, _, f)| f)
            } else {
                None
            }
        });
        if let Some(hook) = hook {
            hook();
        }
    }
    pub(crate) struct Guard {
        home: PathBuf,
        counters: Arc<Counters>,
    }
    impl Guard {
        pub(crate) fn install(home: &Path) -> Self {
            let home = home.canonicalize().unwrap();
            let raw: serde_json::Value = std::fs::read(home.join(super::super::CLIENT_CONFIG_FILE))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_else(|| serde_json::json!([]));
            let counters = Arc::new(Counters {
                client: raw
                    .as_array()
                    .and_then(|rows| rows.iter().find(|v| v["provider_id"] == "google"))
                    .and_then(|v| v["client_id"].as_str())
                    .unwrap_or("no-owned-client")
                    .into(),
                crypto: AtomicUsize::new(0),
                provider: AtomicUsize::new(0),
                source_fences: AtomicUsize::new(0),
                source_payloads: AtomicUsize::new(0),
                google_crypto: AtomicUsize::new(0),
            });
            assert!(
                probes()
                    .lock()
                    .unwrap()
                    .insert(home.clone(), counters.clone())
                    .is_none()
            );
            Self { home, counters }
        }
        pub(crate) fn google_crypto_count(&self) -> usize {
            self.counters.google_crypto.load(Ordering::SeqCst)
        }
        pub(super) fn writer_counts(&self) -> [usize; 2] {
            [
                self.counters.source_fences.load(Ordering::SeqCst),
                self.counters.source_payloads.load(Ordering::SeqCst),
            ]
        }
        pub(crate) fn counts(&self) -> [usize; 2] {
            [
                self.counters.crypto.load(Ordering::SeqCst),
                self.counters.provider.load(Ordering::SeqCst),
            ]
        }
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            probes().lock().unwrap().remove(&self.home);
        }
    }
    pub(crate) fn source_fence(home: &Path) {
        if let Ok(home) = home.canonicalize() {
            if let Some(c) = probes().lock().unwrap().get(&home) {
                c.source_fences.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
    pub(crate) fn source_payload(home: &Path) {
        if let Ok(home) = home.canonicalize() {
            if let Some(c) = probes().lock().unwrap().get(&home) {
                c.source_payloads.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
    pub(crate) fn crypto(home: &Path) {
        if let Ok(home) = home.canonicalize() {
            if let Some(c) = probes().lock().unwrap().get(&home) {
                c.crypto.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
    pub(crate) fn google_crypto(home: &Path) {
        if let Ok(home) = home.canonicalize() {
            if let Some(c) = probes().lock().unwrap().get(&home) {
                c.google_crypto.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
    pub(crate) fn provider(client: &str) {
        for c in probes().lock().unwrap().values() {
            if c.client == client {
                c.provider.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
}

/// W3 observer: bounded same-handle reads only. No migration, mutex, flock,
/// SourceGuard, key store, refresh or repair. Absence is a real absence.
pub(crate) fn closed_readonly_control(home: &Path) -> Result<Value> {
    publication::ensure_unblocked(home)?;
    let path = home.join(TOKEN_FILE);
    let raw = match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return closed_observation_finished(home, Value::Null);
        }
        Err(_) => return Err(Error::Io),
        Ok(_) => {
            crate::google_identity::home_source::w3_observation::FileProof::capture(
                &path,
                4 * 1024 * 1024,
            )
            .map_err(|_| Error::Stale)?
            .1
        }
    };
    let rows: Vec<Value> = serde_json::from_slice(&raw).map_err(|_| Error::Invalid)?;
    let mut google = rows
        .iter()
        .filter(|r| r.get("provider_id").and_then(Value::as_str) == Some("google"));
    let Some(row) = google.next() else {
        return closed_observation_finished(home, Value::Null);
    };
    if google.next().is_some() {
        return Err(Error::Invalid);
    }
    let Some(value) = row.get("google_home") else {
        token_view(row)?;
        return closed_observation_finished(home, json!({"legacy_payload_digest":digest(row)?}));
    };
    let control: Control = serde_json::from_value(value.clone()).map_err(|_| Error::Invalid)?;
    if !control.valid() || control.credential.is_some() != row.get("access_token").is_some() {
        return Err(Error::Invalid);
    }
    control.ready()?;
    publication::ensure_unblocked(home)?;
    // Hash the Google payload within its existing owner. No raw row exits.
    Ok(
        json!({"source_generation":control.source_generation,"credential":control.credential,"payload_digest":digest(row)?,"oauth_payload_digest":payload_digest(row)?}),
    )
}

impl OAuthMaterialMetadata {
    pub(crate) fn w3_weight(&self) -> usize {
        std::mem::size_of_val(self)
            + self.credential.credential_id.capacity()
            + self.generation.capacity()
            + self.scopes.capacity() * std::mem::size_of::<String>()
            + self.scopes.iter().map(String::capacity).sum::<usize>()
    }
}

impl OAuthMaterialMetadata {
    pub(crate) fn w3_matches_control(&self, control: &Value) -> bool {
        control.get("source_generation").and_then(Value::as_str) == Some(self.generation.as_str())
            && serde_json::to_value(&self.credential).ok().as_ref() == control.get("credential")
            && serde_json::to_value(&self.payload).ok().as_ref()
                == control.get("oauth_payload_digest")
    }
}

fn closed_observation_finished(home: &Path, value: Value) -> Result<Value> {
    publication::ensure_unblocked(home)?;
    Ok(value)
}
