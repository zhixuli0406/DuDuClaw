//! Private G/B1 allocation protocol. Only the authenticated gateway may submit
//! a proposal after the confirmation store has entered Applying.
//!
//! Wire versions:
//! - Request/Response schema 1 (v1.80): `receipt` only; every refusal and a
//!   worker timeout reply `receipt: None`; ReadBack answers Ready entries only.
//! - Schema 2 (F6l, F6 design r5 §3.4/§3.12): the request may be a `Retire`
//!   (schema 2 only, with the approver id); the response adds `error`
//!   ([`ErrorCode`]), `phase` (ReadBack answers every phase, S5c) and `idle`
//!   (ReadBack of a Ready entry, S17). All three are omitted when `None`, so a
//!   schema-1 reply serializes exactly as before. A schema-1 request is
//!   always answered with schema-1 semantics.
use duduclaw_isolation::{bounded::Text, capability::EmployeeBinding, ids::{EmployeeId, RequestId, Sha256Digest,
    EMPLOYEE_UID_MAX, EMPLOYEE_UID_MIN}, migration::*};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use crate::Error;
pub const SOCKET: &str = "/run/duduclaw-spawnd/allocation.sock";
pub const MAX_BYTES: usize = 512 * 1024;
/// The schema that carries `Retire` and the schema-2 response fields.
pub const SCHEMA_V2: u16 = 2;
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all="snake_case")]
pub enum Action { Allocate, ReadBack, Disable, Retire }
/// Root allocation entry phase (spawnd journal schema 3, Response schema 2):
/// `Planned → Ready → Retiring → Retired`; `Planned | Ready → Disabling →
/// Disabled` (compensation). `Retiring | Retired` never go to `Disabling` (S6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="snake_case")]
pub enum Phase { Planned, Ready, Disabling, Disabled, Retiring, Retired }
impl Phase {
    /// Whether an entry in this phase still holds its label (Disabled and
    /// Retired entries do not; a Retiring entry keeps it until Retired).
    pub fn holds_label(self) -> bool { !matches!(self, Self::Disabled | Self::Retired) }
}
/// Schema-2 refusal codes. The names reuse `ipc::ErrorCode` where one exists;
/// `Inconsistent` is the allocation-only S4/S5b answer (root state disagrees
/// with the request or a trace of the entry survives elsewhere).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="snake_case")]
pub enum ErrorCode {
    /// Authoritative: journal, registry and storage show no trace (S5b).
    NotFound,
    Inconsistent,
    /// Host busy (live run) or the employee is not idle; retry later.
    NotReady,
    InvalidRequest,
    /// Includes the 4 s worker timeout (S5d); never a `NotFound`.
    IsolationUnavailable,
}
/// Operation-id kinds (F6 §3.12 step 1). The byte values are part of every
/// stored operation id: `Allocate` 0 and `Compensate` 1 equal the v1.80
/// `disable: bool` byte, so existing ids replay unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind { Allocate = 0, Compensate = 1, Retire = 2 }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema: u16, pub nonce: RequestId, pub action: Action, pub item: ConfirmationItem,
    /// `Retire` only (required there, refused elsewhere): the user id of the
    /// Admin who approved the retire card, from the item's approval record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approver: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub binding: ProposalBinding, pub label: String, pub employee: EmployeeId,
    pub uid: u32, pub bytes: u64, pub inodes: u64, pub disabled: bool,
    /// Immutable principal the root allocated this employee for, copied only
    /// from the confirmed proposal at first allocation. `None` is an unbound
    /// legacy (pre-v1.81) receipt: it is never evidence of a Personal owner.
    /// Omitted when `None` so legacy receipts serialize exactly as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal_binding: Option<EmployeeBinding>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub schema: u16, pub nonce: RequestId, pub receipt: Option<Receipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorCode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<Phase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle: Option<bool>,
}
/// What the root answered one request with (spawnd → serve → Response).
#[derive(Debug, Clone)]
pub struct Reply {
    pub receipt: Receipt,
    pub phase: Phase,
    /// ReadBack (schema 2) of a Ready entry: `Some(true)` only when every
    /// liveness source read zero; `None` when it could not be read.
    pub idle: Option<bool>,
}
impl Response {
    /// The wire reply for `request` (schema follows the request).
    pub fn for_request(request: &Request, result: Result<Reply, ErrorCode>) -> Self {
        if request.schema != SCHEMA_V2 {
            return Self { schema: 1, nonce: request.nonce, receipt: result.ok().map(|r| r.receipt), error: None, phase: None, idle: None };
        }
        match result {
            Ok(reply) => Self { schema: SCHEMA_V2, nonce: request.nonce, receipt: Some(reply.receipt), error: None,
                phase: Some(reply.phase), idle: reply.idle },
            Err(code) => Self { schema: SCHEMA_V2, nonce: request.nonce, receipt: None, error: Some(code), phase: None, idle: None },
        }
    }
}
/// `binding` is `None` only for a legacy proposal carrying none of
/// `binding_kind`, `owner_id` or `tenant_id`; such proposals were confirmed
/// before v1.81 and their digests cannot be rewritten. Any partial or
/// inconsistent combination is rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Specification { pub label: String, pub bytes: u64, pub inodes: u64, pub binding: Option<EmployeeBinding> }
/// Principal ids are opaque user UUIDs or tenant UUIDs: printable ASCII
/// without whitespace, 1..=128 bytes. No trimming or normalization.
fn principal_id(value: &str) -> Result<Text<128>, Error> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_graphic()) { return Err(Error::Invalid); }
    Text::new(value.into()).map_err(|_| Error::Invalid)
}
/// Revalidates a binding read back from durable storage.
pub fn validate_principal_binding(binding: &EmployeeBinding) -> Result<(), Error> {
    let (EmployeeBinding::Personal(id) | EmployeeBinding::Shared(id)) = binding;
    principal_id(id.as_str()).map(|_| ())
}
/// An Admin user id carried by a retire (requester, approver): same rule as
/// a principal id.
pub fn validate_actor_id(value: &str) -> Result<(), Error> { principal_id(value).map(|_| ()) }
fn field<'a>(item: &'a ConfirmationItem, name: &str) -> Result<Option<&'a str>, Error> {
    match item.fields.iter().find(|f| f.name == name).map(|f| &f.value) {
        None => Ok(None),
        Some(ProposalValue::Text { value }) => Ok(Some(value.as_str())),
        Some(_) => Err(Error::Invalid),
    }
}
fn required<'a>(item: &'a ConfirmationItem, name: &str) -> Result<&'a str, Error> { field(item, name)?.ok_or(Error::Invalid) }
/// `binding_kind` = "personal" requires `owner_id` (the owner's user id) and
/// forbids `tenant_id`; "shared" requires `tenant_id` and forbids `owner_id`.
/// The tenant pin itself lives in the gateway's apply.db, which this crate
/// cannot read: the gateway must check the proposal tenant equals its pin.
fn principal_binding(item: &ConfirmationItem) -> Result<Option<EmployeeBinding>, Error> {
    match (field(item, "binding_kind")?, field(item, "owner_id")?, field(item, "tenant_id")?) {
        (None, None, None) => Ok(None),
        (Some("personal"), Some(owner), None) => Ok(Some(EmployeeBinding::Personal(principal_id(owner)?))),
        (Some("shared"), None, Some(tenant)) => Ok(Some(EmployeeBinding::Shared(principal_id(tenant)?))),
        _ => Err(Error::Invalid),
    }
}
fn valid_label(label: &str) -> bool {
    !label.is_empty() && label.len() <= 64 && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
pub fn specification(item: &ConfirmationItem) -> Result<Specification, Error> {
    crate::confirmation::verify_item_binding(item).map_err(|_| Error::Invalid)?;
    if item.category != ConfirmationCategory::Employee { return Err(Error::Invalid); }
    let label = required(item, "employee_id")?;
    if !valid_label(label) { return Err(Error::Invalid); }
    let bytes: u64 = required(item, "disk_bytes")?.parse().map_err(|_| Error::Invalid)?;
    if !(64 * 1024 * 1024..=1024 * 1024 * 1024 * 1024).contains(&bytes) { return Err(Error::Limit); }
    let binding = principal_binding(item)?;
    // Fixed root policy, never taken from unconfirmed settings.
    Ok(Specification { label: label.into(), bytes, inodes: (bytes / 16384).clamp(4096, 16_777_216), binding })
}

/// Every field of an `EmployeeRetire` proposal (sorted; all required).
pub const RETIRE_FIELDS: &[&str] = &["early", "employee_hex", "employee_id", "quarantine_digest_prefix", "reason",
    "requested_by", "storage", "target_proposal_digest", "target_proposal_id", "target_revision", "uid"];
/// The one retire reason and storage policy of v1.81 (R12, R13).
pub const RETIRE_REASON: &str = "purged";
pub const RETIRE_STORAGE: &str = "wipe";
/// Length of `quarantine_digest_prefix` (hex characters).
pub const DIGEST_PREFIX_CHARS: usize = 16;
/// A parsed, digest-checked `EmployeeRetire` proposal (F6 §3.12, R12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetireSpec {
    pub label: String,
    /// The Applied Employee allocation being retired (operation id + digest).
    pub target: ProposalBinding,
    pub employee: EmployeeId,
    pub uid: u32,
    pub early: bool,
    pub quarantine_digest_prefix: String,
    pub requested_by: String,
}
fn lower_hex<const N: usize>(text: &str) -> Result<[u8; N], Error> {
    let digits = text.as_bytes();
    if digits.len() != N.checked_mul(2).ok_or(Error::Invalid)? { return Err(Error::Invalid); }
    let nibble = |b: u8| -> Result<u8, Error> {
        match b { b'0'..=b'9' => Ok(b - b'0'), b'a'..=b'f' => Ok(b - b'a' + 10), _ => Err(Error::Invalid) }
    };
    let mut out = [0u8; N];
    for (slot, pair) in out.iter_mut().zip(digits.chunks_exact(2)) {
        let [high, low] = pair else { return Err(Error::Invalid) };
        *slot = (nibble(*high)? << 4) | nibble(*low)?;
    }
    Ok(out)
}
/// Canonical unsigned decimal: no sign, no leading zero (except "0").
fn decimal(text: &str) -> Result<u64, Error> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) || (text.len() > 1 && text.starts_with('0')) {
        return Err(Error::Invalid);
    }
    text.parse().map_err(|_| Error::Invalid)
}
/// Strict parse of a retire item: digest-covered (verify_item_binding), the
/// exact field set, canonical encodings, the employee UID range.
pub fn retire_specification(item: &ConfirmationItem) -> Result<RetireSpec, Error> {
    crate::confirmation::verify_item_binding(item).map_err(|_| Error::Invalid)?;
    if item.category != ConfirmationCategory::EmployeeRetire || item.fields.len() != RETIRE_FIELDS.len() {
        return Err(Error::Invalid);
    }
    let label = required(item, "employee_id")?;
    if !valid_label(label) { return Err(Error::Invalid); }
    let proposal_id = RequestId(lower_hex::<16>(required(item, "target_proposal_id")?)?);
    let revision = decimal(required(item, "target_revision")?)?;
    if revision == 0 || revision > i64::MAX as u64 || proposal_id.0 == [0; 16] { return Err(Error::Invalid); }
    let digest = Sha256Digest(lower_hex::<32>(required(item, "target_proposal_digest")?)?);
    let employee = EmployeeId(lower_hex::<16>(required(item, "employee_hex")?)?);
    if employee.0 == [0; 16] { return Err(Error::Invalid); }
    let uid = u32::try_from(decimal(required(item, "uid")?)?).map_err(|_| Error::Invalid)?;
    if !(EMPLOYEE_UID_MIN..=EMPLOYEE_UID_MAX).contains(&uid) { return Err(Error::Invalid); }
    let early = match required(item, "early")? { "true" => true, "false" => false, _ => return Err(Error::Invalid) };
    let prefix = required(item, "quarantine_digest_prefix")?;
    if prefix.len() != DIGEST_PREFIX_CHARS || !prefix.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(Error::Invalid);
    }
    let requested_by = required(item, "requested_by")?;
    validate_actor_id(requested_by)?;
    if required(item, "reason")? != RETIRE_REASON || required(item, "storage")? != RETIRE_STORAGE { return Err(Error::Invalid); }
    Ok(RetireSpec {
        label: label.into(),
        target: ProposalBinding { operation_id: ProposalOperationId { proposal_id, revision }, proposal_digest: digest },
        employee, uid, early, quarantine_digest_prefix: prefix.into(), requested_by: requested_by.into(),
    })
}
/// The digest-covered field values of a retire proposal for `spec` (the
/// gateway stages exactly these; `retire_specification` reads them back).
pub fn retire_fields(spec: &RetireSpec) -> Vec<ProposalField> {
    let text = |name: &str, value: String| ProposalField { name: name.into(), value: ProposalValue::Text { value } };
    let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let mut fields = vec![
        text("early", spec.early.to_string()),
        text("employee_hex", hex(&spec.employee.0)),
        text("employee_id", spec.label.clone()),
        text("quarantine_digest_prefix", spec.quarantine_digest_prefix.clone()),
        text("reason", RETIRE_REASON.into()),
        text("requested_by", spec.requested_by.clone()),
        text("storage", RETIRE_STORAGE.into()),
        text("target_proposal_digest", hex(&spec.target.proposal_digest.0)),
        text("target_proposal_id", hex(&spec.target.operation_id.proposal_id.0)),
        text("target_revision", spec.target.operation_id.revision.to_string()),
        text("uid", spec.uid.to_string()),
    ];
    fields.sort_by(|a, b| a.name.cmp(&b.name));
    fields
}

pub fn operation(binding: ProposalBinding, kind: OperationKind) -> RequestId {
    let mut hash = Sha256::new();
    hash.update(b"duduclaw-confirmed-allocation-v1\0");
    hash.update(binding.operation_id.proposal_id.0);
    hash.update(binding.operation_id.revision.to_be_bytes());
    hash.update([kind as u8]);
    let digest = hash.finalize();
    let mut id = [0; 16];
    for (slot, byte) in id.iter_mut().zip(digest.iter()) { *slot = *byte; }
    RequestId(id)
}
impl Request {
    fn envelope(&self) -> Result<(), Error> {
        if !matches!(self.schema, 1 | SCHEMA_V2) || self.nonce.0 == [0; 16] { return Err(Error::Invalid); }
        Ok(())
    }
    /// Allocate / ReadBack / Disable of an Employee item. A `Retire` request
    /// is never valid here, so it can never reach a path that creates an entry.
    pub fn validate(&self) -> Result<Specification, Error> {
        self.envelope()?;
        if self.approver.is_some() { return Err(Error::Invalid); }
        let allowed = match self.action {
            Action::Allocate => self.item.status == ConfirmationStatus::Applying,
            Action::ReadBack => matches!(self.item.status, ConfirmationStatus::Applying | ConfirmationStatus::Applied),
            Action::Disable => matches!(self.item.status, ConfirmationStatus::Failed | ConfirmationStatus::Rejecting | ConfirmationStatus::Rejected),
            Action::Retire => return Err(Error::Invalid),
        };
        if !allowed { return Err(Error::Authentication); }
        specification(&self.item)
    }
    /// Retire-only validation (S6): schema 2, `Retire`, an `EmployeeRetire`
    /// item in `Applying`, a valid approver id.
    pub fn validate_retire(&self) -> Result<RetireSpec, Error> {
        self.envelope()?;
        if self.schema != SCHEMA_V2 || self.action != Action::Retire { return Err(Error::Invalid); }
        if self.item.status != ConfirmationStatus::Applying { return Err(Error::Authentication); }
        validate_actor_id(self.approver.as_deref().ok_or(Error::Invalid)?)?;
        retire_specification(&self.item)
    }
    /// Either validation, by action (the serve loop drops anything else).
    pub fn check(&self) -> Result<(), Error> {
        if self.action == Action::Retire { self.validate_retire().map(|_| ()) } else { self.validate().map(|_| ()) }
    }
}
/// Schema-2 reply as the gateway sees it after [`check_response`].
#[derive(Debug, Clone)]
pub enum Answer {
    Reply(Reply),
    Refused(ErrorCode),
}
/// Client-side validation of a schema-2 response to `request`: nonce, the
/// error/receipt exclusivity, and a receipt that matches what was asked
/// (Retire: the target allocation, label, employee, UID; phase Retired).
pub fn check_response(request: &Request, response: Response) -> Result<Answer, Error> {
    if request.schema != SCHEMA_V2 || response.schema != SCHEMA_V2 || response.nonce != request.nonce {
        return Err(Error::Authentication);
    }
    match (response.error, response.receipt, response.phase) {
        (Some(code), None, None) if response.idle.is_none() => return Ok(Answer::Refused(code)),
        (None, Some(receipt), Some(phase)) => {
            if receipt.employee.0 == [0; 16] || (receipt.uid < 1000 && !(receipt.disabled && receipt.uid == 0)) {
                return Err(Error::Authentication);
            }
            let ok = match request.action {
                Action::Retire => {
                    let spec = request.validate_retire()?;
                    receipt.binding == spec.target && receipt.label == spec.label && receipt.employee == spec.employee
                        && receipt.uid == spec.uid && receipt.disabled && phase == Phase::Retired && response.idle.is_none()
                }
                action => {
                    let spec = request.validate()?;
                    let same = receipt.binding == request.item.binding && receipt.label == spec.label
                        && receipt.bytes == spec.bytes && receipt.inodes == spec.inodes && receipt.principal_binding == spec.binding;
                    same && match action {
                        Action::Allocate => phase == Phase::Ready && !receipt.disabled && response.idle.is_none(),
                        Action::Disable => phase == Phase::Disabled && receipt.disabled && response.idle.is_none(),
                        // Every phase (S5c); `disabled` only in the dead phases; idle only for Ready.
                        _ => receipt.disabled == matches!(phase, Phase::Disabled | Phase::Retired)
                            && (response.idle.is_none() || phase == Phase::Ready),
                    }
                }
            };
            if !ok { return Err(Error::Authentication); }
            Ok(Answer::Reply(Reply { receipt, phase, idle: response.idle }))
        }
        _ => Err(Error::Authentication),
    }
}
#[cfg(target_os="linux")]
pub mod transport;

#[cfg(test)]
#[path = "allocation_tests.rs"]
mod tests;
