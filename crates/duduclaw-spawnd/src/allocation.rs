//! Confirmed employee allocation owned by the same lifecycle worker as B1/H.
use std::{fs::{self,File,OpenOptions},io::{self,Read,Write},path::{Path,PathBuf},os::unix::fs::{OpenOptionsExt,MetadataExt}};
use serde::{Serialize,Deserialize};
use duduclaw_migration::allocation::{Action,Request,Receipt,Specification,operation,validate_principal_binding};
use duduclaw_isolation::{ids::EmployeeId,execution::DiskQuota,ipc::ErrorCode,migration::ProposalBinding};
use crate::{Result,registry::{RegistryStore,AccountHost,AccountState,root_path,REGISTRY_DIRECTORY},storage::{Storage,ProvisionRequest}};
fn unavailable()->ErrorCode{ErrorCode::IsolationUnavailable}
/// Schema 1 (v1.80) predates principal bindings; schema 2 adds the optional
/// immutable `principal_binding` per receipt. Both are read; only 2 is written.
const JOURNAL_SCHEMA:u16=2;
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry { receipt:Receipt, phase:Phase }
#[derive(Clone,Copy,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
enum Phase{Planned,Ready,Disabling,Disabled}
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal{schema:u16,entries:Vec<Entry>}
pub struct Allocator{directory:PathBuf,journal:Journal,poisoned:bool}
impl Allocator {
    pub fn open()->io::Result<Self>{Self::open_at(Path::new(REGISTRY_DIRECTORY))}
    fn open_at(directory:&Path)->io::Result<Self>{
        root_path(directory,true)?;let path=directory.join("allocations.json");
        let journal=match fs::symlink_metadata(&path){
            Ok(m)=>{
                root_path(&path,false)?;if m.mode()&0o077!=0{return Err(io::Error::other("allocation journal unavailable"))}
                let mut bytes=vec![];File::open(&path)?.take(4*1024*1024+1).read_to_end(&mut bytes)?;
                if bytes.len()>4*1024*1024{return Err(io::Error::other("allocation journal unavailable"))}
                decode_journal(&bytes)?
            },
            Err(e) if e.kind()==io::ErrorKind::NotFound=>Journal{schema:JOURNAL_SCHEMA,entries:vec![]},Err(e)=>return Err(e),
        };
        Ok(Self{directory:directory.into(),journal,poisoned:false})
    }
    fn save(&mut self)->Result<()>{
        if self.poisoned{return Err(unavailable())}
        // A loaded schema-1 journal is rewritten as schema 2; its unbound
        // legacy entries keep `principal_binding: None` and are never upgraded.
        self.journal.schema=JOURNAL_SCHEMA;
        let result=(||->io::Result<()>{
            let tmp=self.directory.join("allocations.next");
            match fs::symlink_metadata(&tmp){Ok(_)=>{root_path(&tmp,false)?;fs::remove_file(&tmp)?;},Err(e) if e.kind()==io::ErrorKind::NotFound=>{},Err(e)=>return Err(e)}
            let mut f=OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
            f.write_all(&serde_json::to_vec(&self.journal).map_err(io::Error::other)?)?;f.sync_all()?;
            fs::rename(tmp,self.directory.join("allocations.json"))?;File::open(&self.directory)?.sync_all()
        })();
        if result.is_err(){self.poisoned=true;}result.map_err(|_|unavailable())
    }
    /// H4: employees whose journal entry is `Ready` and not disabled (the
    /// allocations an anchor backfill and the `WorkAnchor` op may serve).
    pub fn ready_employees(&self)->Vec<EmployeeId>{
        self.journal.entries.iter().filter(|e|e.phase==Phase::Ready && !e.receipt.disabled).map(|e|e.receipt.employee).collect()
    }
    pub fn handle(&mut self,request:&Request,registry:&mut RegistryStore,accounts:&mut dyn AccountHost,storage:&mut dyn Storage)->Result<Receipt>{
        if self.poisoned{return Err(unavailable())}
        let spec=request.validate().map_err(|_|ErrorCode::InvalidRequest)?;
        let binding=request.item.binding;
        let index=match replay_index(&self.journal,&binding,&spec)?{
            Some(i)=>i,
            None=>{
                if request.action==Action::ReadBack || self.journal.entries.len()>=4096 || self.journal.entries.iter().any(|e|e.receipt.label==spec.label && e.phase!=Phase::Disabled){return Err(ErrorCode::InvalidRequest)}
                let employee=EmployeeId(crate::server::random_id().map_err(|_|unavailable())?);
                if employee.0==[0;16] || self.journal.entries.iter().any(|e|e.receipt.employee==employee){return Err(unavailable())}
                // The principal comes only from this confirmed proposal, never
                // from an earlier (possibly disabled) entry with the same label.
                self.journal.entries.push(Entry{receipt:Receipt{binding,label:spec.label.clone(),employee,uid:0,bytes:spec.bytes,inodes:spec.inodes,disabled:false,principal_binding:spec.binding.clone()},phase:Phase::Planned});
                // The complete binding and random principal survive before UID or disk side effects.
                self.save()?;self.journal.entries.len()-1
            }
        };
        let employee=self.journal.entries[index].receipt.employee;
        let quota=DiskQuota::LoopFilesystem{bytes:spec.bytes,inodes:spec.inodes};
        match request.action{
            Action::Allocate=>{
                if matches!(self.journal.entries[index].phase,Phase::Disabling|Phase::Disabled){return Err(ErrorCode::InvalidRequest)}
                storage.readiness()?;
                let account=registry.allocate(employee,accounts).map_err(|_|unavailable())?;
                self.journal.entries[index].receipt.uid=account.uid;self.save()?;
                storage.provision(&ProvisionRequest{employee,uid:account.uid,operation_id:operation(binding,false),proposal_digest:binding.proposal_digest,quota:quota.clone()})?.check(employee,&quota)?;
                accounts.verify_locked(&account).map_err(|_|unavailable())?;
                storage.usage(employee)?.check(employee,&quota)?;
                self.journal.entries[index].phase=Phase::Ready;self.save()?;
            },
            Action::ReadBack=>{
                if self.journal.entries[index].phase!=Phase::Ready{return Err(unavailable())}
                let account=registry.registry().lookup(employee).map_err(|_|unavailable())?;
                if account.uid!=self.journal.entries[index].receipt.uid{return Err(unavailable())}
                accounts.verify_locked(account).map_err(|_|unavailable())?;
                storage.usage(employee)?.check(employee,&quota)?;
            },
            Action::Disable=>{
                self.journal.entries[index].phase=Phase::Disabling;self.save()?;
                let account=registry.registry().employees.iter().find(|a|a.employee==employee).cloned();
                if let Some(account)=account{
                    registry.retire(employee).map_err(|_|unavailable())?;
                    accounts.recover_locked(&account).map_err(|_|unavailable())?;
                    if registry.registry().employees.iter().any(|a|a.employee==employee && a.state!=AccountState::Tombstone){return Err(unavailable())}
                    storage.destroy(employee,operation(binding,true),binding.proposal_digest)?;
                    if storage.attachments()?.iter().any(|a|a.employee==employee){return Err(unavailable())}
                }
                self.journal.entries[index].receipt.disabled=true;
                self.journal.entries[index].phase=Phase::Disabled;self.save()?;
            },
        }
        Ok(self.journal.entries[index].receipt.clone())
    }
}

fn decode_journal(bytes:&[u8])->io::Result<Journal>{
    let bad=||io::Error::other("allocation journal unavailable");
    let journal=serde_json::from_slice::<Journal>(bytes).map_err(io::Error::other)?;
    if !matches!(journal.schema,1|JOURNAL_SCHEMA) || journal.entries.len()>4096{return Err(bad())}
    for (i,e) in journal.entries.iter().enumerate(){
        if e.receipt.employee.0==[0;16] || journal.entries[..i].iter().any(|p|p.receipt.employee==e.receipt.employee || (p.receipt.label==e.receipt.label && p.phase!=Phase::Disabled && e.phase!=Phase::Disabled) || p.receipt.binding.operation_id==e.receipt.binding.operation_id){return Err(bad())}
        match &e.receipt.principal_binding{
            // Schema 1 was never written with bindings: one there is forged.
            Some(_) if journal.schema==1=>return Err(bad()),
            Some(b)=>validate_principal_binding(b).map_err(|_|bad())?,
            None=>{},
        }
    }
    Ok(journal)
}
/// A replay must match the recorded operation exactly, including the
/// immutable principal: it never re-pins, clears, or upgrades a legacy `None`.
fn replay_index(journal:&Journal,binding:&ProposalBinding,spec:&Specification)->Result<Option<usize>>{
    let Some(i)=journal.entries.iter().position(|e|e.receipt.binding.operation_id==binding.operation_id) else{return Ok(None)};
    let r=&journal.entries[i].receipt;
    if r.binding!=*binding || r.label!=spec.label || r.bytes!=spec.bytes || r.inodes!=spec.inodes || r.principal_binding!=spec.binding{return Err(ErrorCode::InvalidRequest)}
    Ok(Some(i))
}

pub(crate) fn serve(gateway:(u32,u32),sender:std::sync::mpsc::SyncSender<crate::host_worker::AllocationWork>)->io::Result<()> {
    use duduclaw_migration::allocation::{transport,Response};
    let listener=transport::bind(gateway.1).map_err(|_|io::Error::other("allocation socket unavailable"))?;
    std::thread::Builder::new().name("spawnd-allocation".into()).spawn(move||loop{
        let fd=match transport::accept(&listener,gateway){Ok(fd)=>fd,Err(_)=>continue};
        let request:Request=match transport::receive(&fd){Ok(r)=>r,Err(_)=>continue};
        if request.validate().is_err(){continue;}
        let nonce=request.nonce;
        let (reply,receive)=std::sync::mpsc::sync_channel(1);
        let receipt=if sender.try_send((request,reply)).is_ok(){
            receive.recv_timeout(std::time::Duration::from_secs(4)).ok().and_then(Result::ok)
        }else{None};
        let _=transport::send(&fd,&Response{schema:1,nonce,receipt});
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_isolation::{ids::{RequestId,Sha256Digest,UidRange},migration::*,execution::{RunHandle},bounded::Text,capability::EmployeeBinding};
    use crate::{registry::{Registry,HostUidPolicy,EmployeeAccount},storage::{StorageUsage,AttachedStorage,Attachment}};
    use sha2::{Digest,Sha256};
    struct Accounts{create_fail:bool,created:usize,recovered:usize}
    impl AccountHost for Accounts{
        fn policy(&mut self)->io::Result<HostUidPolicy>{Ok(HostUidPolicy{system:UidRange{first:100,last:999},login:UidRange{first:1000,last:60000},subuids:vec![],subgids:vec![],systemd:vec![]})}
        fn verify_namespace(&mut self,_:&Registry)->io::Result<()>{Ok(())}
        fn uid_available(&mut self,_:u32)->io::Result<bool>{Ok(true)}
        fn create_locked(&mut self,_:&EmployeeAccount)->io::Result<()>{self.created+=1;if self.create_fail{Err(io::Error::other("injected useradd failure"))}else{Ok(())}}
        fn verify_locked(&mut self,_:&EmployeeAccount)->io::Result<()>{Ok(())}
        fn recover_locked(&mut self,_:&EmployeeAccount)->io::Result<()>{self.recovered+=1;Ok(())}
    }
    struct Disk{fail_ready:bool,present:bool,provisions:usize,destroys:usize,quota:Option<(EmployeeId,DiskQuota)>}
    impl Storage for Disk{
        fn readiness(&mut self)->Result<()>{if self.fail_ready{Err(unavailable())}else{Ok(())}}
        fn provision(&mut self,r:&ProvisionRequest)->Result<StorageUsage>{self.present=true;self.provisions+=1;self.quota=Some((r.employee,r.quota.clone()));self.usage(r.employee)}
        fn usage(&mut self,e:EmployeeId)->Result<StorageUsage>{let (_,q)=self.quota.as_ref().ok_or_else(unavailable)?;let (bytes,inodes)=crate::storage::quota_limits(q);Ok(StorageUsage{employee:e,bytes:0,inodes:0,limit_bytes:bytes,limit_inodes:inodes,hard_quota_verified:true,device_graph_verified:true,host_reserve_verified:true})}
        fn destroy(&mut self,_:EmployeeId,_:RequestId,_:Sha256Digest)->Result<()>{self.present=false;self.destroys+=1;Ok(())}
        fn attachments(&mut self)->Result<Vec<Attachment>>{Ok(vec![])}
        fn attach(&mut self,_:EmployeeId,_:u32,_:RunHandle,_:&DiskQuota)->Result<AttachedStorage>{Err(unavailable())}
        fn detach(&mut self,_:&Attachment)->Result<()>{Err(unavailable())}
    }
    const ALICE:&[(&str,&str)]=&[("binding_kind","personal"),("owner_id","alice-uuid")];
    const BOB:&[(&str,&str)]=&[("binding_kind","personal"),("owner_id","bob-uuid")];
    const TEAM:&[(&str,&str)]=&[("binding_kind","shared"),("tenant_id","tenant-a")];
    fn personal(owner:&str)->EmployeeBinding{EmployeeBinding::Personal(Text::new(owner.into()).unwrap())}
    fn request_with(status:ConfirmationStatus,extra:&[(&str,&str)])->Request{
        let mut fields:Vec<ProposalField>=[("disk_bytes","67108864"),("employee_id","sales")].iter().chain(extra)
            .map(|(n,v)|ProposalField{name:(*n).into(),value:ProposalValue::Text{value:(*v).into()}}).collect();
        fields.sort_by(|a,b|a.name.cmp(&b.name));
        let mut hash=Sha256::new();hash.update(b"duduclaw-migration-proposal-v1\0");hash.update(serde_json::to_vec(&(ConfirmationCategory::Employee,&fields)).unwrap());
        Request{schema:1,nonce:RequestId([2;16]),action:Action::Allocate,item:ConfirmationItem{binding:ProposalBinding{operation_id:ProposalOperationId{proposal_id:RequestId([1;16]),revision:1},proposal_digest:Sha256Digest(hash.finalize().into())},category:ConfirmationCategory::Employee,status,high_risk:true,fields}}
    }
    fn entry(request:&Request,phase:Phase)->Entry{
        let spec=request.validate().unwrap();
        Entry{receipt:Receipt{binding:request.item.binding,label:spec.label,employee:EmployeeId([7;16]),uid:70001,bytes:spec.bytes,inodes:spec.inodes,disabled:phase==Phase::Disabled,principal_binding:spec.binding},phase}
    }
    fn saved(journal:Journal)->Journal{
        let dir=tempfile::tempdir().unwrap();
        let mut allocator=Allocator{directory:dir.path().into(),journal,poisoned:false};
        allocator.save().unwrap();
        decode_journal(&fs::read(dir.path().join("allocations.json")).unwrap()).unwrap()
    }
    #[test]
    fn allocation_receipt_schema2_pins_personal_owner(){
        let req=request_with(ConfirmationStatus::Applying,ALICE);
        assert_eq!(req.validate().unwrap().binding,Some(personal("alice-uuid")));
        let journal=saved(Journal{schema:JOURNAL_SCHEMA,entries:vec![entry(&req,Phase::Ready)]});
        assert_eq!(journal.schema,2);
        assert_eq!(journal.entries[0].receipt.principal_binding,Some(personal("alice-uuid")));
        let bytes=serde_json::to_vec(&journal).unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains(r#""principal_binding":{"kind":"personal","id":"alice-uuid"}"#));
        // ReadBack/Allocate replay after restart resolves to the same pinned entry.
        let mut readback=req.clone();readback.action=Action::ReadBack;
        assert_eq!(replay_index(&journal,&readback.item.binding,&readback.validate().unwrap()).unwrap(),Some(0));
        // A stored binding that fails validation makes the journal unavailable.
        let forged=String::from_utf8(bytes).unwrap().replace(r#""id":"alice-uuid""#,r#""id":"""#);
        assert!(decode_journal(forged.as_bytes()).is_err());
    }
    #[test]
    fn allocation_schema1_journal_reads_as_unbound_legacy(){
        let legacy=request_with(ConfirmationStatus::Applying,&[]);
        let v180=Journal{schema:1,entries:vec![entry(&legacy,Phase::Ready)]};
        let encoded=serde_json::to_string(&v180).unwrap();
        assert!(!encoded.contains("principal_binding"),"byte-compatible with the v1.80 journal: {encoded}");
        let journal=decode_journal(encoded.as_bytes()).unwrap();
        assert_eq!(journal.schema,1);assert_eq!(journal.entries[0].receipt.principal_binding,None);
        assert_eq!(replay_index(&journal,&legacy.item.binding,&legacy.validate().unwrap()).unwrap(),Some(0));
        // Rewritten as schema 2 without inventing a binding for the legacy entry.
        let rewritten=saved(journal);
        assert_eq!(rewritten.schema,2);assert_eq!(rewritten.entries[0].receipt.principal_binding,None);
        // A schema-1 file can never carry a binding, and unknown schemas fail closed.
        let mut forged=v180.clone();forged.entries[0].receipt.principal_binding=Some(personal("alice-uuid"));
        assert!(decode_journal(&serde_json::to_vec(&forged).unwrap()).is_err());
        for schema in [0u16,3,u16::MAX]{
            let mut other=v180.clone();other.schema=schema;
            assert!(decode_journal(&serde_json::to_vec(&other).unwrap()).is_err(),"schema {schema}");
        }
        assert!(decode_journal(encoded.replace(r#""disabled":false"#,r#""disabled":false,"owner":"alice""#).as_bytes()).is_err());
    }
    #[test]
    fn allocation_same_operation_binding_change_rejected(){
        let alice=request_with(ConfirmationStatus::Applying,ALICE);
        let journal=Journal{schema:JOURNAL_SCHEMA,entries:vec![entry(&alice,Phase::Ready)]};
        // A re-digested proposal with another principal under the same operation.
        for extra in [BOB,TEAM,&[][..]]{
            let changed=request_with(ConfirmationStatus::Applying,extra);
            assert!(matches!(replay_index(&journal,&changed.item.binding,&changed.validate().unwrap()),Err(ErrorCode::InvalidRequest)));
        }
        // Even with an identical proposal binding, a differing recorded principal is refused.
        let spec=alice.validate().unwrap();
        for other in [Some(personal("bob-uuid")),Some(EmployeeBinding::Shared(Text::new("alice-uuid".into()).unwrap())),None]{
            let mut tampered=journal.clone();tampered.entries[0].receipt.principal_binding=other;
            assert!(matches!(replay_index(&tampered,&alice.item.binding,&spec),Err(ErrorCode::InvalidRequest)));
        }
        // A legacy unbound entry is never upgraded by a later bound proposal.
        let legacy=request_with(ConfirmationStatus::Applying,&[]);
        let unbound=Journal{schema:1,entries:vec![entry(&legacy,Phase::Ready)]};
        let mut upgrade=alice.clone();upgrade.item.binding.proposal_digest=legacy.item.binding.proposal_digest;
        assert!(matches!(replay_index(&unbound,&upgrade.item.binding,&spec),Err(ErrorCode::InvalidRequest)));
        // Disabled entries keep their principal; the same operation still matches it only.
        let disabled=Journal{schema:JOURNAL_SCHEMA,entries:vec![entry(&alice,Phase::Disabled)]};
        let bob=request_with(ConfirmationStatus::Applying,BOB);
        assert!(matches!(replay_index(&disabled,&bob.item.binding,&bob.validate().unwrap()),Err(ErrorCode::InvalidRequest)));
        assert_eq!(saved(disabled).entries[0].receipt.principal_binding,Some(personal("alice-uuid")));
    }
    #[test]
    #[ignore="root Linux /run filesystem fixture; account and storage effects are injected, not live deployment acceptance"]
    fn iso_root_e2e_confirmed_allocation_binding_restart_and_compensation(){
        use std::os::unix::fs::DirBuilderExt;
        assert_eq!(unsafe{libc::geteuid()},0);
        for failure in [0,1,2]{
            let path=PathBuf::from(format!("/run/iso-allocation-test-{}-{failure}",std::process::id()));
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            let mut registry=RegistryStore::open_at(&path).unwrap();let mut allocator=Allocator::open_at(&path).unwrap();
            let mut accounts=Accounts{create_fail:failure==2,created:0,recovered:0};
            let mut disk=Disk{fail_ready:failure==1,present:false,provisions:0,destroys:0,quota:None};
            let mut req=request_with(ConfirmationStatus::Pending,ALICE);
            assert!(allocator.handle(&req,&mut registry,&mut accounts,&mut disk).is_err());
            assert_eq!(accounts.created,0);assert_eq!(disk.provisions,0);assert!(!path.join("allocations.json").exists());
            req.item.status=ConfirmationStatus::Applying;
            let first=allocator.handle(&req,&mut registry,&mut accounts,&mut disk);
            if failure==0{
                let first=first.unwrap();assert_eq!(first.principal_binding,Some(personal("alice-uuid")));
                drop(allocator);allocator=Allocator::open_at(&path).unwrap();
                assert_eq!(allocator.journal.schema,2,"journal is written as schema 2");
                let second=allocator.handle(&req,&mut registry,&mut accounts,&mut disk).unwrap();
                assert_eq!(first.employee,second.employee);assert_eq!(first.uid,second.uid);assert_eq!(accounts.created,1);
                assert_eq!(second.principal_binding,Some(personal("alice-uuid")),"Personal owner survives restart");
                let mut readback=req.clone();readback.action=Action::ReadBack;readback.item.status=ConfirmationStatus::Applied;
                let read=allocator.handle(&readback,&mut registry,&mut accounts,&mut disk).unwrap();
                assert_eq!(read.employee,first.employee);assert_eq!(read.principal_binding,Some(personal("alice-uuid")),"ReadBack returns the pinned owner");
                let mut changed=req.clone();changed.item.binding.proposal_digest.0[0]^=1;
                assert!(allocator.handle(&changed,&mut registry,&mut accounts,&mut disk).is_err());
                // Same operation re-proposed for another owner (re-digested) is refused.
                let mut rebound=request_with(ConfirmationStatus::Applying,BOB);rebound.item.binding.operation_id=req.item.binding.operation_id;
                assert!(matches!(allocator.handle(&rebound,&mut registry,&mut accounts,&mut disk),Err(ErrorCode::InvalidRequest)));
                assert_eq!(accounts.created,1);
            }else{assert!(first.is_err());assert_eq!(disk.provisions,0);}
            req.action=Action::Disable;req.item.status=ConfirmationStatus::Failed;
            let disabled=allocator.handle(&req,&mut registry,&mut accounts,&mut disk).unwrap();assert!(disabled.disabled);assert!(!disk.present);
            assert_eq!(disabled.principal_binding,Some(personal("alice-uuid")),"compensation never rewrites the principal");
            if failure==1{assert_eq!(disabled.uid,0);assert_eq!(accounts.created,0);}
            if failure==2{assert!(accounts.recovered>0);assert_eq!(registry.registry().employees[0].state,AccountState::Tombstone);}
            req.action=Action::Allocate;req.item.status=ConfirmationStatus::Applying;
            assert!(allocator.handle(&req,&mut registry,&mut accounts,&mut disk).is_err(),"same operation never revives tombstone");
            // A new proposal for the same label carries its own principal only.
            req=request_with(ConfirmationStatus::Applying,TEAM);
            req.item.binding.operation_id.proposal_id=RequestId([3;16]);
            accounts.create_fail=false;disk.fail_ready=false;
            let replacement=allocator.handle(&req,&mut registry,&mut accounts,&mut disk).unwrap();
            assert_ne!(replacement.employee,disabled.employee);
            assert_eq!(replacement.principal_binding,Some(EmployeeBinding::Shared(Text::new("tenant-a".into()).unwrap())),"old Personal binding is never revived");
            if failure!=1{assert_eq!(replacement.uid,70001,"prior UID is never recycled");}
            drop(allocator);allocator=Allocator::open_at(&path).unwrap();
            assert!(allocator.journal.entries.iter().any(|e|e.receipt.employee==disabled.employee && e.phase==Phase::Disabled && e.receipt.principal_binding==Some(personal("alice-uuid"))));
            req.action=Action::Disable;req.item.status=ConfirmationStatus::Failed;
            allocator.handle(&req,&mut registry,&mut accounts,&mut disk).unwrap();
            drop(allocator);drop(registry);
            for name in ["registry.json","registry.lock","allocations.json"]{fs::remove_file(path.join(name)).unwrap();}fs::remove_dir(path).unwrap();
        }
    }
}
