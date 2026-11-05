//! Private G/B1 allocation protocol. Only the authenticated gateway may submit
//! a proposal after the confirmation store has entered Applying.
use duduclaw_isolation::{ids::{EmployeeId, RequestId, Sha256Digest}, migration::*};
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
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response { pub schema: u16, pub nonce: RequestId, pub receipt: Option<Receipt> }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Specification { pub label: String, pub bytes: u64, pub inodes: u64 }
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
    // Fixed root policy, never taken from unconfirmed settings.
    Ok(Specification { label: label.into(), bytes, inodes: (bytes / 16384).clamp(4096, 16_777_216) })
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
    #[test]
    fn allocation_operation_binds_revision_and_compensation() {
        let binding = ProposalBinding { operation_id: ProposalOperationId { proposal_id: RequestId([1;16]), revision:1 }, proposal_digest: Sha256Digest([2;32]) };
        assert_ne!(operation(binding,false),operation(binding,true));
        let mut next=binding; next.operation_id.revision+=1;
        assert_ne!(operation(binding,false),operation(next,false));
    }
}
