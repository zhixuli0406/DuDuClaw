//! Durable content receipts. Archiving is never a terminal materialization receipt.
//! External product writes happen between begin() and finish(), outside transactions.
use std::path::PathBuf;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use crate::{Error, contract::{ImportRecord, ImportedContent, ProposalBinding}, ids::{EmployeeId, RequestId, Sha256Digest}, import_receiver::ImportGrant};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmedAllocation {
    pub proposal: ProposalBinding,
    pub employee: EmployeeId,
    pub agent: String,
    pub uid: u32,
    pub allocation_digest: Sha256Digest,
    pub quota_bytes: u64,
    pub workspace: PathBuf,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag="decision", rename_all="snake_case", deny_unknown_fields)]
pub enum MaterializationDecision {
    Confirmed { allocation: ConfirmedAllocation },
    Rejected { proposal: ProposalBinding },
    Retained { policy: String },
}
impl MaterializationDecision {
    pub fn validate(&self, grant: &ImportGrant) -> Result<(), Error> {
        match self {
            Self::Confirmed { allocation: a } => {
                if Some(a.employee)!=grant.employee || a.uid==0 || a.quota_bytes==0
                    || a.allocation_digest.0==[0;32] || a.agent.is_empty() || a.agent.len()>64
                    || a.agent.starts_with('_') || !a.agent.bytes().all(|b|b.is_ascii_alphanumeric()||matches!(b,b'-'|b'_'))
                    || !a.workspace.is_absolute() || a.workspace.components().any(|p| !matches!(p,std::path::Component::RootDir|std::path::Component::Normal(_))) {
                    return Err(Error::Invalid);
                }
            }
            Self::Rejected { .. } => if grant.employee.is_none() {return Err(Error::Invalid);},
            Self::Retained { policy } => if policy!="administrator_read_only_history" || !matches!(grant.class,
                crate::import_receiver::ContentClass::TaskHistory|crate::import_receiver::ContentClass::LegacyAuditArchive) {return Err(Error::Invalid);},
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlobCopyReceipt {
    pub source_handle: RequestId,
    pub destination: PathBuf,
    pub sha256: Sha256Digest,
    pub size_bytes: u64,
    pub allocation_digest: Sha256Digest,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag="outcome",rename_all="snake_case",deny_unknown_fields)]
pub enum RecordReceipt {
    Materialized { readback: Sha256Digest, blob_copy: Option<BlobCopyReceipt> },
    Rejected,
    Retained { policy: String },
}
fn encoded<T:Serialize>(value:&T)->Result<String,Error>{Ok(serde_json::to_string(value)?)}
pub fn payload_digest(record:&ImportRecord)->Result<Sha256Digest,Error>{Ok(Sha256Digest(Sha256::digest(serde_json::to_vec(record)?).into()))}
pub struct MaterializationStore { db: Connection }
impl MaterializationStore {
    pub fn from_connection(db:Connection)->Result<Self,Error>{
        db.execute_batch("PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS content_streams (migration TEXT NOT NULL, source TEXT NOT NULL, grant TEXT NOT NULL, PRIMARY KEY(migration,source));
            CREATE TABLE IF NOT EXISTS content_receipts (migration TEXT NOT NULL, source TEXT NOT NULL, record TEXT NOT NULL,
                payload_digest TEXT NOT NULL, decision TEXT, receipt TEXT, PRIMARY KEY(migration,source,record));")?;
        Ok(Self{db})
    }
    pub fn open(path:&std::path::Path)->Result<Self,Error>{Self::from_connection(Connection::open(path)?)}
    pub fn open_read_only(path:&std::path::Path)->Result<Self,Error>{
        Ok(Self{db:Connection::open_with_flags(path,rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY|rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)?})
    }
    /// Seal an authenticated stream and its complete inventory in one transaction.
    pub fn register(&mut self,grant:&ImportGrant,records:&[ImportRecord])->Result<(),Error>{
        if records.len() as u64!=grant.records{return Err(Error::Readback);}
        let migration=encoded(&grant.migration_id)?;let g=encoded(grant)?;
        let tx=self.db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let old:Option<String>=tx.query_row("SELECT grant FROM content_streams WHERE migration=?1 AND source=?2",params![migration,grant.source_id],|r|r.get(0)).optional()?;
        if old.as_ref().is_some_and(|v|v!=&g){return Err(Error::Conflict);}
        let mut ids=std::collections::BTreeSet::new();
        for record in records {
            record.validate().map_err(|_|Error::Invalid)?;
            if record.migration_id!=grant.migration_id || record.employee!=grant.employee || !ids.insert(&record.source_record_id){return Err(Error::Invalid);}
            let digest=encoded(&payload_digest(record)?)?;
            let old:Option<String>=tx.query_row("SELECT payload_digest FROM content_receipts WHERE migration=?1 AND source=?2 AND record=?3",params![migration,grant.source_id,record.source_record_id],|r|r.get(0)).optional()?;
            if old.as_ref().is_some_and(|v|v!=&digest){return Err(Error::Conflict);}
            tx.execute("INSERT OR IGNORE INTO content_receipts(migration,source,record,payload_digest) VALUES(?1,?2,?3,?4)",params![migration,grant.source_id,record.source_record_id,digest])?;
        }
        tx.execute("INSERT OR IGNORE INTO content_streams VALUES(?1,?2,?3)",params![migration,grant.source_id,g])?;
        tx.commit()?;Ok(())
    }
    /// Persist the exact approved allocation before any product effect. Returns
    /// false for a terminal replay; a different allocation can never reuse it.
    pub fn begin(&self,grant:&ImportGrant,record:&ImportRecord,decision:&MaterializationDecision)->Result<bool,Error>{
        decision.validate(grant)?;
        let migration=encoded(&grant.migration_id)?;let d=encoded(decision)?;let hash=encoded(&payload_digest(record)?)?;
        self.db.execute("UPDATE content_receipts SET decision=?1 WHERE migration=?2 AND source=?3 AND record=?4 AND payload_digest=?5 AND decision IS NULL",
            params![d,migration,grant.source_id,record.source_record_id,hash])?;
        let (stored,receipt):(String,Option<String>)=self.db.query_row("SELECT decision,receipt FROM content_receipts WHERE migration=?1 AND source=?2 AND record=?3 AND payload_digest=?4",
            params![migration,grant.source_id,record.source_record_id,hash],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if stored!=d{return Err(Error::Conflict);}Ok(receipt.is_none())
    }
    pub fn finish(&self,grant:&ImportGrant,record:&ImportRecord,decision:&MaterializationDecision,receipt:&RecordReceipt)->Result<(),Error>{
        decision.validate(grant)?;
        match (decision,receipt) {
            (MaterializationDecision::Confirmed{allocation},RecordReceipt::Materialized{readback,blob_copy})=>{
                if *readback!=payload_digest(record)?{return Err(Error::Readback);}
                match (&record.content,blob_copy) {
                    (ImportedContent::Attachment{blob}|ImportedContent::WorkProduct{blob},Some(copy))=>{
                        if copy.source_handle!=blob.source_handle||copy.sha256!=blob.sha256||copy.size_bytes!=blob.size_bytes
                            ||copy.allocation_digest!=allocation.allocation_digest || !copy.destination.is_absolute(){return Err(Error::Readback);}
                    }
                    (ImportedContent::Attachment{..}|ImportedContent::WorkProduct{..},None)=>return Err(Error::Readback),
                    (_,Some(_))=>return Err(Error::Invalid),
                    _=>{},
                }
            }
            (MaterializationDecision::Rejected{..},RecordReceipt::Rejected)=>{},
            (MaterializationDecision::Retained{policy:a},RecordReceipt::Retained{policy:b}) if a==b=>{},
            // Histories deliberately remain inert; the policy is explicit in the receipt.
            (MaterializationDecision::Confirmed{..},RecordReceipt::Retained{policy})
                if policy=="administrator_read_only_history" && matches!(record.content,ImportedContent::TaskHistory{..}|ImportedContent::LegacyAuditArchive{..})=>{},
            _=>return Err(Error::Invalid),
        }
        let receipt=encoded(receipt)?;let decision=encoded(decision)?;
        let changed=self.db.execute("UPDATE content_receipts SET receipt=?1 WHERE migration=?2 AND source=?3 AND record=?4 AND payload_digest=?5 AND decision=?6 AND (receipt IS NULL OR receipt=?1)",
            params![receipt,encoded(&grant.migration_id)?,grant.source_id,record.source_record_id,encoded(&payload_digest(record)?)?,decision])?;
        if changed!=1{return Err(Error::Conflict);}Ok(())
    }
    pub fn all_terminal(&self,grants:&[ImportGrant])->Result<bool,Error>{
        for grant in grants {
            let migration=encoded(&grant.migration_id)?;
            let old:Option<String>=self.db.query_row("SELECT grant FROM content_streams WHERE migration=?1 AND source=?2",params![migration,grant.source_id],|r|r.get(0)).optional()?;
            if old.as_deref()!=Some(encoded(grant)?.as_str()){return Ok(false);}
            let (count,terminal):(u64,u64)=self.db.query_row("SELECT COUNT(*),COUNT(receipt) FROM content_receipts WHERE migration=?1 AND source=?2",params![migration,grant.source_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if count!=grant.records||terminal!=count{return Ok(false);}
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{contract::{ProposalOperationId, ImportedBlob}, import_receiver::ContentClass};
    fn fixture()->(ImportGrant,ImportRecord,MaterializationDecision){
        let employee=EmployeeId([2;16]);
        let grant=ImportGrant{migration_id:RequestId([1;16]),employee:Some(employee),source_id:"stream".into(),class:ContentClass::Memory,
            stream_bytes:1,records:1,stream_sha256:Sha256Digest([3;32])};
        let record=ImportRecord{schema_version:1,migration_id:grant.migration_id,sequence:0,employee:grant.employee,
            source_record_id:"record".into(),content:ImportedContent::Memory{text:"legacy fact".into()}};
        let allocation=ConfirmedAllocation{proposal:ProposalBinding{operation_id:ProposalOperationId{proposal_id:RequestId([4;16]),revision:1},proposal_digest:Sha256Digest([5;32])},
            employee,agent:"sales".into(),uid:200000,allocation_digest:Sha256Digest([6;32]),quota_bytes:1024,workspace:"/workspace/sales".into()};
        (grant,record,MaterializationDecision::Confirmed{allocation})
    }
    #[test]
    fn archive_only_and_intent_never_count_as_imported_or_allow_cleanup(){
        let (grant,record,decision)=fixture();
        let mut store=MaterializationStore::from_connection(Connection::open_in_memory().unwrap()).unwrap();
        assert!(!store.all_terminal(&[grant.clone()]).unwrap());
        store.register(&grant,&[record.clone()]).unwrap();
        assert!(!store.all_terminal(&[grant.clone()]).unwrap());
        assert!(store.begin(&grant,&record,&decision).unwrap());
        assert!(!store.all_terminal(&[grant.clone()]).unwrap());
        assert!(store.finish(&grant,&record,&decision,&RecordReceipt::Materialized{readback:Sha256Digest([0;32]),blob_copy:None}).is_err());
        store.finish(&grant,&record,&decision,&RecordReceipt::Materialized{readback:payload_digest(&record).unwrap(),blob_copy:None}).unwrap();
        assert!(store.all_terminal(&[grant]).unwrap());
    }
    #[test]
    fn crash_resume_after_product_effect_reuses_identity_and_rejects_rebinding(){
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("receipts.db");
        let (grant,record,decision)=fixture();
        let mut store=MaterializationStore::open(&path).unwrap();store.register(&grant,&[record.clone()]).unwrap();
        assert!(store.begin(&grant,&record,&decision).unwrap());
        // Product adapter publishes by this same record identity, then crashes
        // before receipt CAS. The durable intent survives reopening.
        drop(store);let store=MaterializationStore::open(&path).unwrap();
        assert!(store.begin(&grant,&record,&decision).unwrap());
        let mut other=decision.clone();if let MaterializationDecision::Confirmed{allocation}= &mut other {allocation.uid+=1;}
        assert_eq!(store.begin(&grant,&record,&other),Err(Error::Conflict));
        let receipt=RecordReceipt::Materialized{readback:payload_digest(&record).unwrap(),blob_copy:None};
        store.finish(&grant,&record,&decision,&receipt).unwrap();
        drop(store);let store=MaterializationStore::open(&path).unwrap();
        assert!(!store.begin(&grant,&record,&decision).unwrap());
        store.finish(&grant,&record,&decision,&receipt).unwrap();
        assert!(store.all_terminal(&[grant]).unwrap());
    }
    #[test]
    fn blobs_require_copy_and_readback_receipts_bound_to_allocation(){
        let (mut grant,mut record,decision)=fixture();grant.class=ContentClass::Attachment;
        let blob=ImportedBlob{source_handle:RequestId([7;16]),display_name:"report".into(),size_bytes:5,sha256:Sha256Digest([8;32])};
        record.content=ImportedContent::Attachment{blob:blob.clone()};
        let mut store=MaterializationStore::from_connection(Connection::open_in_memory().unwrap()).unwrap();store.register(&grant,&[record.clone()]).unwrap();store.begin(&grant,&record,&decision).unwrap();
        let digest=payload_digest(&record).unwrap();
        assert!(store.finish(&grant,&record,&decision,&RecordReceipt::Materialized{readback:digest,blob_copy:None}).is_err());
        let mut copy=BlobCopyReceipt{source_handle:blob.source_handle,destination:"/attachments/import-file".into(),sha256:blob.sha256,size_bytes:5,allocation_digest:Sha256Digest([9;32])};
        assert!(store.finish(&grant,&record,&decision,&RecordReceipt::Materialized{readback:digest,blob_copy:Some(copy.clone())}).is_err());
        copy.allocation_digest=Sha256Digest([6;32]);
        store.finish(&grant,&record,&decision,&RecordReceipt::Materialized{readback:digest,blob_copy:Some(copy)}).unwrap();
        assert!(store.all_terminal(&[grant]).unwrap());
    }
    #[test]
    fn rejection_is_explicit_and_not_materialization(){
        let (grant,record,decision)=fixture();let MaterializationDecision::Confirmed{allocation}=decision else{unreachable!()};
        let rejection=MaterializationDecision::Rejected{proposal:allocation.proposal};
        let mut store=MaterializationStore::from_connection(Connection::open_in_memory().unwrap()).unwrap();store.register(&grant,&[record.clone()]).unwrap();
        store.begin(&grant,&record,&rejection).unwrap();
        assert!(store.finish(&grant,&record,&rejection,&RecordReceipt::Materialized{readback:payload_digest(&record).unwrap(),blob_copy:None}).is_err());
        store.finish(&grant,&record,&rejection,&RecordReceipt::Rejected).unwrap();
        assert!(store.all_terminal(&[grant]).unwrap());
    }
    #[test]
    fn every_record_and_stream_needs_a_terminal_receipt(){
        let (mut grant,record,decision)=fixture();grant.records=2;let mut second=record.clone();second.source_record_id="second".into();second.sequence=1;
        let mut store=MaterializationStore::from_connection(Connection::open_in_memory().unwrap()).unwrap();store.register(&grant,&[record.clone(),second.clone()]).unwrap();
        store.begin(&grant,&record,&decision).unwrap();store.finish(&grant,&record,&decision,&RecordReceipt::Materialized{readback:payload_digest(&record).unwrap(),blob_copy:None}).unwrap();
        assert!(!store.all_terminal(&[grant.clone()]).unwrap());
        store.begin(&grant,&second,&decision).unwrap();store.finish(&grant,&second,&decision,&RecordReceipt::Materialized{readback:payload_digest(&second).unwrap(),blob_copy:None}).unwrap();
        assert!(store.all_terminal(&[grant.clone()]).unwrap());
        let mut missing=grant.clone();missing.source_id="missing".into();assert!(!store.all_terminal(&[grant,missing]).unwrap());
    }
}

#[cfg(test)]
mod partial_batch_tests {
    use super::*;
    use crate::{contract::{ProposalOperationId, ImportedBlob}, import_receiver::ContentClass};
    #[test]
    fn later_batches_need_only_unfinished_streams_after_product_changes(){
        let dir=tempfile::tempdir().unwrap();
        let db=dir.path().join("receipts.db");let product=dir.path().join("attachment");
        std::fs::write(&product,b"first").unwrap();
        let mut a=ImportGrant{migration_id:RequestId([1;16]),employee:Some(EmployeeId([2;16])),source_id:"a".into(),
            class:ContentClass::Attachment,stream_bytes:1,records:1,stream_sha256:Sha256Digest([3;32])};
        let allocation=ConfirmedAllocation{proposal:ProposalBinding{operation_id:ProposalOperationId{proposal_id:RequestId([4;16]),revision:1},proposal_digest:Sha256Digest([5;32])},
            employee:EmployeeId([2;16]),agent:"a".into(),uid:200000,allocation_digest:Sha256Digest([6;32]),quota_bytes:1024,workspace:dir.path().into()};
        let decision=MaterializationDecision::Confirmed{allocation:allocation.clone()};
        let blob=ImportedBlob{source_handle:RequestId([7;16]),display_name:"file".into(),size_bytes:5,sha256:Sha256Digest(Sha256::digest(b"first").into())};
        let record=ImportRecord{schema_version:1,migration_id:a.migration_id,sequence:0,employee:a.employee,source_record_id:"a:0".into(),content:ImportedContent::Attachment{blob:blob.clone()}};
        let mut ledger=MaterializationStore::open(&db).unwrap();ledger.register(&a,&[record.clone()]).unwrap();ledger.begin(&a,&record,&decision).unwrap();
        ledger.finish(&a,&record,&decision,&RecordReceipt::Materialized{readback:payload_digest(&record).unwrap(),blob_copy:Some(BlobCopyReceipt{
            source_handle:blob.source_handle,destination:product.clone(),sha256:blob.sha256,size_bytes:5,allocation_digest:allocation.allocation_digest})}).unwrap();
        let mut b=a.clone();b.source_id="b".into();b.employee=Some(EmployeeId([9;16]));let mut pending=record.clone();pending.employee=b.employee;pending.source_record_id="b:0".into();
        ledger.register(&b,&[pending]).unwrap();drop(ledger);
        // After returning to the dashboard, the product owns A's content.
        // An ordinary deletion must not make a later batch recopy A from legacy.
        std::fs::remove_file(&product).unwrap();let ledger=MaterializationStore::open(&db).unwrap();
        assert!(ledger.all_terminal(std::slice::from_ref(&a)).unwrap());
        assert!(!ledger.all_terminal(std::slice::from_ref(&b)).unwrap());
        assert!(!ledger.all_terminal(&[a.clone(),b]).unwrap());
        // Empty authenticated streams have no record obligation. The root plan
        // must still resolve their employee, but must not demand a record receipt.
        a.source_id="empty".into();a.records=0;drop(ledger);
        let mut ledger=MaterializationStore::open(&db).unwrap();ledger.register(&a,&[]).unwrap();
        assert!(ledger.all_terminal(&[a]).unwrap());
    }
}
