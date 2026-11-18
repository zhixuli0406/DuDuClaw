//! R8r-2: `provision_providers` on the allocator (schema 5) with the
//! in-memory registry and the storage double of `allocation_tests`.
use super::*;
use super::tests::{Fx,FakeRegistry,allocate,employee_request,retire_request,ALICE};
use duduclaw_isolation::{ids::RequestId,migration::ConfirmationStatus};
use duduclaw_migration::allocation::providers::{decode_frame,Frame,Payload,ProviderAction,SCHEMA_V5,Zeroizing};

const DOCUMENT:&[u8]=br#"{"providers":[{"name":"main","api_key":"sk-test-SECRET-42","base_url":"https://llm.example/v1"}]}"#;
fn hex(id:&EmployeeId)->String{id.0.iter().map(|b|format!("{b:02x}")).collect()}
fn request(label:Option<&str>,id:Option<&EmployeeId>,clear:bool)->Box<ProviderRequest>{
    Box::new(ProviderRequest{schema:SCHEMA_V5,nonce:RequestId([3;16]),action:ProviderAction::ProvisionProviders,item:None,
        employee_id:id.map(hex),label:label.map(Into::into),
        payload:(!clear).then(||Payload::new(Zeroizing::new(DOCUMENT.to_vec())).unwrap()),clear})
}
fn set(label:&str)->Box<ProviderRequest>{request(Some(label),None,false)}
fn provide(fx:&mut Fx,request:Box<ProviderRequest>,host_busy:bool)->std::result::Result<Provisioned,ProviderErrorCode>{
    fx.allocator.providers(request,ProvidersEnv{registry:fx.registry.registry(),storage:&mut fx.disk,host_busy,now_unix:1_000})
}
fn record(fx:&Fx,employee:EmployeeId)->Option<Provisioned>{
    fx.allocator.journal.entries.iter().find(|e|e.receipt.employee==employee).unwrap().providers
}
fn disable(id:u8,label:&str)->Request{
    let mut request=employee_request(id,label,ConfirmationStatus::Failed,ALICE);request.action=Action::Disable;request
}

#[test]
fn providers_by_label_and_id_write_and_record_no_payload_or_hash(){
    let mut fx=Fx::portable();
    let receipt=allocate(&mut fx,1,"sales");
    let done=provide(&mut fx,set("sales"),false).unwrap();
    assert_eq!(done,Provisioned{provisioned:true,updated_unix:1_000});
    assert_eq!(fx.disk.documents.get(&receipt.employee).map(Vec::as_slice),Some(DOCUMENT));
    assert_eq!(fx.disk.provider_calls,vec![(receipt.employee,receipt.uid,true)],"the employee's own uid");
    assert_eq!(record(&fx,receipt.employee),Some(done));
    // Every call rewrites (no "same content ⇒ skip"), by id as well.
    provide(&mut fx,request(None,Some(&receipt.employee),false),false).unwrap();
    assert_eq!(fx.disk.provider_calls.len(),2);
    let journal=String::from_utf8(fs::read(fx.dir.join("allocations.json")).unwrap()).unwrap();
    assert!(journal.contains(r#""providers":{"provisioned":true,"updated_unix":1000}"#),"{journal}");
    assert!(!journal.contains("SECRET") && !journal.contains("api_key") && !journal.contains("hash") && !journal.contains("payload"),"{journal}");
    fx.reopen();
    assert_eq!(record(&fx,receipt.employee),Some(done),"the record survives a restart");
}

#[test]
fn providers_resolution_refuses_unknown_planned_disabled_and_registry_mismatch(){
    let mut fx=Fx::portable();
    assert_eq!(provide(&mut fx,set("sales"),false),Err(ProviderErrorCode::NotAllocated),"unknown label");
    assert_eq!(provide(&mut fx,request(None,Some(&EmployeeId([9;16])),false),false),Err(ProviderErrorCode::NotAllocated),"unknown id");
    fx.accounts.create_fail=true;
    assert!(fx.handle(&employee_request(2,"planned",ConfirmationStatus::Applying,ALICE)).is_err());
    assert_eq!(provide(&mut fx,set("planned"),false),Err(ProviderErrorCode::NotAllocated),"planned");
    fx.accounts.create_fail=false;
    let gone=allocate(&mut fx,3,"gone");
    assert_eq!(fx.handle(&disable(3,"gone")).unwrap().phase,Phase::Disabled);
    assert_eq!(provide(&mut fx,set("gone"),false),Err(ProviderErrorCode::NotAllocated),"a disabled entry holds no label");
    assert_eq!(provide(&mut fx,request(None,Some(&gone.employee),false),false),Err(ProviderErrorCode::NotAllocated),"disabled");
    // Journal Ready, registry row Tombstone / another UID / missing.
    let live=allocate(&mut fx,4,"live");
    let healthy=fx.registry.registry().clone();
    fx.registry.retire(live.employee).unwrap();
    assert_eq!(provide(&mut fx,set("live"),false),Err(ProviderErrorCode::NotAllocated),"tombstone");
    let mut other=healthy.clone();
    let row=other.employees.iter_mut().find(|a|a.employee==live.employee).unwrap();row.uid+=1;row.gid+=1;
    fx.registry=Box::new(FakeRegistry(other));
    assert_eq!(provide(&mut fx,set("live"),false),Err(ProviderErrorCode::NotAllocated),"other uid");
    fx.registry=Box::new(FakeRegistry(Registry::default()));
    assert_eq!(provide(&mut fx,set("live"),false),Err(ProviderErrorCode::NotAllocated),"missing row");
    fx.registry=Box::new(FakeRegistry(healthy));
    assert!(fx.disk.provider_calls.is_empty(),"no refusal reaches storage");
    // The gateway names the employee by its confirmation binding.
    let mut gateway=request(None,None,false);
    gateway.item=Some(employee_request(4,"live",ConfirmationStatus::Applied,ALICE).item);
    assert_eq!(provide(&mut fx,gateway,false).unwrap().provisioned,true);
    let mut changed=request(None,None,false);
    let mut item=employee_request(4,"live",ConfirmationStatus::Applied,&[]).item;item.binding.operation_id=live.binding.operation_id;
    changed.item=Some(item);
    assert_eq!(provide(&mut fx,changed,false),Err(ProviderErrorCode::NotAllocated),"a binding that disagrees with the entry");
}

#[test]
fn providers_wait_while_a_run_is_live_or_the_employee_is_attached(){
    let mut fx=Fx::portable();
    let receipt=allocate(&mut fx,1,"sales");
    let other=allocate(&mut fx,2,"ops");
    assert_eq!(provide(&mut fx,set("sales"),true),Err(ProviderErrorCode::NotReady),"host busy");
    let lease=|employee|crate::storage::Attachment{employee,run:duduclaw_isolation::execution::RunHandle(duduclaw_isolation::ids::RunId([5;16])),
        token:RequestId([6;16]),device_major:7,device_minor:1};
    fx.disk.leases.push(lease(receipt.employee));
    assert_eq!(provide(&mut fx,set("sales"),false),Err(ProviderErrorCode::NotReady),"attached");
    assert!(fx.disk.provider_calls.is_empty());
    assert_eq!(record(&fx,receipt.employee),None,"nothing recorded");
    fx.disk.leases=vec![lease(other.employee)];
    assert!(provide(&mut fx,set("sales"),false).is_ok(),"another employee's lease is not this one's");
}

#[test]
fn providers_clear_records_not_provisioned_and_tolerates_absence(){
    let mut fx=Fx::portable();
    let receipt=allocate(&mut fx,1,"sales");
    let cleared=provide(&mut fx,request(Some("sales"),None,true),false).unwrap();
    assert_eq!(cleared,Provisioned{provisioned:false,updated_unix:1_000});
    provide(&mut fx,set("sales"),false).unwrap();
    provide(&mut fx,request(Some("sales"),None,true),false).unwrap();
    assert!(fx.disk.documents.is_empty());
    assert_eq!(record(&fx,receipt.employee).map(|r|r.provisioned),Some(false));
    assert_eq!(fx.disk.provider_calls.iter().map(|c|c.2).collect::<Vec<_>>(),[false,true,false]);
}

#[test]
fn providers_storage_failures_are_fixed_codes_and_keep_the_conservative_record(){
    let mut fx=Fx::portable();
    let receipt=allocate(&mut fx,1,"sales");
    for (failure,code) in [(ProvidersFailure::HomeEntryUnsafe,ProviderErrorCode::HomeEntryUnsafe),
        (ProvidersFailure::NoSpace,ProviderErrorCode::StorageUnavailable),(ProvidersFailure::Unavailable,ProviderErrorCode::StorageUnavailable),
        (ProvidersFailure::Busy,ProviderErrorCode::NotReady)]{
        fx.disk.provider_fail=Some(failure);
        assert_eq!(provide(&mut fx,set("sales"),false),Err(code));
    }
    // A write that may have left a file is recorded as provisioned, so Disable removes it.
    assert_eq!(record(&fx,receipt.employee).map(|r|r.provisioned),Some(true));
    fx.disk.provider_fail=None;
    // An invalid payload is refused before any storage call.
    let frame=br#"{"schema":5,"nonce":[3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3],"action":"provision_providers","label":"sales","payload":"!!!!"}"#;
    let Ok(Frame::Providers(invalid))=decode_frame(frame) else{panic!("a schema-5 frame")};
    let calls=fx.disk.provider_calls.len();
    assert_eq!(provide(&mut fx,invalid,false),Err(ProviderErrorCode::InvalidPayload));
    assert_eq!(fx.disk.provider_calls.len(),calls);
}

#[test]
fn disable_removes_a_provisioned_document_first_and_resumes(){
    let mut fx=Fx::portable();
    let receipt=allocate(&mut fx,1,"sales");
    provide(&mut fx,set("sales"),false).unwrap();
    // A removal that cannot run refuses the Disable before its state change.
    fx.disk.provider_fail=Some(ProvidersFailure::Unavailable);
    assert_eq!(fx.handle(&disable(1,"sales")).unwrap_err(),wire::ErrorCode::IsolationUnavailable);
    assert_eq!(fx.phase(receipt.employee),Phase::Ready);
    assert!(fx.disk.documents.contains_key(&receipt.employee));
    fx.disk.provider_fail=Some(ProvidersFailure::Busy);
    assert_eq!(fx.handle(&disable(1,"sales")).unwrap_err(),wire::ErrorCode::NotReady);
    fx.disk.provider_fail=None;
    assert_eq!(fx.handle_with(&disable(1,"sales"),true,None).unwrap_err(),wire::ErrorCode::NotReady,"a live run: the existing gate");
    let calls=fx.disk.provider_calls.len();
    assert_eq!(fx.handle(&disable(1,"sales")).unwrap().phase,Phase::Disabled);
    assert_eq!(fx.disk.provider_calls.len(),calls+1);
    assert_eq!(fx.disk.provider_calls.last(),Some(&(receipt.employee,receipt.uid,false)),"the safe unlink");
    assert_eq!(record(&fx,receipt.employee),None,"the record is cleared");
    assert_eq!(fx.handle(&disable(1,"sales")).unwrap().phase,Phase::Disabled,"replay");
    assert_eq!(fx.disk.provider_calls.len(),calls+1,"nothing more to remove");
    // Without a provisioned record Disable never touches the home.
    let mut fx=Fx::portable();
    allocate(&mut fx,1,"sales");
    provide(&mut fx,request(Some("sales"),None,true),false).unwrap();
    let calls=fx.disk.provider_calls.len();
    assert_eq!(fx.handle(&disable(1,"sales")).unwrap().phase,Phase::Disabled);
    assert_eq!(fx.disk.provider_calls.len(),calls);
}

#[test]
fn retire_clears_the_provider_record_with_the_image(){
    let mut fx=Fx::portable();
    let receipt=allocate(&mut fx,1,"sales");
    provide(&mut fx,set("sales"),false).unwrap();
    let calls=fx.disk.provider_calls.len();
    assert_eq!(fx.handle(&retire_request(&receipt,0x41)).unwrap().phase,Phase::Retired);
    assert_eq!(record(&fx,receipt.employee),None);
    assert_eq!(fx.disk.provider_calls.len(),calls,"the image is destroyed; no separate unlink");
    assert!(!fx.disk.present(receipt.employee));
}

#[test]
fn journal_provider_record_belongs_to_ready_schema3_entries(){
    let mut fx=Fx::portable();
    allocate(&mut fx,1,"sales");
    provide(&mut fx,set("sales"),false).unwrap();
    let text=String::from_utf8(fs::read(fx.dir.join("allocations.json")).unwrap()).unwrap();
    assert!(decode_journal(text.as_bytes()).is_ok());
    assert!(decode_journal(text.replace(r#""schema":3"#,r#""schema":2"#).as_bytes()).is_err(),"older schemas never carry it");
    assert!(decode_journal(text.replace(r#""phase":"ready""#,r#""phase":"disabling""#).as_bytes()).is_err(),"Ready only");
    assert!(decode_journal(text.replace(r#""updated_unix":1000}"#,r#""updated_unix":1000,"sha256":"00"}"#).as_bytes()).is_err(),"no hash field");
}
