//! Allocation journal and request handling. Portable tests use an in-memory
//! registry ([`FakeRegistry`]) and injected account/storage hosts; the
//! `iso_root_` tests run the real root-owned `RegistryStore` under `/run`.
use super::*;
use duduclaw_isolation::{ids::{RequestId,Sha256Digest,UidRange,EMPLOYEE_UID_MIN,EMPLOYEE_UID_MAX},migration::*,execution::RunHandle,bounded::Text,capability::EmployeeBinding};
use crate::{registry::HostUidPolicy,storage::{StorageUsage,AttachedStorage,Attachment}};
use sha2::{Digest,Sha256};

pub(super) struct Accounts{pub(super) create_fail:bool,pub(super) created:usize,pub(super) recovered:usize,pub(super) recover_fail:usize}
impl Accounts{pub(super) fn new()->Self{Self{create_fail:false,created:0,recovered:0,recover_fail:0}}}
impl AccountHost for Accounts{
    fn policy(&mut self)->io::Result<HostUidPolicy>{Ok(HostUidPolicy{system:UidRange{first:100,last:999},login:UidRange{first:1000,last:60000},subuids:vec![],subgids:vec![],systemd:vec![]})}
    fn verify_namespace(&mut self,_:&Registry)->io::Result<()>{Ok(())}
    fn uid_available(&mut self,_:u32)->io::Result<bool>{Ok(true)}
    fn create_locked(&mut self,_:&EmployeeAccount)->io::Result<()>{self.created+=1;if self.create_fail{Err(io::Error::other("injected useradd failure"))}else{Ok(())}}
    fn verify_locked(&mut self,_:&EmployeeAccount)->io::Result<()>{Ok(())}
    fn recover_locked(&mut self,_:&EmployeeAccount)->io::Result<()>{
        self.recovered+=1;
        if self.recover_fail>0{self.recover_fail-=1;return Err(io::Error::other("injected recovery failure"))}
        Ok(())
    }
}
/// Storage double: images and quotas per employee, injected failures.
pub(super) struct Disk{pub(super) fail_ready:bool,pub(super) images:Vec<(EmployeeId,DiskQuota)>,pub(super) provisions:usize,pub(super) destroys:usize,
    pub(super) destroy_fail:usize,pub(super) leases:Vec<Attachment>,pub(super) traces_fail:bool,pub(super) extra_traces:Vec<EmployeeId>,
    pub(super) provision_fail:bool}
impl Disk{pub(super) fn new()->Self{Self{fail_ready:false,images:vec![],provisions:0,destroys:0,destroy_fail:0,leases:vec![],traces_fail:false,
    extra_traces:vec![],provision_fail:false}}
    pub(super) fn present(&self,e:EmployeeId)->bool{self.images.iter().any(|(i,_)|*i==e)}}
impl Storage for Disk{
    fn readiness(&mut self)->Result<()>{if self.fail_ready{Err(unavailable())}else{Ok(())}}
    fn provision(&mut self,r:&ProvisionRequest)->Result<StorageUsage>{
        if self.provision_fail{return Err(unavailable())}
        self.provisions+=1;
        if !self.present(r.employee){self.images.push((r.employee,r.quota.clone()));}
        self.usage(r.employee)
    }
    fn usage(&mut self,e:EmployeeId)->Result<StorageUsage>{
        let (_,q)=self.images.iter().find(|(i,_)|*i==e).ok_or_else(unavailable)?;let (bytes,inodes)=crate::storage::quota_limits(q);
        Ok(StorageUsage{employee:e,bytes:4096,inodes:3,limit_bytes:bytes,limit_inodes:inodes,hard_quota_verified:true,device_graph_verified:true,host_reserve_verified:true})
    }
    fn destroy(&mut self,e:EmployeeId,_:RequestId,_:Sha256Digest)->Result<()>{
        if self.destroy_fail>0{self.destroy_fail-=1;return Err(unavailable())}
        if self.leases.iter().any(|a|a.employee==e){return Err(ErrorCode::InvalidRequest)}
        self.images.retain(|(i,_)|*i!=e);self.destroys+=1;Ok(())
    }
    fn attachments(&mut self)->Result<Vec<Attachment>>{Ok(self.leases.clone())}
    fn attach(&mut self,_:EmployeeId,_:u32,_:RunHandle,_:&DiskQuota)->Result<AttachedStorage>{Err(unavailable())}
    fn detach(&mut self,_:&Attachment)->Result<()>{Err(unavailable())}
    fn traces(&mut self)->Result<Vec<EmployeeId>>{
        if self.traces_fail{return Err(unavailable())}
        Ok(self.images.iter().map(|(e,_)|*e).chain(self.extra_traces.iter().copied()).collect())
    }
}
/// In-memory registry with the root store's rules (UIDs never reused).
pub(super) struct FakeRegistry(pub(super) Registry);
impl RegistryOps for FakeRegistry{
    fn registry(&self)->&Registry{&self.0}
    fn allocate(&mut self,employee:EmployeeId,host:&mut dyn AccountHost)->io::Result<EmployeeAccount>{
        if let Some(existing)=self.0.employees.iter().find(|a|a.employee==employee){
            return if existing.state==AccountState::Ready{Ok(existing.clone())}else{Err(io::Error::other("not ready"))};
        }
        let uid=(EMPLOYEE_UID_MIN..=EMPLOYEE_UID_MAX).find(|uid|!self.0.employees.iter().any(|a|a.uid==*uid)).ok_or_else(||io::Error::other("full"))?;
        let mut account=EmployeeAccount{employee,uid,gid:uid,state:AccountState::Reserved};
        let created=host.create_locked(&account).is_ok();
        account.state=if created{AccountState::Ready}else{AccountState::Tombstone};
        self.0.employees.push(account.clone());
        if created{Ok(account)}else{Err(io::Error::other("create"))}
    }
    fn retire(&mut self,employee:EmployeeId)->io::Result<()>{
        let account=self.0.employees.iter_mut().find(|a|a.employee==employee).ok_or_else(||io::Error::other("missing"))?;
        account.state=AccountState::Tombstone;Ok(())
    }
}
pub(super) fn quiet()->EmployeeIdleReport{EmployeeIdleReport{runs:0,attachments:0,processes:0,units:0}}
/// One allocator + registry + hosts, portable (tempdir) or root (`/run`).
pub(super) struct Fx{pub(super) dir:PathBuf,pub(super) registry:Box<dyn RegistryOps>,pub(super) accounts:Accounts,pub(super) disk:Disk,
    pub(super) allocator:Allocator,pub(super) root:bool,_temp:Option<tempfile::TempDir>}
impl Fx{
    pub(super) fn portable()->Self{
        let temp=tempfile::tempdir().unwrap();let dir=temp.path().to_path_buf();
        let allocator=Allocator{directory:dir.clone(),journal:Journal{schema:JOURNAL_SCHEMA,entries:vec![]},poisoned:false,events:vec![]};
        Self{dir,registry:Box::new(FakeRegistry(Registry::default())),accounts:Accounts::new(),disk:Disk::new(),allocator,root:false,_temp:Some(temp)}
    }
    pub(super) fn root(name:&str)->Self{
        use std::os::unix::fs::DirBuilderExt;
        assert_eq!(unsafe{libc::geteuid()},0,"iso_root_ tests run as root in a disposable VM");
        let dir=PathBuf::from(format!("/run/iso-retire-test-{}-{name}",std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let registry=RegistryStore::open_at(&dir).unwrap();let mut disk=Disk::new();
        let allocator=Allocator::open_at(&dir,RegistryOps::registry(&registry),&mut disk).unwrap();
        Self{dir,registry:Box::new(registry),accounts:Accounts::new(),disk,allocator,root:true,_temp:None}
    }
    pub(super) fn handle(&mut self,request:&Request)->Answer<Reply>{self.handle_with(request,false,None)}
    /// `idle: None` = a quiet readback for the request's subject, as the host would read it.
    pub(super) fn handle_with(&mut self,request:&Request,host_busy:bool,idle:Option<Result<EmployeeIdleReport>>)->Answer<Reply>{
        let idle=idle.or_else(||self.allocator.subject(request).map(|_|Ok(quiet())));
        self.allocator.handle(request,Env{registry:self.registry.as_mut(),accounts:&mut self.accounts,storage:&mut self.disk,host_busy,idle,now_unix:1_000})
    }
    /// A daemon restart of the allocator (the journal is re-read from disk).
    pub(super) fn reopen(&mut self){
        self.allocator=if self.root{Allocator::open_at(&self.dir,self.registry.registry(),&mut self.disk).unwrap()}
            else{Allocator{directory:self.dir.clone(),journal:decode_journal(&fs::read(self.dir.join("allocations.json")).unwrap()).unwrap(),poisoned:false,events:vec![]}};
    }
    pub(super) fn phase(&self,employee:EmployeeId)->Phase{self.allocator.journal.entries.iter().find(|e|e.receipt.employee==employee).unwrap().phase}
    pub(super) fn account(&self,employee:EmployeeId)->EmployeeAccount{self.registry.registry().employees.iter().find(|a|a.employee==employee).unwrap().clone()}
    pub(super) fn finish(self){if self.root{let dir=self.dir.clone();drop(self);fs::remove_dir_all(dir).unwrap();}}
}
pub(super) const ALICE:&[(&str,&str)]=&[("binding_kind","personal"),("owner_id","alice-uuid")];
const BOB:&[(&str,&str)]=&[("binding_kind","personal"),("owner_id","bob-uuid")];
pub(super) const TEAM:&[(&str,&str)]=&[("binding_kind","shared"),("tenant_id","tenant-a")];
fn personal(owner:&str)->EmployeeBinding{EmployeeBinding::Personal(Text::new(owner.into()).unwrap())}
fn digest_of(category:ConfirmationCategory,fields:&[ProposalField])->Sha256Digest{
    let mut hash=Sha256::new();hash.update(b"duduclaw-migration-proposal-v1\0");hash.update(serde_json::to_vec(&(category,fields)).unwrap());
    Sha256Digest(hash.finalize().into())
}
/// An Employee proposal for `label` with proposal id `[id;16]`.
pub(super) fn employee_request(id:u8,label:&str,status:ConfirmationStatus,extra:&[(&str,&str)])->Request{
    let mut fields:Vec<ProposalField>=[("disk_bytes","67108864"),("employee_id",label)].iter().chain(extra)
        .map(|(n,v)|ProposalField{name:(*n).into(),value:ProposalValue::Text{value:(*v).into()}}).collect();
    fields.sort_by(|a,b|a.name.cmp(&b.name));
    Request{schema:1,nonce:RequestId([2;16]),action:Action::Allocate,approver:None,item:ConfirmationItem{binding:ProposalBinding{operation_id:ProposalOperationId{proposal_id:RequestId([id;16]),revision:1},
        proposal_digest:digest_of(ConfirmationCategory::Employee,&fields)},category:ConfirmationCategory::Employee,status,high_risk:true,fields}}
}
fn request_with(status:ConfirmationStatus,extra:&[(&str,&str)])->Request{employee_request(1,"sales",status,extra)}
/// Allocate `label` and return the Ready receipt.
pub(super) fn allocate(fx:&mut Fx,id:u8,label:&str)->Receipt{
    let request=employee_request(id,label,ConfirmationStatus::Applying,ALICE);
    let reply=fx.handle(&request).unwrap();assert_eq!(reply.phase,Phase::Ready);reply.receipt
}
/// The retire proposal (id `[id;16]`) for an allocated receipt.
pub(super) fn retire_spec(receipt:&Receipt)->RetireSpec{
    RetireSpec{label:receipt.label.clone(),target:receipt.binding,employee:receipt.employee,uid:receipt.uid,early:true,
        quarantine_digest_prefix:"00112233445566ff".into(),requested_by:"admin-a".into()}
}
pub(super) fn retire_request_for(spec:&RetireSpec,id:u8)->Request{
    let fields=wire::retire_fields(spec);
    Request{schema:wire::SCHEMA_V2,nonce:RequestId([2;16]),action:Action::Retire,approver:Some("admin-b".into()),
        item:ConfirmationItem{binding:ProposalBinding{operation_id:ProposalOperationId{proposal_id:RequestId([id;16]),revision:1},
            proposal_digest:digest_of(ConfirmationCategory::EmployeeRetire,&fields)},category:ConfirmationCategory::EmployeeRetire,
            status:ConfirmationStatus::Applying,high_risk:true,fields}}
}
pub(super) fn retire_request(receipt:&Receipt,id:u8)->Request{retire_request_for(&retire_spec(receipt),id)}
fn entry(request:&Request,phase:Phase)->Entry{
    let spec=request.validate().unwrap();
    let (retire,retired_at)=match phase{Phase::Retiring=>(Some(retire_binding(0x40)),None),Phase::Retired=>(Some(retire_binding(0x40)),Some(5)),_=>(None,None)};
    Entry{receipt:Receipt{binding:request.item.binding,label:spec.label,employee:EmployeeId([7;16]),uid:70001,bytes:spec.bytes,inodes:spec.inodes,
        disabled:matches!(phase,Phase::Disabled|Phase::Retired),principal_binding:spec.binding},phase,retire,retired_at}
}
fn retire_binding(id:u8)->ProposalBinding{ProposalBinding{operation_id:ProposalOperationId{proposal_id:RequestId([id;16]),revision:1},proposal_digest:Sha256Digest([id;32])}}
fn saved(journal:Journal)->Journal{
    let dir=tempfile::tempdir().unwrap();
    let mut allocator=Allocator{directory:dir.path().into(),journal,poisoned:false,events:vec![]};
    allocator.save().unwrap();
    decode_journal(&fs::read(dir.path().join("allocations.json")).unwrap()).unwrap()
}
#[test]
fn allocation_receipt_schema2_pins_personal_owner(){
    let req=request_with(ConfirmationStatus::Applying,ALICE);
    assert_eq!(req.validate().unwrap().binding,Some(personal("alice-uuid")));
    let journal=saved(Journal{schema:2,entries:vec![entry(&req,Phase::Ready)]});
    assert_eq!(journal.schema,JOURNAL_SCHEMA);
    assert_eq!(journal.entries[0].receipt.principal_binding,Some(personal("alice-uuid")));
    let bytes=serde_json::to_vec(&journal).unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains(r#""principal_binding":{"kind":"personal","id":"alice-uuid"}"#));
    let mut readback=req.clone();readback.action=Action::ReadBack;
    assert_eq!(replay_index(&journal,&readback.item.binding,&readback.validate().unwrap()).unwrap(),Some(0));
    let forged=String::from_utf8(bytes).unwrap().replace(r#""id":"alice-uuid""#,r#""id":"""#);
    assert!(decode_journal(forged.as_bytes()).is_err());
}
#[test]
fn allocation_schema1_journal_reads_as_unbound_legacy(){
    let legacy=request_with(ConfirmationStatus::Applying,&[]);
    let v180=Journal{schema:1,entries:vec![entry(&legacy,Phase::Ready)]};
    let encoded=serde_json::to_string(&v180).unwrap();
    assert!(!encoded.contains("principal_binding") && !encoded.contains("retire"),"byte-compatible with the v1.80 journal: {encoded}");
    let journal=decode_journal(encoded.as_bytes()).unwrap();
    assert_eq!(journal.schema,1);assert_eq!(journal.entries[0].receipt.principal_binding,None);
    assert_eq!(replay_index(&journal,&legacy.item.binding,&legacy.validate().unwrap()).unwrap(),Some(0));
    let rewritten=saved(journal);
    assert_eq!(rewritten.schema,JOURNAL_SCHEMA);assert_eq!(rewritten.entries[0].receipt.principal_binding,None);
    let mut forged=v180.clone();forged.entries[0].receipt.principal_binding=Some(personal("alice-uuid"));
    assert!(decode_journal(&serde_json::to_vec(&forged).unwrap()).is_err());
    for schema in [0u16,4,u16::MAX]{
        let mut other=v180.clone();other.schema=schema;
        assert!(decode_journal(&serde_json::to_vec(&other).unwrap()).is_err(),"schema {schema}");
    }
    assert!(decode_journal(encoded.replace(r#""disabled":false"#,r#""disabled":false,"owner":"alice""#).as_bytes()).is_err());
}
#[test]
fn allocation_same_operation_binding_change_rejected(){
    let alice=request_with(ConfirmationStatus::Applying,ALICE);
    let journal=Journal{schema:JOURNAL_SCHEMA,entries:vec![entry(&alice,Phase::Ready)]};
    for extra in [BOB,TEAM,&[][..]]{
        let changed=request_with(ConfirmationStatus::Applying,extra);
        assert!(matches!(replay_index(&journal,&changed.item.binding,&changed.validate().unwrap()),Err(ErrorCode::InvalidRequest)));
    }
    let spec=alice.validate().unwrap();
    for other in [Some(personal("bob-uuid")),Some(EmployeeBinding::Shared(Text::new("alice-uuid".into()).unwrap())),None]{
        let mut tampered=journal.clone();tampered.entries[0].receipt.principal_binding=other;
        assert!(matches!(replay_index(&tampered,&alice.item.binding,&spec),Err(ErrorCode::InvalidRequest)));
    }
    let legacy=request_with(ConfirmationStatus::Applying,&[]);
    let unbound=Journal{schema:1,entries:vec![entry(&legacy,Phase::Ready)]};
    let mut upgrade=alice.clone();upgrade.item.binding.proposal_digest=legacy.item.binding.proposal_digest;
    assert!(matches!(replay_index(&unbound,&upgrade.item.binding,&spec),Err(ErrorCode::InvalidRequest)));
    let disabled=Journal{schema:JOURNAL_SCHEMA,entries:vec![entry(&alice,Phase::Disabled)]};
    let bob=request_with(ConfirmationStatus::Applying,BOB);
    assert!(matches!(replay_index(&disabled,&bob.item.binding,&bob.validate().unwrap()),Err(ErrorCode::InvalidRequest)));
    assert_eq!(saved(disabled).entries[0].receipt.principal_binding,Some(personal("alice-uuid")));
}

// ---- F6l unit tests (F6 §6 row) ----

#[test]
fn journal_schema3_reads_schema1_and_2(){
    let alice=request_with(ConfirmationStatus::Applying,ALICE);
    let legacy=request_with(ConfirmationStatus::Applying,&[]);
    // Schema 2 (v1.81 pre-F6l) and schema 1 (v1.80) files read and are rewritten as 3.
    for (schema,request) in [(2u16,&alice),(1,&legacy)]{
        let old=Journal{schema,entries:vec![entry(request,Phase::Ready),{
            let mut dead=entry(&employee_request(5,"old",ConfirmationStatus::Applying,if schema==1{&[][..]}else{ALICE}),Phase::Disabled);
            dead.receipt.employee=EmployeeId([8;16]);dead}]};
        let encoded=serde_json::to_vec(&old).unwrap();
        let read=decode_journal(&encoded).unwrap();
        assert_eq!(read.schema,schema);
        let written=saved(read);
        assert_eq!(written.schema,3);
        assert_eq!(written.entries.len(),2);
        assert!(written.entries.iter().all(|e|e.retire.is_none() && e.retired_at.is_none()));
        let text=serde_json::to_string(&written).unwrap();
        assert!(!text.contains("retire"),"retire fields are omitted when None: {text}");
        // An older schema never carries the retire phases or fields (forged).
        let mut forged=old.clone();forged.entries[0]=entry(request,Phase::Retiring);
        assert!(decode_journal(&serde_json::to_vec(&forged).unwrap()).is_err(),"schema {schema} Retiring");
    }
    // Schema 3 round-trips Retiring and Retired.
    let journal=Journal{schema:3,entries:vec![entry(&alice,Phase::Retired)]};
    let back=saved(journal);
    assert_eq!((back.entries[0].phase,back.entries[0].retire,back.entries[0].retired_at),(Phase::Retired,Some(retire_binding(0x40)),Some(5)));
    // Phase/field shape is enforced.
    let mut no_binding=entry(&alice,Phase::Retiring);no_binding.retire=None;
    let mut live_retired=entry(&alice,Phase::Retired);live_retired.receipt.disabled=false;
    let mut stray=entry(&alice,Phase::Ready);stray.retired_at=Some(1);
    for bad in [no_binding,live_retired,stray]{
        assert!(decode_journal(&serde_json::to_vec(&Journal{schema:3,entries:vec![bad]}).unwrap()).is_err());
    }
    // A Retired entry no longer holds the label; a Retiring one still does.
    let mut next=entry(&employee_request(6,"sales",ConfirmationStatus::Applying,ALICE),Phase::Ready);next.receipt.employee=EmployeeId([9;16]);
    assert!(decode_journal(&serde_json::to_vec(&Journal{schema:3,entries:vec![entry(&alice,Phase::Retired),next.clone()]}).unwrap()).is_ok());
    assert!(decode_journal(&serde_json::to_vec(&Journal{schema:3,entries:vec![entry(&alice,Phase::Retiring),next]}).unwrap()).is_err());
}
#[test]
fn retire_never_creates_entry(){
    let mut fx=Fx::portable();
    let template=Receipt{binding:request_with(ConfirmationStatus::Applied,ALICE).item.binding,label:"sales".into(),employee:EmployeeId([7;16]),
        uid:70001,bytes:67108864,inodes:4096,disabled:false,principal_binding:None};
    let request=retire_request(&template,0x41);
    // The Employee (entry-creating) validation never accepts a retire.
    assert!(request.validate().is_err());
    assert_eq!(fx.handle(&request).unwrap_err(),wire::ErrorCode::NotFound,"no trace anywhere: authoritative absence");
    assert!(fx.allocator.journal.entries.is_empty(),"retire never creates an entry");
    assert!(!fx.dir.join("allocations.json").exists(),"nothing was saved");
    // A retire disguised as an Allocate/ReadBack/Disable is invalid before any lookup.
    for action in [Action::Allocate,Action::ReadBack,Action::Disable]{
        let mut disguised=request.clone();disguised.action=action;disguised.approver=None;
        assert_eq!(fx.handle(&disguised).unwrap_err(),wire::ErrorCode::InvalidRequest);
    }
    assert!(fx.allocator.journal.entries.is_empty());
    assert_eq!((fx.accounts.created,fx.disk.provisions),(0,0));
}
#[test]
fn decode_rejects_retire_binding_collision(){
    let alice=request_with(ConfirmationStatus::Applying,ALICE);
    let mut first=entry(&alice,Phase::Retiring);
    let mut second=entry(&employee_request(6,"other",ConfirmationStatus::Applying,ALICE),Phase::Ready);second.receipt.employee=EmployeeId([9;16]);
    let ok=Journal{schema:3,entries:vec![first.clone(),second.clone()]};
    assert!(decode_journal(&serde_json::to_vec(&ok).unwrap()).is_ok());
    // Retire binding equal to another entry's allocation binding.
    first.retire=Some(second.receipt.binding);
    assert!(decode_journal(&serde_json::to_vec(&Journal{schema:3,entries:vec![first.clone(),second.clone()]}).unwrap()).is_err());
    // … or to its own allocation binding.
    first.retire=Some(first.receipt.binding);
    assert!(decode_journal(&serde_json::to_vec(&Journal{schema:3,entries:vec![first.clone(),second.clone()]}).unwrap()).is_err());
    // … or to another entry's retire binding.
    first.retire=Some(retire_binding(0x40));
    second.phase=Phase::Retired;second.receipt.disabled=true;second.retire=Some(retire_binding(0x40));second.retired_at=Some(9);
    assert!(decode_journal(&serde_json::to_vec(&Journal{schema:3,entries:vec![first,second]}).unwrap()).is_err());
}
#[test]
fn disable_refused_on_retiring_or_retired(){
    assert!(disable_allowed(Phase::Planned) && disable_allowed(Phase::Ready) && disable_allowed(Phase::Disabling));
    assert!(!disable_allowed(Phase::Retiring) && !disable_allowed(Phase::Retired));
    let mut fx=Fx::portable();
    let receipt=allocate(&mut fx,1,"sales");
    // A Retiring entry (root failed after step 5) refuses compensation.
    fx.accounts.recover_fail=1;
    assert_eq!(fx.handle(&retire_request(&receipt,0x41)).unwrap_err(),wire::ErrorCode::IsolationUnavailable);
    assert_eq!(fx.phase(receipt.employee),Phase::Retiring);
    let mut disable=employee_request(1,"sales",ConfirmationStatus::Failed,ALICE);disable.action=Action::Disable;
    assert_eq!(fx.handle(&disable).unwrap_err(),wire::ErrorCode::InvalidRequest);
    assert_eq!(fx.phase(receipt.employee),Phase::Retiring,"never Disabling");
    fx.handle(&retire_request(&receipt,0x41)).unwrap();
    assert_eq!(fx.handle(&disable).unwrap_err(),wire::ErrorCode::InvalidRequest);
    assert_eq!(fx.phase(receipt.employee),Phase::Retired);
}
#[test]
fn readback_retired_reports_phase(){
    let mut fx=Fx::portable();
    let receipt=allocate(&mut fx,1,"sales");
    let mut readback=employee_request(1,"sales",ConfirmationStatus::Applied,ALICE);readback.action=Action::ReadBack;readback.schema=wire::SCHEMA_V2;
    assert_eq!(fx.handle(&readback).unwrap().phase,Phase::Ready);
    fx.handle(&retire_request(&receipt,0x41)).unwrap();
    let reply=fx.handle(&readback).unwrap();
    assert_eq!((reply.phase,reply.receipt.disabled,reply.idle),(Phase::Retired,true,None));
    assert_eq!(reply.receipt.employee,receipt.employee);
    // Schema 1 keeps the Ready-only semantics.
    readback.schema=1;
    assert_eq!(fx.handle(&readback).unwrap_err(),wire::ErrorCode::IsolationUnavailable);
    // The wire reply carries the phase.
    readback.schema=wire::SCHEMA_V2;
    let response=wire::Response::for_request(&readback,fx.handle(&readback));
    assert_eq!(response.phase,Some(Phase::Retired));
    assert!(matches!(wire::check_response(&readback,response).unwrap(),wire::Answer::Reply(r) if r.phase==Phase::Retired));
}
#[test]
fn allocation_readback_reports_idle(){
    let mut fx=Fx::portable();
    let receipt=allocate(&mut fx,1,"sales");
    let mut readback=employee_request(1,"sales",ConfirmationStatus::Applied,ALICE);readback.action=Action::ReadBack;readback.schema=wire::SCHEMA_V2;
    assert_eq!(fx.allocator.subject(&readback),Some(receipt.employee),"the host reads idle for a Ready subject");
    assert_eq!(fx.handle_with(&readback,true,Some(Ok(quiet()))).unwrap().idle,Some(true),"ReadBack never waits for the host gate");
    for busy in [EmployeeIdleReport{runs:1,..quiet()},EmployeeIdleReport{attachments:1,..quiet()},EmployeeIdleReport{processes:1,..quiet()},EmployeeIdleReport{units:1,..quiet()}]{
        assert_eq!(fx.handle_with(&readback,false,Some(Ok(busy))).unwrap().idle,Some(false));
    }
    assert_eq!(fx.handle_with(&readback,false,Some(Err(unavailable()))).unwrap().idle,None,"unreadable is unknown, never idle");
    readback.schema=1;
    assert_eq!(fx.allocator.subject(&readback),None,"schema 1 never pays for the idle readback");
    assert_eq!(fx.handle_with(&readback,false,Some(Ok(quiet()))).unwrap().idle,None);
}
#[test]
fn retire_scenarios_with_in_memory_registry(){
    for (name,scenario) in super::retire_tests::SCENARIOS{
        let mut fx=Fx::portable();
        scenario(&mut fx);
        eprintln!("retire scenario passed: {name}");
    }
}
#[test]
#[ignore="root Linux /run filesystem fixture; account and storage effects are injected, not live deployment acceptance"]
fn iso_root_e2e_confirmed_allocation_binding_restart_and_compensation(){
    for failure in [0,1,2]{
        let mut fx=Fx::root(&format!("e2e-{failure}"));
        fx.accounts.create_fail=failure==2;fx.disk.fail_ready=failure==1;
        let mut req=request_with(ConfirmationStatus::Pending,ALICE);
        assert!(fx.handle(&req).is_err());
        assert_eq!(fx.accounts.created,0);assert_eq!(fx.disk.provisions,0);assert!(!fx.dir.join("allocations.json").exists());
        req.item.status=ConfirmationStatus::Applying;
        let first=fx.handle(&req);
        if failure==0{
            let first=first.unwrap().receipt;assert_eq!(first.principal_binding,Some(personal("alice-uuid")));
            fx.reopen();
            assert_eq!(fx.allocator.journal.schema,JOURNAL_SCHEMA);
            let second=fx.handle(&req).unwrap().receipt;
            assert_eq!(first.employee,second.employee);assert_eq!(first.uid,second.uid);assert_eq!(fx.accounts.created,1);
            assert_eq!(second.principal_binding,Some(personal("alice-uuid")),"Personal owner survives restart");
            let mut readback=req.clone();readback.action=Action::ReadBack;readback.item.status=ConfirmationStatus::Applied;
            let read=fx.handle(&readback).unwrap().receipt;
            assert_eq!(read.employee,first.employee);assert_eq!(read.principal_binding,Some(personal("alice-uuid")));
            let mut changed=req.clone();changed.item.binding.proposal_digest.0[0]^=1;
            assert!(fx.handle(&changed).is_err());
            let mut rebound=request_with(ConfirmationStatus::Applying,BOB);rebound.item.binding.operation_id=req.item.binding.operation_id;
            assert_eq!(fx.handle(&rebound).unwrap_err(),wire::ErrorCode::InvalidRequest);
            assert_eq!(fx.accounts.created,1);
        }else{assert!(first.is_err());assert_eq!(fx.disk.provisions,0);}
        req.action=Action::Disable;req.item.status=ConfirmationStatus::Failed;
        let disabled=fx.handle(&req).unwrap().receipt;assert!(disabled.disabled);assert!(fx.disk.images.is_empty());
        assert_eq!(disabled.principal_binding,Some(personal("alice-uuid")));
        if failure==1{assert_eq!(disabled.uid,0);assert_eq!(fx.accounts.created,0);}
        if failure==2{assert!(fx.accounts.recovered>0);assert_eq!(fx.registry.registry().employees[0].state,AccountState::Tombstone);}
        req.action=Action::Allocate;req.item.status=ConfirmationStatus::Applying;
        assert!(fx.handle(&req).is_err(),"same operation never revives tombstone");
        req=request_with(ConfirmationStatus::Applying,TEAM);req.item.binding.operation_id.proposal_id=RequestId([3;16]);
        fx.accounts.create_fail=false;fx.disk.fail_ready=false;
        let replacement=fx.handle(&req).unwrap().receipt;
        assert_ne!(replacement.employee,disabled.employee);
        assert_eq!(replacement.principal_binding,Some(EmployeeBinding::Shared(Text::new("tenant-a".into()).unwrap())));
        if failure!=1{assert_eq!(replacement.uid,70001,"prior UID is never recycled");}
        fx.reopen();
        assert!(fx.allocator.journal.entries.iter().any(|e|e.receipt.employee==disabled.employee && e.phase==Phase::Disabled));
        req.action=Action::Disable;req.item.status=ConfirmationStatus::Failed;
        fx.handle(&req).unwrap();
        fx.finish();
    }
}
