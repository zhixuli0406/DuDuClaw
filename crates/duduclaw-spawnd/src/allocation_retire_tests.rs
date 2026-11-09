//! F6l root retire (F6 design r5 §3.12, §6 F6l row). Every scenario runs
//! portably against an in-memory registry (`tests::retire_scenarios_…`) and
//! as an `iso_root_retire_` test against the real root-owned `RegistryStore`
//! under `/run`; account and storage effects are injected in both.
use super::*;
use super::tests::*;
use std::os::unix::process::CommandExt;
use duduclaw_isolation::{ids::{RequestId,RunId,Sha256Digest},execution::RunHandle,migration::ConfirmationStatus};
use crate::storage::Attachment;
use duduclaw_migration::allocation::ErrorCode as W;

pub(super) type Scenario=fn(&mut Fx);
pub(super) const SCENARIOS:&[(&str,Scenario)]=&[
    ("refuses_live_run_unit_lease_or_process",refuses_live_run_unit_lease_or_process),
    ("resume_skips_intruder_refusal",resume_skips_intruder_refusal),
    ("refuses_target_mismatch",refuses_target_mismatch),
    ("refuses_non_ready_entry",refuses_non_ready_entry),
    ("replay_same_op_idempotent",replay_same_op_idempotent),
    ("retired_replay_answered_before_host_gate",retired_replay_answered_before_host_gate),
    ("second_retire_binding_refused",second_retire_binding_refused),
    ("crash_after_retiring_resumes",crash_after_retiring_resumes),
    ("partial_destroy_resumes",partial_destroy_resumes),
    ("frees_label_for_new_allocation",frees_label_for_new_allocation),
    ("uid_never_recycled",uid_never_recycled),
    ("allocate_and_readback_refused_after",allocate_and_readback_refused_after),
    ("audits_retired_with_requester_approver",audits_retired_with_requester_approver),
    ("missing_journal_with_registry_or_images_refuses_open",missing_journal_with_registry_or_images_refuses_open),
    ("not_found_only_without_any_trace",not_found_only_without_any_trace),
];
fn intruders(fx:&mut Fx,employee:EmployeeId)->usize{
    fx.allocator.take_events().iter().filter(|e|matches!(e,AllocationEvent::Intruder{employee:who} if *who==employee)).count()
}
fn untouched(fx:&Fx,receipt:&Receipt){
    assert_eq!(fx.phase(receipt.employee),Phase::Ready);
    assert_eq!(fx.account(receipt.employee).state,AccountState::Ready);
    assert!(fx.disk.present(receipt.employee));
}
fn same(a:&Receipt,b:&Receipt)->bool{serde_json::to_value(a).unwrap()==serde_json::to_value(b).unwrap()}
fn readback(id:u8,label:&str,schema:u16)->Request{
    let mut request=employee_request(id,label,ConfirmationStatus::Applied,ALICE);request.action=Action::ReadBack;request.schema=schema;request
}
fn disable(id:u8,label:&str)->Request{
    let mut request=employee_request(id,label,ConfirmationStatus::Failed,ALICE);request.action=Action::Disable;request
}

/// §3.12 steps 3–4 (S14): any run, unit, lease or process refuses; the entry stays Ready.
fn refuses_live_run_unit_lease_or_process(fx:&mut Fx){
    let receipt=allocate(fx,1,"sales");let request=retire_request(&receipt,0x41);
    assert_eq!(fx.handle_with(&request,true,None).unwrap_err(),W::NotReady,"host gate");
    for busy in [EmployeeIdleReport{runs:1,..quiet()},EmployeeIdleReport{units:1,..quiet()},
        EmployeeIdleReport{attachments:1,..quiet()},EmployeeIdleReport{runs:1,processes:2,..quiet()}]{
        assert_eq!(fx.handle_with(&request,false,Some(Ok(busy))).unwrap_err(),W::NotReady,"{busy:?}");
        untouched(fx,&receipt);
    }
    assert_eq!(intruders(fx,receipt.employee),0,"processes of a known run are not an intrusion");
    // A process with no run behind it: refused and audited as an intruder.
    assert_eq!(fx.handle_with(&request,false,Some(Ok(EmployeeIdleReport{processes:1,..quiet()}))).unwrap_err(),W::NotReady);
    assert_eq!(intruders(fx,receipt.employee),1);
    // An unreadable liveness source refuses (never read as zero).
    assert_eq!(fx.handle_with(&request,false,Some(Err(ErrorCode::IsolationUnavailable))).unwrap_err(),W::IsolationUnavailable);
    untouched(fx,&receipt);
    assert_eq!((fx.accounts.recovered,fx.disk.destroys),(0,0));
    assert_eq!(fx.handle(&request).unwrap().phase,Phase::Retired,"once idle the retire proceeds");
}
/// S14: a `Retiring` resume skips the liveness refusal and relies on recover_locked.
fn resume_skips_intruder_refusal(fx:&mut Fx){
    let receipt=allocate(fx,1,"sales");let request=retire_request(&receipt,0x41);
    fx.accounts.recover_fail=1;
    assert_eq!(fx.handle(&request).unwrap_err(),W::IsolationUnavailable);
    assert_eq!(fx.phase(receipt.employee),Phase::Retiring);
    assert_eq!(fx.allocator.subject(&request),None,"no idle readback is needed to resume");
    let recovered=fx.accounts.recovered;
    let reply=fx.handle_with(&request,false,Some(Ok(EmployeeIdleReport{processes:3,..quiet()}))).unwrap();
    assert_eq!(reply.phase,Phase::Retired);
    assert!(fx.accounts.recovered>recovered,"stray processes are killed with a no-process readback");
    assert_eq!(intruders(fx,receipt.employee),0);
}
/// §3.12 step 2: binding, label, employee and UID must all match (else Inconsistent).
fn refuses_target_mismatch(fx:&mut Fx){
    let receipt=allocate(fx,1,"sales");
    for change in 0..5{
        let mut spec=retire_spec(&receipt);
        match change{
            0=>spec.uid+=1, 1=>spec.label="other".into(), 2=>spec.employee=EmployeeId([0x55;16]),
            3=>spec.target.proposal_digest=Sha256Digest([0x66;32]), _=>spec.target.operation_id.revision=2,
        }
        assert_eq!(fx.handle(&retire_request_for(&spec,0x41)).unwrap_err(),W::Inconsistent,"change {change}");
        untouched(fx,&receipt);
    }
    assert_eq!((fx.accounts.recovered,fx.disk.destroys),(0,0));
}
/// Planned, Disabling and Disabled entries are never retired.
fn refuses_non_ready_entry(fx:&mut Fx){
    fx.disk.provision_fail=true;
    assert!(fx.handle(&employee_request(2,"planned",ConfirmationStatus::Applying,TEAM)).is_err());
    fx.disk.provision_fail=false;
    let planned=fx.allocator.journal.entries.iter().find(|e|e.receipt.label=="planned").unwrap().clone();
    assert_eq!(planned.phase,Phase::Planned);
    assert_eq!(fx.handle(&retire_request(&planned.receipt,0x42)).unwrap_err(),W::InvalidRequest);
    let dis=allocate(fx,3,"dis");
    fx.disk.destroy_fail=1;
    assert!(fx.handle(&disable(3,"dis")).is_err());
    assert_eq!(fx.phase(dis.employee),Phase::Disabling);
    assert_eq!(fx.handle(&retire_request(&dis,0x43)).unwrap_err(),W::InvalidRequest);
    let dead=fx.handle(&disable(3,"dis")).unwrap().receipt;
    assert_eq!(fx.phase(dis.employee),Phase::Disabled);
    assert_eq!(fx.handle(&retire_request(&dead,0x43)).unwrap_err(),W::InvalidRequest);
    assert_eq!(fx.handle(&retire_request(&dis,0x43)).unwrap_err(),W::InvalidRequest);
}
fn replay_same_op_idempotent(fx:&mut Fx){
    let receipt=allocate(fx,1,"sales");let request=retire_request(&receipt,0x41);
    let first=fx.handle(&request).unwrap();
    let (destroys,recovered)=(fx.disk.destroys,fx.accounts.recovered);
    fx.allocator.take_events();
    let second=fx.handle(&request).unwrap();
    assert!(same(&first.receipt,&second.receipt) && second.phase==Phase::Retired);
    assert_eq!((fx.disk.destroys,fx.accounts.recovered),(destroys,recovered),"a replay has no side effect");
    assert!(fx.allocator.take_events().is_empty(),"a replay is not audited again");
    fx.reopen();
    assert!(same(&fx.handle(&request).unwrap().receipt,&first.receipt));
}
/// S15: a Retired replay is answered even while the host is busy.
fn retired_replay_answered_before_host_gate(fx:&mut Fx){
    let receipt=allocate(fx,1,"sales");let request=retire_request(&receipt,0x41);
    fx.handle(&request).unwrap();
    let reply=fx.handle_with(&request,true,Some(Err(ErrorCode::IsolationUnavailable))).unwrap();
    assert_eq!(reply.phase,Phase::Retired);
    assert!(reply.receipt.disabled);
}
/// S6: another retire binding on a retiring or retired entry, or a retire
/// binding equal to an allocation binding, is refused.
fn second_retire_binding_refused(fx:&mut Fx){
    let receipt=allocate(fx,1,"sales");
    fx.accounts.recover_fail=1;
    assert!(fx.handle(&retire_request(&receipt,0x41)).is_err());
    assert_eq!(fx.phase(receipt.employee),Phase::Retiring);
    assert_eq!(fx.handle(&retire_request(&receipt,0x42)).unwrap_err(),W::InvalidRequest);
    fx.handle(&retire_request(&receipt,0x41)).unwrap();
    assert_eq!(fx.handle(&retire_request(&receipt,0x42)).unwrap_err(),W::InvalidRequest);
    let other=allocate(fx,2,"other");
    // A retire whose own operation id is the other employee's allocation id.
    assert_eq!(fx.handle(&retire_request(&other,2)).unwrap_err(),W::InvalidRequest);
    // … or the first employee's (already used) retire id.
    assert_eq!(fx.handle(&retire_request(&other,0x41)).unwrap_err(),W::InvalidRequest);
    untouched(fx,&other);
}
fn crash_after_retiring_resumes(fx:&mut Fx){
    let receipt=allocate(fx,1,"sales");let request=retire_request(&receipt,0x41);
    fx.accounts.recover_fail=1;
    assert_eq!(fx.handle(&request).unwrap_err(),W::IsolationUnavailable);
    fx.reopen();
    assert_eq!(fx.phase(receipt.employee),Phase::Retiring,"Retiring was durable before the side effects");
    assert_eq!(fx.account(receipt.employee).state,AccountState::Tombstone);
    // The label stays held until Retired.
    assert_eq!(fx.handle(&employee_request(5,"sales",ConfirmationStatus::Applying,TEAM)).unwrap_err(),W::InvalidRequest);
    assert_eq!(fx.handle(&request).unwrap().phase,Phase::Retired);
    assert!(!fx.disk.present(receipt.employee));
}
fn partial_destroy_resumes(fx:&mut Fx){
    let receipt=allocate(fx,1,"sales");let request=retire_request(&receipt,0x41);
    fx.disk.destroy_fail=1;
    assert_eq!(fx.handle(&request).unwrap_err(),W::IsolationUnavailable);
    assert_eq!(fx.phase(receipt.employee),Phase::Retiring);
    assert!(fx.disk.present(receipt.employee),"image not yet destroyed");
    fx.reopen();
    assert_eq!(fx.handle(&request).unwrap().phase,Phase::Retired);
    assert!(!fx.disk.present(receipt.employee));
    // A lease left behind refuses the wipe; the entry stays Retiring.
    let other=allocate(fx,2,"other");
    fx.disk.leases.push(Attachment{employee:other.employee,run:RunHandle(RunId([9;16])),token:RequestId([9;16]),device_major:7,device_minor:1});
    assert!(fx.handle(&retire_request(&other,0x42)).is_err());
    assert_eq!(fx.phase(other.employee),Phase::Retiring);
    fx.disk.leases.clear();
    assert_eq!(fx.handle(&retire_request(&other,0x42)).unwrap().phase,Phase::Retired);
}
fn frees_label_for_new_allocation(fx:&mut Fx){
    let receipt=allocate(fx,1,"sales");
    let next=employee_request(5,"sales",ConfirmationStatus::Applying,TEAM);
    assert_eq!(fx.handle(&next).unwrap_err(),W::InvalidRequest,"a Ready entry holds the label");
    fx.handle(&retire_request(&receipt,0x41)).unwrap();
    let fresh=fx.handle(&next).unwrap();
    assert_eq!((fresh.phase,fresh.receipt.label.as_str()),(Phase::Ready,"sales"));
    assert_ne!(fresh.receipt.employee,receipt.employee,"a new employee id");
    assert_ne!(fresh.receipt.uid,receipt.uid,"a new UID");
}
/// R15 / §7 N4: the retired UID stays burned and mapped.
fn uid_never_recycled(fx:&mut Fx){
    let receipt=allocate(fx,1,"sales");
    fx.handle(&retire_request(&receipt,0x41)).unwrap();
    for (id,label) in [(5u8,"sales"),(6,"b"),(7,"c")]{
        assert_ne!(allocate(fx,id,label).uid,receipt.uid);
    }
    let account=fx.account(receipt.employee);
    assert_eq!((account.uid,account.state),(receipt.uid,AccountState::Tombstone),"the mapping is never removed");
    fx.reopen();
    let entry=fx.allocator.journal.entries.iter().find(|e|e.receipt.employee==receipt.employee).unwrap();
    assert_eq!((entry.phase,entry.receipt.uid),(Phase::Retired,receipt.uid));
}
fn allocate_and_readback_refused_after(fx:&mut Fx){
    let receipt=allocate(fx,1,"sales");
    fx.handle(&retire_request(&receipt,0x41)).unwrap();
    let (created,provisions)=(fx.accounts.created,fx.disk.provisions);
    assert_eq!(fx.handle(&employee_request(1,"sales",ConfirmationStatus::Applying,ALICE)).unwrap_err(),W::InvalidRequest,"never revived");
    assert_eq!(fx.handle(&readback(1,"sales",1)).unwrap_err(),W::IsolationUnavailable,"schema 1: not Ready");
    let reply=fx.handle(&readback(1,"sales",wire::SCHEMA_V2)).unwrap();
    assert_eq!((reply.phase,reply.receipt.disabled),(Phase::Retired,true));
    assert_eq!(fx.handle(&disable(1,"sales")).unwrap_err(),W::InvalidRequest);
    assert_eq!((fx.accounts.created,fx.disk.provisions),(created,provisions));
    assert_eq!(fx.phase(receipt.employee),Phase::Retired);
}
fn audits_retired_with_requester_approver(fx:&mut Fx){
    let receipt=allocate(fx,1,"sales");
    fx.allocator.take_events();
    fx.handle(&retire_request(&receipt,0x41)).unwrap();
    let events=fx.allocator.take_events();
    assert_eq!(events.len(),1);
    let AllocationEvent::Retired{employee,detail}=&events[0] else{panic!("expected Retired")};
    assert_eq!(*employee,receipt.employee);
    assert_eq!(*detail,Detail::Retired{uid:receipt.uid,label:"sales".into(),used_bytes:Some(4096),used_inodes:Some(3),
        quota_bytes:receipt.bytes,quota_inodes:receipt.inodes,quarantine_digest_prefix:"00112233445566ff".into(),
        requested_by:"admin-a".into(),approved_by:"admin-b".into(),early:true});
    let record=crate::audit::Record{at_unix_ms:1,event:crate::audit::Event::Retired,employee:Some(receipt.employee),run:None,suppressed:0,detail:Some(detail.clone())};
    let json=serde_json::to_value(&record).unwrap();
    assert_eq!((json["event"].as_str(),json["detail"]["kind"].as_str()),(Some("retired"),Some("retired")));
    assert_eq!((json["detail"]["requested_by"].as_str(),json["detail"]["approved_by"].as_str()),(Some("admin-a"),Some("admin-b")));
    assert!(serde_json::from_value::<crate::audit::Record>(json).is_ok());
}
/// S5a: a lost journal is refused while the registry or storage shows an employee.
fn missing_journal_with_registry_or_images_refuses_open(fx:&mut Fx){
    assert!(missing_journal(fx.registry.registry(),&mut fx.disk).is_ok(),"nothing anywhere: opens empty");
    fx.disk.traces_fail=true;
    assert!(missing_journal(fx.registry.registry(),&mut fx.disk).is_ok(),"unreadable storage: absence re-checks before any NotFound");
    fx.disk.traces_fail=false;
    fx.disk.extra_traces.push(EmployeeId([0x33;16]));
    assert!(missing_journal(fx.registry.registry(),&mut fx.disk).is_err(),"an image without a journal");
    fx.disk.extra_traces.clear();
    let receipt=allocate(fx,1,"sales");
    assert!(missing_journal(fx.registry.registry(),&mut fx.disk).is_err(),"a live account without a journal");
    fx.handle(&retire_request(&receipt,0x41)).unwrap();
    assert!(missing_journal(fx.registry.registry(),&mut fx.disk).is_ok(),"only Tombstone accounts and no image remain");
    if fx.root{
        allocate(fx,2,"other");
        fs::remove_file(fx.dir.join("allocations.json")).unwrap();
        assert!(Allocator::open_at(&fx.dir,fx.registry.registry(),&mut fx.disk).is_err(),"the real open refuses");
    }
}
/// S5b: NotFound only when journal, registry and storage show no trace.
fn not_found_only_without_any_trace(fx:&mut Fx){
    let ghost=Receipt{binding:employee_request(9,"ghost",ConfirmationStatus::Applied,ALICE).item.binding,label:"ghost".into(),
        employee:EmployeeId([0x77;16]),uid:70_500,bytes:67108864,inodes:4096,disabled:false,principal_binding:None};
    let request=retire_request(&ghost,0x49);
    assert_eq!(fx.handle(&request).unwrap_err(),W::NotFound);
    fx.disk.traces_fail=true;
    assert_eq!(fx.handle(&request).unwrap_err(),W::IsolationUnavailable,"unreadable storage is never NotFound");
    fx.disk.traces_fail=false;
    fx.disk.extra_traces.push(ghost.employee);
    assert_eq!(fx.handle(&request).unwrap_err(),W::Inconsistent,"storage trace of the employee");
    fx.disk.extra_traces=vec![EmployeeId([0x78;16])];
    assert_eq!(fx.handle(&request).unwrap_err(),W::Inconsistent,"an image the journal does not know");
    fx.disk.extra_traces.clear();
    allocate(fx,1,"sales");
    assert_eq!(fx.handle(&request).unwrap_err(),W::NotFound,"tracked employees are not a trace of this one");
    let stray=EmployeeId([0x79;16]);
    fx.registry.allocate(stray,&mut fx.accounts).unwrap();
    assert_eq!(fx.handle(&request).unwrap_err(),W::Inconsistent,"a live account the journal does not know (stale journal)");
    assert_eq!(fx.handle(&readback(9,"ghost",wire::SCHEMA_V2)).unwrap_err(),W::Inconsistent,"same rule for ReadBack");
    assert_eq!(fx.handle(&readback(9,"ghost",1)).unwrap_err(),W::InvalidRequest,"schema 1 unchanged");
    fx.registry.retire(stray).unwrap();
    assert_eq!(fx.handle(&request).unwrap_err(),W::NotFound,"unknown tombstones are allowed (S5a)");
    assert_eq!(fx.handle(&readback(9,"ghost",wire::SCHEMA_V2)).unwrap_err(),W::NotFound);
    fx.registry.allocate(ghost.employee,&mut fx.accounts).unwrap();
    assert_eq!(fx.handle(&request).unwrap_err(),W::Inconsistent,"a registry account of the employee");
    fx.registry.retire(ghost.employee).unwrap();
    assert_eq!(fx.handle(&request).unwrap_err(),W::Inconsistent,"in any state");
}

macro_rules! root_scenario {
    ($($test:ident => $scenario:ident),* $(,)?) => {$(
        #[test]
        #[ignore="root Linux /run registry fixture in a disposable VM (DUDUCLAW_ISO_DISPOSABLE=1); account and storage effects injected"]
        fn $test(){ let mut fx=Fx::root(stringify!($scenario)); $scenario(&mut fx); fx.finish(); }
    )*};
}
root_scenario!{
    iso_root_retire_refuses_live_run_unit_lease_or_process => refuses_live_run_unit_lease_or_process,
    iso_root_retire_resume_skips_intruder_refusal => resume_skips_intruder_refusal,
    iso_root_retire_refuses_target_mismatch => refuses_target_mismatch,
    iso_root_retire_refuses_non_ready_entry => refuses_non_ready_entry,
    iso_root_retire_replay_same_op_idempotent => replay_same_op_idempotent,
    iso_root_retire_retired_replay_answered_before_host_gate => retired_replay_answered_before_host_gate,
    iso_root_retire_second_retire_binding_refused => second_retire_binding_refused,
    iso_root_retire_crash_after_retiring_resumes => crash_after_retiring_resumes,
    iso_root_retire_partial_destroy_resumes => partial_destroy_resumes,
    iso_root_retire_frees_label_for_new_allocation => frees_label_for_new_allocation,
    iso_root_retire_uid_never_recycled => uid_never_recycled,
    iso_root_retire_allocate_and_readback_refused_after => allocate_and_readback_refused_after,
    iso_root_retire_audits_retired_with_requester_approver => audits_retired_with_requester_approver,
    iso_root_retire_missing_journal_with_registry_or_images_refuses_open => missing_journal_with_registry_or_images_refuses_open,
    iso_root_retire_not_found_only_without_any_trace => not_found_only_without_any_trace,
}

/// S14 with a real process: a root process that carries only the employee's
/// gid (uid stays 0) counts as live through the H4 `/proc` scan.
#[test]
#[ignore="root Linux: spawns /bin/sleep with the employee gid and reads /proc (disposable VM, DUDUCLAW_ISO_DISPOSABLE=1)"]
fn iso_root_retire_gid_match_counts_as_live(){
    let mut fx=Fx::root("gid");
    let receipt=allocate(&mut fx,1,"sales");let request=retire_request(&receipt,0x41);
    let gid=fx.account(receipt.employee).gid;
    let mut child=std::process::Command::new("/bin/sleep").arg("30").gid(gid).spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    let count=crate::accounts::employee_process_count(receipt.uid,gid).unwrap();
    assert!(count>=1,"a gid-only process is the employee's");
    let report=EmployeeIdleReport{processes:u16::try_from(count).unwrap_or(u16::MAX),..quiet()};
    assert_eq!(fx.handle_with(&request,false,Some(Ok(report))).unwrap_err(),W::NotReady);
    assert_eq!(fx.phase(receipt.employee),Phase::Ready);
    assert_eq!(intruders(&mut fx,receipt.employee),1,"no run behind it: an intrusion");
    child.kill().unwrap();child.wait().unwrap();
    assert_eq!(crate::accounts::employee_process_count(receipt.uid,gid).unwrap(),0);
    assert_eq!(fx.handle_with(&request,false,Some(Ok(quiet()))).unwrap().phase,Phase::Retired);
    fx.finish();
}
