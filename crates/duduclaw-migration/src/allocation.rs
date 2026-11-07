//! Private G/B1 allocation protocol. Only the authenticated gateway may submit
//! a proposal after the confirmation store has entered Applying.
use duduclaw_isolation::{bounded::Text, capability::EmployeeBinding, ids::{EmployeeId, RequestId, Sha256Digest}, migration::*};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use crate::Error;
pub const SOCKET: &str = "/run/duduclaw-spawnd/allocation.sock";
pub const MAX_BYTES: usize = 512 * 1024;
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all="snake_case")]
pub enum Action { Allocate, ReadBack, Disable }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request { pub schema: u16, pub nonce: RequestId, pub action: Action, pub item: ConfirmationItem }
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
pub struct Response { pub schema: u16, pub nonce: RequestId, pub receipt: Option<Receipt> }
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
/// `binding_kind` = "personal" requires `owner_id` (the owner's user id) and
/// forbids `tenant_id`; "shared" requires `tenant_id` and forbids `owner_id`.
/// The tenant pin itself lives in the gateway's apply.db, which this crate
/// cannot read: the gateway must check the proposal tenant equals its pin.
fn principal_binding(item: &ConfirmationItem) -> Result<Option<EmployeeBinding>, Error> {
    let field = |name: &str| -> Result<Option<&str>, Error> {
        match item.fields.iter().find(|f| f.name == name).map(|f| &f.value) {
            None => Ok(None),
            Some(ProposalValue::Text { value }) => Ok(Some(value.as_str())),
            Some(_) => Err(Error::Invalid),
        }
    };
    match (field("binding_kind")?, field("owner_id")?, field("tenant_id")?) {
        (None, None, None) => Ok(None),
        (Some("personal"), Some(owner), None) => Ok(Some(EmployeeBinding::Personal(principal_id(owner)?))),
        (Some("shared"), None, Some(tenant)) => Ok(Some(EmployeeBinding::Shared(principal_id(tenant)?))),
        _ => Err(Error::Invalid),
    }
}
pub fn specification(item: &ConfirmationItem) -> Result<Specification, Error> {
    crate::confirmation::verify_item_binding(item).map_err(|_| Error::Invalid)?;
    if item.category != ConfirmationCategory::Employee { return Err(Error::Invalid); }
    let text = |name: &str| -> Result<&str, Error> {
        match item.fields.iter().find(|f| f.name == name).map(|f| &f.value) {
            Some(ProposalValue::Text { value }) => Ok(value), _ => Err(Error::Invalid),
        }
    };
    let label = text("employee_id")?;
    if label.is_empty() || label.len() > 64 || !label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err(Error::Invalid);
    }
    let bytes: u64 = text("disk_bytes")?.parse().map_err(|_| Error::Invalid)?;
    if !(64 * 1024 * 1024..=1024 * 1024 * 1024 * 1024).contains(&bytes) { return Err(Error::Limit); }
    let binding = principal_binding(item)?;
    // Fixed root policy, never taken from unconfirmed settings.
    Ok(Specification { label: label.into(), bytes, inodes: (bytes / 16384).clamp(4096, 16_777_216), binding })
}
pub fn operation(binding: ProposalBinding, disable: bool) -> RequestId {
    let mut hash = Sha256::new();
    hash.update(b"duduclaw-confirmed-allocation-v1\0");
    hash.update(binding.operation_id.proposal_id.0);
    hash.update(binding.operation_id.revision.to_be_bytes());
    hash.update([u8::from(disable)]);
    let digest = hash.finalize();
    let mut id = [0; 16]; id.copy_from_slice(&digest[..16]); RequestId(id)
}
impl Request {
    pub fn validate(&self) -> Result<Specification, Error> {
        if self.schema != 1 || self.nonce.0 == [0; 16] { return Err(Error::Invalid); }
        let allowed = match self.action {
            Action::Allocate => self.item.status == ConfirmationStatus::Applying,
            Action::ReadBack => matches!(self.item.status, ConfirmationStatus::Applying | ConfirmationStatus::Applied),
            Action::Disable => matches!(self.item.status, ConfirmationStatus::Failed | ConfirmationStatus::Rejecting | ConfirmationStatus::Rejected),
        };
        if !allowed { return Err(Error::Authentication); }
        specification(&self.item)
    }
}
#[cfg(target_os="linux")]
pub mod transport;

#[cfg(test)]
mod tests {
    use super::*;
    fn employee(status: ConfirmationStatus) -> ConfirmationItem {
        let fields=vec![ProposalField{name:"disk_bytes".into(),value:ProposalValue::Text{value:"67108864".into()}},
            ProposalField{name:"employee_id".into(),value:ProposalValue::Text{value:"sales".into()}}];
        let mut hash=Sha256::new();hash.update(b"duduclaw-migration-proposal-v1\0");
        hash.update(serde_json::to_vec(&(ConfirmationCategory::Employee,&fields)).unwrap());
        ConfirmationItem{binding:ProposalBinding{operation_id:ProposalOperationId{proposal_id:RequestId([1;16]),revision:1},proposal_digest:Sha256Digest(hash.finalize().into())},
            category:ConfirmationCategory::Employee,status,high_risk:true,fields}
    }
    #[test]
    fn allocation_refuses_preconfirmation_and_changed_full_payload() {
        let mut request=Request{schema:1,nonce:RequestId([2;16]),action:Action::Allocate,item:employee(ConfirmationStatus::Pending)};
        assert_eq!(request.validate().unwrap_err(),Error::Authentication);
        request.item.status=ConfirmationStatus::Approved;
        assert_eq!(request.validate().unwrap_err(),Error::Authentication);
        request.item.status=ConfirmationStatus::Applying;
        assert_eq!(request.validate().unwrap().label,"sales");
        request.item.fields.push(ProposalField{name:"settings".into(),value:ProposalValue::Text{value:"tampered".into()}});
        assert!(request.validate().is_err());
    }
    #[test]
    fn allocation_disable_requires_terminal_failure_or_rejection() {
        let mut request=Request{schema:1,nonce:RequestId([2;16]),action:Action::Disable,item:employee(ConfirmationStatus::Applying)};
        assert!(request.validate().is_err());
        request.item.status=ConfirmationStatus::Failed;assert!(request.validate().is_ok());
        request.item.status=ConfirmationStatus::Rejecting;assert!(request.validate().is_ok());
    }
    fn bound(extra: &[(&str, &str)]) -> ConfirmationItem {
        let mut fields: Vec<ProposalField> = [("disk_bytes", "67108864"), ("employee_id", "sales")].iter().chain(extra)
            .map(|(n, v)| ProposalField { name: (*n).into(), value: ProposalValue::Text { value: (*v).into() } }).collect();
        fields.sort_by(|a, b| a.name.cmp(&b.name));
        let mut hash = Sha256::new(); hash.update(b"duduclaw-migration-proposal-v1\0");
        hash.update(serde_json::to_vec(&(ConfirmationCategory::Employee, &fields)).unwrap());
        ConfirmationItem { binding: ProposalBinding { operation_id: ProposalOperationId { proposal_id: RequestId([1;16]), revision: 1 }, proposal_digest: Sha256Digest(hash.finalize().into()) },
            category: ConfirmationCategory::Employee, status: ConfirmationStatus::Applying, high_risk: true, fields }
    }
    fn text(value: &str) -> Text<128> { Text::new(value.into()).unwrap() }
    #[test]
    fn allocation_specification_parses_personal_and_shared_binding() {
        let personal = specification(&bound(&[("binding_kind", "personal"), ("owner_id", "alice-uuid")])).unwrap();
        assert_eq!(personal.binding, Some(EmployeeBinding::Personal(text("alice-uuid"))));
        let shared = specification(&bound(&[("binding_kind", "shared"), ("tenant_id", "tenant-a")])).unwrap();
        assert_eq!(shared.binding, Some(EmployeeBinding::Shared(text("tenant-a"))));
        assert_eq!(specification(&bound(&[])).unwrap().binding, None, "legacy proposal stays unbound");
    }
    #[test]
    fn allocation_specification_rejects_invalid_binding_fields() {
        let long = "a".repeat(129);
        for extra in [
            vec![("binding_kind", "Personal"), ("owner_id", "alice")],
            vec![("binding_kind", "team"), ("owner_id", "alice")],
            vec![("binding_kind", ""), ("owner_id", "alice")],
            vec![("binding_kind", "personal")],
            vec![("binding_kind", "personal"), ("owner_id", "")],
            vec![("binding_kind", "personal"), ("owner_id", " alice")],
            vec![("binding_kind", "personal"), ("owner_id", "al\nice")],
            vec![("binding_kind", "personal"), ("owner_id", long.as_str())],
            vec![("binding_kind", "personal"), ("owner_id", "alice"), ("tenant_id", "tenant-a")],
            vec![("binding_kind", "shared")],
            vec![("binding_kind", "shared"), ("owner_id", "alice")],
            vec![("binding_kind", "shared"), ("owner_id", "alice"), ("tenant_id", "tenant-a")],
            vec![("binding_kind", "shared"), ("tenant_id", "")],
            vec![("owner_id", "alice")],
            vec![("tenant_id", "tenant-a")],
        ] {
            assert_eq!(specification(&bound(&extra)).unwrap_err(), Error::Invalid, "{extra:?}");
        }
        let mut secret = bound(&[("binding_kind", "personal")]);
        secret.fields.push(ProposalField { name: "owner_id".into(), value: ProposalValue::SecretFingerprint { keyed_fingerprint: Sha256Digest([1;32]), needs_rotation: false } });
        assert!(specification(&secret).is_err());
        // The binding is digest-covered: swapping the owner without re-digesting is stale.
        let mut swapped = bound(&[("binding_kind", "personal"), ("owner_id", "alice")]);
        for f in &mut swapped.fields { if f.name == "owner_id" { f.value = ProposalValue::Text { value: "bob".into() }; } }
        assert!(specification(&swapped).is_err());
    }
    #[test]
    fn allocation_receipt_legacy_json_reads_unbound_and_serializes_unchanged() {
        let legacy = Receipt { binding: bound(&[]).binding, label: "sales".into(), employee: EmployeeId([3;16]), uid: 70001, bytes: 67108864, inodes: 4096, disabled: false, principal_binding: None };
        let encoded = serde_json::to_string(&legacy).unwrap();
        assert!(!encoded.contains("principal_binding"));
        assert_eq!(serde_json::from_str::<Receipt>(&encoded).unwrap().principal_binding, None);
        let mut personal = legacy.clone(); personal.principal_binding = Some(EmployeeBinding::Personal(text("alice")));
        let encoded = serde_json::to_string(&personal).unwrap();
        assert!(encoded.contains(r#""principal_binding":{"kind":"personal","id":"alice"}"#), "{encoded}");
        assert_eq!(serde_json::from_str::<Receipt>(&encoded).unwrap().principal_binding, personal.principal_binding);
        assert!(serde_json::from_str::<Receipt>(&encoded.replace(r#""kind":"personal""#, r#""kind":"owner""#)).is_err());
        assert!(validate_principal_binding(&EmployeeBinding::Personal(text(""))).is_err());
        assert!(validate_principal_binding(&EmployeeBinding::Shared(text("tenant-a"))).is_ok());
    }
    #[test]
    fn allocation_operation_binds_revision_and_compensation() {
        let binding = ProposalBinding { operation_id: ProposalOperationId { proposal_id: RequestId([1;16]), revision:1 }, proposal_digest: Sha256Digest([2;32]) };
        assert_ne!(operation(binding,false),operation(binding,true));
        let mut next=binding; next.operation_id.revision+=1;
        assert_ne!(operation(binding,false),operation(next,false));
    }
}
