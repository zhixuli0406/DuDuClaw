//! Schema 5 (R8r-2): `provision_providers`, the root-side write of an isolated
//! employee's model connection document (`/home/employee/.duduclaw-api/
//! providers.json`, inside the employee's own storage image).
//!
//! A separate frame type on the same socket: the schema 1–4 [`Request`] and
//! [`super::Response`] are unchanged byte for byte and can never carry this
//! action, its payload or its codes ([`super::Action`] has no such variant,
//! `Request` denies unknown fields, and [`ProviderErrorCode`] is not
//! [`super::ErrorCode`]). A [`ProviderRequest`] is valid in schema 5 only.
//!
//! Two peers may send it, told apart by SO_PEERCRED ([`classify`]): the
//! gateway, naming the employee by its confirmation item (the same binding
//! every other action uses), and host root (uid 0 AND gid 0), naming it by
//! `employee_id` (32 lowercase hex) or by the exact label root's allocation
//! journal records. Root may send this action only ([`admits`]).
//!
//! The payload is opaque to root (never parsed): base64 in the frame,
//! 1..=65,536 bytes decoded, held in a zeroizing buffer from decode to write,
//! never `Clone`, never printed (`Debug` redacts). The reply carries
//! `{provisioned, updated_unix}` only — never the payload, never its hash.
use std::fmt;
use duduclaw_isolation::{ids::{EmployeeId, RequestId}, migration::{ConfirmationItem, ConfirmationStatus}};
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use zeroize::Zeroize;
/// The payload container (re-exported so callers need no direct dependency).
pub use zeroize::Zeroizing;
use super::{Request, Specification};
use crate::Error;

/// The `provision_providers` schema. Nothing else is valid in it.
pub const SCHEMA_V5: u16 = 5;
/// Largest decoded payload (bytes).
pub const MAX_PAYLOAD: usize = 65_536;
/// Largest padded base64 text of a [`MAX_PAYLOAD`] payload.
const MAX_TEXT: usize = MAX_PAYLOAD.div_ceil(3) * 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAction { ProvisionProviders }

/// Schema-5 refusal codes (fixed names; never carried by schemas 1–4).
/// `NotReady` is the existing "a run is live / the employee is in use, retry
/// later" meaning (the design's `busy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorCode {
    /// No journal entry that is Ready and not disabled, with a Ready registry
    /// row of the same employee and UID (same rule as `Entitle`).
    NotAllocated,
    NotReady,
    /// The payload is not canonical base64 of 1..=65,536 bytes.
    InvalidPayload,
    /// `.duduclaw-api` (or a `providers.json` directory) in the employee home
    /// is not what root may write through; nothing was written.
    HomeEntryUnsafe,
    /// The image could not be attached/written (quota, ENOSPC, I/O).
    StorageUnavailable,
    InvalidRequest,
    /// Includes the 4 s worker timeout: the same call may be sent again
    /// (every call rewrites).
    IsolationUnavailable,
}

/// The opaque provider document. `None` inside = the frame carried a payload
/// that is not canonical base64 of 1..=[`MAX_PAYLOAD`] bytes (answered
/// [`ProviderErrorCode::InvalidPayload`], never echoed).
/// Deliberately not `Clone`; `Debug` never shows the bytes.
pub struct Payload(Option<Zeroizing<Vec<u8>>>);
impl Payload {
    /// Takes ownership of `bytes` (no copy). Refuses an empty or oversized one.
    pub fn new(bytes: Zeroizing<Vec<u8>>) -> Result<Self, Error> {
        if bytes.is_empty() || bytes.len() > MAX_PAYLOAD { return Err(Error::Invalid); }
        Ok(Self(Some(bytes)))
    }
    /// The decoded bytes; `None` for an invalid payload.
    pub fn bytes(&self) -> Option<&[u8]> { self.0.as_ref().map(|bytes| bytes.as_slice()) }
}
impl fmt::Debug for Payload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0.is_some() { "Payload(<redacted>)" } else { "Payload(<invalid>)" })
    }
}
impl Serialize for Payload {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let bytes = self.bytes().ok_or_else(|| <S::Error as serde::ser::Error>::custom("invalid payload"))?;
        let text = encode(bytes);
        serializer.serialize_str(&text)
    }
}
impl<'de> Deserialize<'de> for Payload {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> de::Visitor<'de> for Visitor {
            type Value = Payload;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("a base64 payload") }
            /// Only text borrowed from the (zeroized) frame is decoded: no
            /// other copy of the base64 text is made here.
            fn visit_borrowed_str<E: de::Error>(self, text: &'de str) -> Result<Payload, E> { Ok(Payload(decode(text.as_bytes()))) }
            /// A string the parser had to unescape into its own scratch
            /// buffer is not canonical base64: invalid, and never echoed
            /// (serde's default would quote it in the error message).
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Payload, E> { Ok(Payload(None)) }
        }
        deserializer.deserialize_str(Visitor)
    }
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
/// Standard padded base64 into a zeroizing string of the exact final size.
fn encode(bytes: &[u8]) -> Zeroizing<String> {
    let mut out = Zeroizing::new(String::with_capacity(bytes.len().div_ceil(3) * 4));
    for chunk in bytes.chunks(3) {
        let mut n = (u32::from(chunk[0]) << 16) | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8) | u32::from(chunk.get(2).copied().unwrap_or(0));
        for i in 0..4 {
            out.push(if i <= chunk.len() { char::from(ALPHABET[(n >> (18 - 6 * i)) as usize & 63]) } else { '=' });
        }
        n.zeroize();
    }
    out
}
/// Strict decoding: standard alphabet, padding required (length a multiple
/// of 4), padding only at the end, zero unused bits, 1..=[`MAX_PAYLOAD`]
/// bytes. The output buffer is allocated once at its upper bound (no
/// reallocation leaves an unzeroized copy).
fn decode(text: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    if text.is_empty() || text.len() % 4 != 0 || text.len() > MAX_TEXT { return None; }
    let value = |c: u8| -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some(u32::from(c - b'A')),
            b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    let mut out = Zeroizing::new(Vec::with_capacity(text.len() / 4 * 3));
    let chunks = text.len() / 4;
    for (index, chunk) in text.chunks_exact(4).enumerate() {
        let pad = if index + 1 == chunks { chunk.iter().rev().take_while(|&&c| c == b'=').count() } else { 0 };
        if pad > 2 { return None; }
        let mut n = 0u32;
        for (position, &c) in chunk.iter().enumerate() {
            let sextet = if position >= 4 - pad { 0 } else { value(c)? };
            n = (n << 6) | sextet;
        }
        if (pad == 1 && n & 0xff != 0) || (pad == 2 && n & 0xffff != 0) { return None; }
        let mut bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&bytes[..3 - pad]);
        bytes.zeroize();
        n.zeroize();
    }
    if out.is_empty() || out.len() > MAX_PAYLOAD { return None; }
    Some(out)
}

/// One schema-5 request. Exactly one identification (gateway: `item`; host
/// root: `employee_id` or `label`) and exactly one of `payload` / `clear`.
/// Deliberately not `Clone`; `Debug` redacts the payload.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderRequest {
    pub schema: u16,
    pub nonce: RequestId,
    pub action: ProviderAction,
    /// Gateway peer only: the employee's confirmation item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ConfirmationItem>,
    /// Host root peer only: the `EmployeeId` as 32 lowercase hex digits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub employee_id: Option<String>,
    /// Host root peer only: the exact label of the allocation journal entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Payload>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub clear: bool,
}
fn is_false(value: &bool) -> bool { !*value }
impl fmt::Debug for ProviderRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderRequest").field("schema", &self.schema).field("nonce", &self.nonce).field("action", &self.action)
            .field("item", &self.item.as_ref().map(|item| item.binding)).field("employee_id", &self.employee_id)
            .field("label", &self.label).field("payload", &self.payload).field("clear", &self.clear).finish()
    }
}
/// Whom a checked request names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identification { Binding(Specification), Employee(EmployeeId), Label(String) }
impl ProviderRequest {
    /// What the socket server checks before queueing; a frame failing it is
    /// dropped unanswered, exactly like an invalid schema 1–4 frame.
    pub fn envelope(&self) -> Result<(), Error> {
        if self.schema != SCHEMA_V5 || self.nonce.0 == [0; 16] { return Err(Error::Invalid); }
        Ok(())
    }
    /// The complete shape check (root answers its code). The gateway's item
    /// must be a digest-bound Employee item in Applying or Applied (as for
    /// `Entitle`); a label follows the allocation label rule.
    pub fn check(&self) -> Result<Identification, ProviderErrorCode> {
        use ProviderErrorCode as Code;
        self.envelope().map_err(|_| Code::InvalidRequest)?;
        let identification = match (&self.item, &self.employee_id, &self.label) {
            (Some(item), None, None) => {
                if !matches!(item.status, ConfirmationStatus::Applying | ConfirmationStatus::Applied) { return Err(Code::InvalidRequest); }
                Identification::Binding(super::specification(item).map_err(|_| Code::InvalidRequest)?)
            },
            (None, Some(id), None) => Identification::Employee(parse_employee_id(id).ok_or(Code::InvalidRequest)?),
            (None, None, Some(label)) if super::valid_label(label) => Identification::Label(label.clone()),
            _ => return Err(Code::InvalidRequest),
        };
        match (&self.payload, self.clear) {
            (Some(payload), false) => if payload.bytes().is_none() { return Err(Code::InvalidPayload); },
            (None, true) => {},
            _ => return Err(Code::InvalidRequest),
        }
        Ok(identification)
    }
}
/// `EmployeeId` from exactly 32 lowercase hex digits (never all zero).
pub fn parse_employee_id(text: &str) -> Option<EmployeeId> {
    super::lower_hex::<16>(text).ok().filter(|bytes| *bytes != [0; 16]).map(EmployeeId)
}
/// The allocation label rule (1..=64 of ASCII letters, digits, `-`, `_`).
pub fn valid_label(label: &str) -> bool { super::valid_label(label) }

/// One frame on the allocation socket.
pub enum Frame { Allocation(Request), Providers(Box<ProviderRequest>) }
/// A schema 1–4 [`Request`] when the frame is one (unchanged parsing);
/// otherwise a [`ProviderRequest`]. The two can never both match.
pub fn decode_frame(bytes: &[u8]) -> Result<Frame, Error> {
    if let Ok(request) = serde_json::from_slice::<Request>(bytes) { return Ok(Frame::Allocation(request)); }
    serde_json::from_slice::<ProviderRequest>(bytes).map(|request| Frame::Providers(Box::new(request))).map_err(|_| Error::Invalid)
}

/// The authenticated socket peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Peer { Gateway, Root }
/// SO_PEERCRED `(uid, gid)` → peer. The gateway exactly as before; host root
/// only as uid 0 AND gid 0; anything else (or an unset gateway) is refused.
pub fn classify(credentials: (u32, u32), gateway: (u32, u32)) -> Option<Peer> {
    if gateway.0 == 0 || gateway.1 == 0 { return None; }
    if credentials == gateway { Some(Peer::Gateway) } else if credentials == (0, 0) { Some(Peer::Root) } else { None }
}
/// Every schema 1–4 action is the gateway's only. `provision_providers`:
/// the gateway names the employee by its item, root by id or label.
pub fn admits(peer: Peer, frame: &Frame) -> bool {
    match (peer, frame) {
        (Peer::Gateway, Frame::Allocation(_)) => true,
        (Peer::Root, Frame::Allocation(_)) => false,
        (Peer::Gateway, Frame::Providers(request)) => request.item.is_some() && request.employee_id.is_none() && request.label.is_none(),
        (Peer::Root, Frame::Providers(request)) => request.item.is_none() && request.employee_id.is_some() != request.label.is_some(),
    }
}

/// What root now records for the employee (and answers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provisioned { pub provisioned: bool, pub updated_unix: u64 }
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderResponse {
    pub schema: u16,
    pub nonce: RequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProviderErrorCode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provisioned: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_unix: Option<u64>,
}
impl ProviderResponse {
    pub fn answer(nonce: RequestId, result: Result<Provisioned, ProviderErrorCode>) -> Self {
        match result {
            Ok(done) => Self { schema: SCHEMA_V5, nonce, error: None, provisioned: Some(done.provisioned), updated_unix: Some(done.updated_unix) },
            Err(code) => Self { schema: SCHEMA_V5, nonce, error: Some(code), provisioned: None, updated_unix: None },
        }
    }
}
/// Client-side check: schema 5, the request's nonce, a code XOR a complete
/// success, and a success that matches what was asked (`clear` ⇒ not
/// provisioned, a payload ⇒ provisioned).
pub fn check_provider_response(nonce: RequestId, clear: bool, response: ProviderResponse)
    -> Result<Result<Provisioned, ProviderErrorCode>, Error> {
    if response.schema != SCHEMA_V5 || response.nonce != nonce { return Err(Error::Authentication); }
    match (response.error, response.provisioned, response.updated_unix) {
        (Some(code), None, None) => Ok(Err(code)),
        (None, Some(provisioned), Some(updated_unix)) if provisioned != clear => Ok(Ok(Provisioned { provisioned, updated_unix })),
        _ => Err(Error::Authentication),
    }
}

#[cfg(test)]
#[path = "providers_tests.rs"]
mod tests;
