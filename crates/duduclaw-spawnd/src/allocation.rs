//! Confirmed employee allocation owned by the same lifecycle worker as B1/H.
//!
//! Journal schema 3 (F6l, F6 design r5 §3.12): `Phase` adds `Retiring` and
//! `Retired`, `Entry` adds `retire` (the retire proposal binding) and
//! `retired_at` (both omitted when `None`). Schemas 1 and 2 are still read
//! (they can never carry the new phases or fields); only 3 is written.
//!
//! Retire (`Action::Retire`, schema 2 only) frees a purged employee's root
//! allocation: the registry account goes to `Tombstone` (the UID stays
//! burned: never recycled, §7 N4), the loop image is destroyed and the entry
//! becomes `Retired`, which no longer holds the label. It never creates an
//! entry, is idempotent, and is resumed by the gateway after any failure.
use std::{fs::{self,File,OpenOptions},io::{self,Read,Write},path::{Path,PathBuf},os::unix::fs::{OpenOptionsExt,MetadataExt}};
use serde::{Serialize,Deserialize};
use duduclaw_migration::allocation::{self as wire,Action,Request,Receipt,Reply,Phase,RetireSpec,Specification,OperationKind,operation,validate_principal_binding};
use duduclaw_isolation::{ids::EmployeeId,execution::DiskQuota,ipc::{ErrorCode,EmployeeIdleReport},migration::ProposalBinding};
use crate::{Result,audit::Detail,registry::{Registry,RegistryStore,AccountHost,AccountState,EmployeeAccount,root_path,REGISTRY_DIRECTORY},storage::{Storage,ProvisionRequest}};
fn unavailable()->ErrorCode{ErrorCode::IsolationUnavailable}
/// Allocation answers (schema-2 codes; a schema-1 reply drops them).
type Answer<T>=std::result::Result<T,wire::ErrorCode>;
/// An internal (root component) failure as an allocation answer. A root
/// `NotFound` is never an authoritative absence: only [`absence`] says that.
fn code(error:ErrorCode)->wire::ErrorCode{
    match error{ErrorCode::NotReady=>wire::ErrorCode::NotReady,ErrorCode::InvalidRequest=>wire::ErrorCode::InvalidRequest,_=>wire::ErrorCode::IsolationUnavailable}
}
/// Schema 1 (v1.80) predates principal bindings; schema 2 adds the optional
/// immutable `principal_binding` per receipt; schema 3 adds the retire phases.
const JOURNAL_SCHEMA:u16=3;
/// The per-host lifetime cap (entries are never removed; §7 N4 follow-up:
/// compaction of Retired entries).
const MAX_ENTRIES:usize=4096;
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    receipt:Receipt, phase:Phase,
    #[serde(default,skip_serializing_if="Option::is_none")]
    retire:Option<ProposalBinding>,
    #[serde(default,skip_serializing_if="Option::is_none")]
    retired_at:Option<u64>,
}
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal{schema:u16,entries:Vec<Entry>}
/// The registry operations the allocator uses (the root `RegistryStore` in
/// production; tests may inject an in-memory registry).
pub trait RegistryOps {
    fn registry(&self)->&Registry;
    fn allocate(&mut self,employee:EmployeeId,host:&mut dyn AccountHost)->io::Result<EmployeeAccount>;
    fn retire(&mut self,employee:EmployeeId)->io::Result<()>;
}
impl RegistryOps for RegistryStore {
    fn registry(&self)->&Registry{RegistryStore::registry(self)}
    fn allocate(&mut self,employee:EmployeeId,host:&mut dyn AccountHost)->io::Result<EmployeeAccount>{RegistryStore::allocate(self,employee,host)}
    fn retire(&mut self,employee:EmployeeId)->io::Result<()>{RegistryStore::retire(self,employee)}
}
/// Everything one request may touch, read by the host right before the call.
pub struct Env<'a> {
    pub registry:&'a mut dyn RegistryOps,
    pub accounts:&'a mut dyn AccountHost,
    pub storage:&'a mut dyn Storage,
    /// Some run is live on the host (`host.rs` lifecycle gate, §3.12 step 3).
    pub host_busy:bool,
    /// The fresh `EmployeeIdle` readback of [`Allocator::subject`]; any
    /// unreadable source is an `Err`, never a zero.
    pub idle:Option<Result<EmployeeIdleReport>>,
    pub now_unix:u64,
}
/// Root audit facts produced by one request (the host emits them).
pub enum AllocationEvent {
    Retired{employee:EmployeeId,detail:Detail},
    /// A process of the employee with no run behind it (S14).
    Intruder{employee:EmployeeId},
}
pub struct Allocator{directory:PathBuf,journal:Journal,poisoned:bool,events:Vec<AllocationEvent>}
impl Allocator {
    pub fn open(registry:&Registry,storage:&mut dyn Storage)->io::Result<Self>{Self::open_at(Path::new(REGISTRY_DIRECTORY),registry,storage)}
    fn open_at(directory:&Path,registry:&Registry,storage:&mut dyn Storage)->io::Result<Self>{
        root_path(directory,true)?;let path=directory.join("allocations.json");
        let journal=match fs::symlink_metadata(&path){
            Ok(m)=>{
                root_path(&path,false)?;if m.mode()&0o077!=0{return Err(io::Error::other("allocation journal unavailable"))}
                let mut bytes=vec![];File::open(&path)?.take(4*1024*1024+1).read_to_end(&mut bytes)?;
                if bytes.len()>4*1024*1024{return Err(io::Error::other("allocation journal unavailable"))}
                decode_journal(&bytes)?
            },
            Err(e) if e.kind()==io::ErrorKind::NotFound=>{missing_journal(registry,storage)?;Journal{schema:JOURNAL_SCHEMA,entries:vec![]}},
            Err(e)=>return Err(e),
        };
        Ok(Self{directory:directory.into(),journal,poisoned:false,events:vec![]})
    }
    fn save(&mut self)->Result<()>{
        if self.poisoned{return Err(unavailable())}
        // An older journal is rewritten as schema 3; its unbound legacy
        // entries keep `principal_binding: None` and are never upgraded.
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
    /// The employee whose idle readback this request needs: a schema-2
    /// ReadBack (S17) or a Retire (§3.12 step 4) of a `Ready` entry.
    pub fn subject(&self,request:&Request)->Option<EmployeeId>{
        let target=match request.action{
            Action::ReadBack if request.schema==wire::SCHEMA_V2=>request.item.binding.operation_id,
            Action::Retire=>wire::retire_specification(&request.item).ok()?.target.operation_id,
            _=>return None,
        };
        self.journal.entries.iter().find(|e|e.receipt.binding.operation_id==target && e.phase==Phase::Ready).map(|e|e.receipt.employee)
    }
    /// Root audit facts of the last requests, oldest first.
    pub fn take_events(&mut self)->Vec<AllocationEvent>{std::mem::take(&mut self.events)}
    pub fn handle(&mut self,request:&Request,mut env:Env<'_>)->Answer<Reply>{
        if self.poisoned{return Err(wire::ErrorCode::IsolationUnavailable)}
        if request.action==Action::Retire{return self.retire(request,env)}
        let spec=request.validate().map_err(|_|wire::ErrorCode::InvalidRequest)?;
        let v2=request.schema==wire::SCHEMA_V2;
        // Allocation never races an active run or the lifecycle's H journal.
        if request.action!=Action::ReadBack && env.host_busy{return Err(wire::ErrorCode::NotReady)}
        let binding=request.item.binding;
        let index=match replay_index(&self.journal,&binding,&spec){
            Ok(Some(i))=>i,
            // S4: a ReadBack that disagrees with the entry is Inconsistent.
            Err(e)=>return Err(if v2 && request.action==Action::ReadBack{wire::ErrorCode::Inconsistent}else{code(e)}),
            Ok(None)=>{
                if request.action==Action::ReadBack{
                    return Err(if v2{absence(&self.journal,env.registry.registry(),env.storage.traces(),None)}else{wire::ErrorCode::InvalidRequest});
                }
                if self.journal.entries.len()>=MAX_ENTRIES || self.journal.entries.iter().any(|e|e.receipt.label==spec.label && e.phase.holds_label()){return Err(wire::ErrorCode::InvalidRequest)}
                let employee=EmployeeId(crate::server::random_id().map_err(|_|wire::ErrorCode::IsolationUnavailable)?);
                if employee.0==[0;16] || self.journal.entries.iter().any(|e|e.receipt.employee==employee){return Err(wire::ErrorCode::IsolationUnavailable)}
                // The principal comes only from this confirmed proposal, never
                // from an earlier (possibly disabled) entry with the same label.
                self.journal.entries.push(Entry{receipt:Receipt{binding,label:spec.label.clone(),employee,uid:0,bytes:spec.bytes,inodes:spec.inodes,disabled:false,principal_binding:spec.binding.clone()},phase:Phase::Planned,retire:None,retired_at:None});
                // The complete binding and random principal survive before UID or disk side effects.
                self.save().map_err(code)?;self.journal.entries.len()-1
            }
        };
        let employee=self.journal.entries[index].receipt.employee;
        let quota=DiskQuota::LoopFilesystem{bytes:spec.bytes,inodes:spec.inodes};
        match request.action{
            Action::Allocate=>{
                // Never revives a compensated or retired entry (S6).
                if !matches!(self.journal.entries[index].phase,Phase::Planned|Phase::Ready){return Err(wire::ErrorCode::InvalidRequest)}
                env.storage.readiness().map_err(code)?;
                let account=env.registry.allocate(employee,env.accounts).map_err(|_|wire::ErrorCode::IsolationUnavailable)?;
                self.journal.entries[index].receipt.uid=account.uid;self.save().map_err(code)?;
                env.storage.provision(&ProvisionRequest{employee,uid:account.uid,operation_id:operation(binding,OperationKind::Allocate),proposal_digest:binding.proposal_digest,quota:quota.clone()})
                    .and_then(|usage|usage.check(employee,&quota)).map_err(code)?;
                env.accounts.verify_locked(&account).map_err(|_|wire::ErrorCode::IsolationUnavailable)?;
                env.storage.usage(employee).and_then(|usage|usage.check(employee,&quota)).map_err(code)?;
                self.journal.entries[index].phase=Phase::Ready;self.save().map_err(code)?;
            },
            Action::ReadBack=>{
                let entry=&self.journal.entries[index];
                if entry.phase!=Phase::Ready{
                    // S5c: schema 2 answers every phase with the stored receipt.
                    return if v2{Ok(stored(entry))}else{Err(wire::ErrorCode::IsolationUnavailable)};
                }
                let account=env.registry.registry().lookup(employee).map_err(|_|wire::ErrorCode::IsolationUnavailable)?;
                if account.uid!=entry.receipt.uid{return Err(if v2{wire::ErrorCode::Inconsistent}else{wire::ErrorCode::IsolationUnavailable})}
                env.accounts.verify_locked(account).map_err(|_|wire::ErrorCode::IsolationUnavailable)?;
                env.storage.usage(employee).and_then(|usage|usage.check(employee,&quota)).map_err(code)?;
                // S17: idle only from a complete readback; unknown is not idle.
                let idle=if v2{match env.idle{Some(Ok(report))=>Some(report.idle()),_=>None}}else{None};
                return Ok(Reply{receipt:self.journal.entries[index].receipt.clone(),phase:Phase::Ready,idle});
            },
            Action::Disable=>{
                if !disable_allowed(self.journal.entries[index].phase){return Err(wire::ErrorCode::InvalidRequest)}
                self.journal.entries[index].phase=Phase::Disabling;self.save().map_err(code)?;
                let account=env.registry.registry().employees.iter().find(|a|a.employee==employee).cloned();
                if let Some(account)=account{
                    env.registry.retire(employee).map_err(|_|wire::ErrorCode::IsolationUnavailable)?;
                    env.accounts.recover_locked(&account).map_err(|_|wire::ErrorCode::IsolationUnavailable)?;
                    if env.registry.registry().employees.iter().any(|a|a.employee==employee && a.state!=AccountState::Tombstone){return Err(wire::ErrorCode::IsolationUnavailable)}
                    env.storage.destroy(employee,operation(binding,OperationKind::Compensate),binding.proposal_digest).map_err(code)?;
                    if env.storage.attachments().map_err(code)?.iter().any(|a|a.employee==employee){return Err(wire::ErrorCode::IsolationUnavailable)}
                }
                self.journal.entries[index].receipt.disabled=true;
                self.journal.entries[index].phase=Phase::Disabled;self.save().map_err(code)?;
            },
            Action::Retire=>return Err(wire::ErrorCode::InvalidRequest),
        }
        Ok(stored(&self.journal.entries[index]))
    }
    /// F6 §3.12 root operation, steps 1–8. Every failure after step 5 leaves
    /// the entry `Retiring` (the label stays held) and the same request
    /// resumes it; a `Retired` replay is answered before the host gate (S15).
    fn retire(&mut self,request:&Request,mut env:Env<'_>)->Answer<Reply>{
        use wire::ErrorCode::*;
        // 1. Retire-only validation (S6): never the entry-creating path.
        let spec=request.validate_retire().map_err(|_|InvalidRequest)?;
        let approver=request.approver.clone().ok_or(InvalidRequest)?;
        let retire=request.item.binding;
        // 2. The target entry.
        let (index,fresh)=match retire_lookup(&self.journal,&spec,&retire)?{
            Lookup::Missing=>return Err(absence(&self.journal,env.registry.registry(),env.storage.traces(),Some(spec.employee))),
            Lookup::Replay(i)=>return Ok(stored(&self.journal.entries[i])),
            Lookup::Resume(i)=>(i,false),
            Lookup::Fresh(i)=>(i,true),
        };
        // 3. Host gate.
        if env.host_busy{return Err(NotReady)}
        let account=env.registry.registry().employees.iter().find(|a|a.employee==spec.employee).cloned().ok_or(Inconsistent)?;
        if account.uid!=spec.uid || account.gid!=spec.uid{return Err(Inconsistent)}
        let mut used=None;
        if fresh{
            // 4. Liveness before any change; the entry stays Ready.
            if account.state!=AccountState::Ready{return Err(Inconsistent)}
            let report=match env.idle{Some(Ok(report))=>report,_=>return Err(IsolationUnavailable)};
            if !report.idle(){
                if report.processes>0 && report.runs==0{self.events.push(AllocationEvent::Intruder{employee:spec.employee});}
                return Err(NotReady);
            }
            // Audit only (the last observed usage); unreadable is not a refusal.
            used=env.storage.usage(spec.employee).ok().map(|usage|(usage.bytes,usage.inodes));
            // 5. Retiring, saved before any side effect.
            let entry=&mut self.journal.entries[index];
            entry.phase=Phase::Retiring;entry.retire=Some(retire);
            self.save().map_err(code)?;
        }
        // 6. Account → Tombstone (runs resolve Ready accounts only), then kill
        // anything that appeared after step 4 with a no-process readback.
        if account.state!=AccountState::Tombstone{env.registry.retire(spec.employee).map_err(|_|IsolationUnavailable)?;}
        env.accounts.recover_locked(&account).map_err(|_|IsolationUnavailable)?;
        if env.registry.registry().employees.iter().any(|a|a.employee==spec.employee && a.state!=AccountState::Tombstone){return Err(IsolationUnavailable)}
        // 7. Wipe (journaled, resumable, refuses a lease); then no trace.
        env.storage.destroy(spec.employee,operation(retire,OperationKind::Retire),retire.proposal_digest).map_err(code)?;
        if env.storage.traces().map_err(code)?.contains(&spec.employee)
            || env.storage.attachments().map_err(code)?.iter().any(|a|a.employee==spec.employee){return Err(IsolationUnavailable)}
        // 8. Retired.
        let entry=&mut self.journal.entries[index];
        entry.receipt.disabled=true;entry.phase=Phase::Retired;entry.retired_at=Some(env.now_unix);
        let reply=stored(entry);
        self.save().map_err(code)?;
        let (quota_bytes,quota_inodes)=(reply.receipt.bytes,reply.receipt.inodes);
        self.events.push(AllocationEvent::Retired{employee:spec.employee,detail:Detail::Retired{uid:spec.uid,label:spec.label.clone(),
            used_bytes:used.map(|(bytes,_)|bytes),used_inodes:used.map(|(_,inodes)|inodes),quota_bytes,quota_inodes,
            quarantine_digest_prefix:spec.quarantine_digest_prefix.clone(),requested_by:spec.requested_by.clone(),approved_by:approver,early:spec.early}});
        Ok(reply)
    }
}
/// The stored receipt and phase (S5c). `idle` is only ever reported by a
/// live ReadBack of a Ready entry.
fn stored(entry:&Entry)->Reply{Reply{receipt:entry.receipt.clone(),phase:entry.phase,idle:None}}
/// `Disable` (compensation) refuses a retiring or retired entry (S6).
fn disable_allowed(phase:Phase)->bool{!matches!(phase,Phase::Retiring|Phase::Retired)}
enum Lookup{Missing,Replay(usize),Resume(usize),Fresh(usize)}
/// §3.12 step 2: the entry by the target operation id. Binding, label,
/// employee and UID must all match; only `Ready` (fresh) or the same retire
/// binding (`Retiring` resume, `Retired` replay) proceed.
fn retire_lookup(journal:&Journal,spec:&RetireSpec,retire:&ProposalBinding)->Answer<Lookup>{
    let Some(i)=journal.entries.iter().position(|e|e.receipt.binding.operation_id==spec.target.operation_id) else{return Ok(Lookup::Missing)};
    let entry=&journal.entries[i];
    let receipt=&entry.receipt;
    if receipt.binding!=spec.target || receipt.label!=spec.label || receipt.employee!=spec.employee || receipt.uid!=spec.uid{
        return Err(wire::ErrorCode::Inconsistent);
    }
    let same=entry.retire.as_ref()==Some(retire);
    match entry.phase{
        Phase::Retired if same=>Ok(Lookup::Replay(i)),
        Phase::Retiring if same=>Ok(Lookup::Resume(i)),
        Phase::Ready=>{
            // S6: a retire binding never equals an allocation or another retire.
            if journal.entries.iter().any(|e|e.receipt.binding.operation_id==retire.operation_id
                || e.retire.is_some_and(|r|r.operation_id==retire.operation_id)){return Err(wire::ErrorCode::InvalidRequest)}
            Ok(Lookup::Fresh(i))
        },
        _=>Err(wire::ErrorCode::InvalidRequest),
    }
}
/// S5b: `NotFound` only when nothing anywhere shows a trace — of the expected
/// employee (journal, registry in any state, storage), and of any live account
/// or image the journal does not know (the journal itself is then not
/// authoritative, e.g. restored from an older backup). Unreadable storage is
/// `IsolationUnavailable`, never `NotFound`.
fn absence(journal:&Journal,registry:&Registry,traces:Result<Vec<EmployeeId>>,expected:Option<EmployeeId>)->wire::ErrorCode{
    let Ok(traces)=traces else{return wire::ErrorCode::IsolationUnavailable};
    let known=|employee:&EmployeeId|journal.entries.iter().any(|e|e.receipt.employee==*employee);
    if let Some(expected)=expected{
        if known(&expected) || registry.employees.iter().any(|a|a.employee==expected) || traces.contains(&expected){return wire::ErrorCode::Inconsistent}
    }
    if registry.employees.iter().any(|a|a.state!=AccountState::Tombstone && !known(&a.employee)) || traces.iter().any(|e|!known(e)){
        return wire::ErrorCode::Inconsistent;
    }
    wire::ErrorCode::NotFound
}
/// S5a: a missing journal is opened (empty) only while the registry holds no
/// non-Tombstone account and storage reports no image or live ledger entry.
/// Unreadable storage does not block the open: no allocation, ReadBack or
/// Retire can succeed without it, and [`absence`] re-checks the traces before
/// any `NotFound`.
fn missing_journal(registry:&Registry,storage:&mut dyn Storage)->io::Result<()>{
    if registry.employees.iter().any(|a|a.state!=AccountState::Tombstone){return Err(io::Error::other("allocation journal missing while employee accounts exist"))}
    match storage.traces(){
        Ok(traces) if !traces.is_empty()=>Err(io::Error::other("allocation journal missing while employee storage exists")),
        _=>Ok(()),
    }
}

fn decode_journal(bytes:&[u8])->io::Result<Journal>{
    let bad=||io::Error::other("allocation journal unavailable");
    let journal=serde_json::from_slice::<Journal>(bytes).map_err(io::Error::other)?;
    if !matches!(journal.schema,1|2|JOURNAL_SCHEMA) || journal.entries.len()>MAX_ENTRIES{return Err(bad())}
    for (i,e) in journal.entries.iter().enumerate(){
        if e.receipt.employee.0==[0;16]{return Err(bad())}
        for p in journal.entries.iter().take(i){
            if p.receipt.employee==e.receipt.employee || (p.receipt.label==e.receipt.label && p.phase.holds_label() && e.phase.holds_label())
                || p.receipt.binding.operation_id==e.receipt.binding.operation_id{return Err(bad())}
        }
        // S6: a retire binding is unique: never any receipt's binding, never another entry's retire.
        if let Some(retire)=&e.retire{
            if journal.entries.iter().any(|p|p.receipt.binding.operation_id==retire.operation_id)
                || journal.entries.iter().enumerate().any(|(j,p)|j!=i && p.retire.is_some_and(|r|r.operation_id==retire.operation_id)){return Err(bad())}
        }
        let shape=match e.phase{
            Phase::Retiring=>e.retire.is_some() && e.retired_at.is_none() && !e.receipt.disabled,
            Phase::Retired=>e.retire.is_some() && e.retired_at.is_some() && e.receipt.disabled,
            _=>e.retire.is_none() && e.retired_at.is_none(),
        };
        // Schemas 1–2 were never written with the retire phases or fields.
        if !shape || (journal.schema<JOURNAL_SCHEMA && matches!(e.phase,Phase::Retiring|Phase::Retired)){return Err(bad())}
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
        if request.check().is_err(){continue;}
        let (reply,receive)=std::sync::mpsc::sync_channel(1);
        // A full mailbox or the 4 s worker timeout is IsolationUnavailable
        // (S5d): the gateway retries; it is never a NotFound.
        let result=if sender.try_send((request.clone(),reply)).is_ok(){
            receive.recv_timeout(std::time::Duration::from_secs(4)).unwrap_or(Err(wire::ErrorCode::IsolationUnavailable))
        }else{Err(wire::ErrorCode::IsolationUnavailable)};
        let _=transport::send(&fd,&Response::for_request(&request,result));
    })?;
    Ok(())
}

#[cfg(test)]
#[path = "allocation_tests.rs"]
mod tests;
#[cfg(test)]
#[path = "allocation_retire_tests.rs"]
mod retire_tests;
