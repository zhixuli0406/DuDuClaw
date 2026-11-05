//! Post-confirmation root coordinator. Root alone can resolve legacy blob handles.
use super::*;
use crate::{contract::{ConfirmationCategory, ConfirmationItem, ConfirmationStatus, ProposalValue},
    materialization::{ConfirmedAllocation, MaterializationDecision, MaterializationStore, BlobCopyReceipt},
    import_receiver::ImportedContent, ids::RequestId};

pub(super) fn blob_handle(migration:RequestId,source:&str)->RequestId {
    let digest=Sha256::digest(serde_json::to_vec(&("migration-blob-v1",migration,source)).expect("strings serialize"));
    let mut id=[0;16];id.copy_from_slice(&digest[..16]);RequestId(id)
}
#[derive(Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct InventorySeal {version:u16,migration:RequestId,manifest_sha256:[u8;32]}
pub(super) fn seal_inventory(plan:&RootPlan,encoded:&[u8])->Result<(),Error>{
    let seal=InventorySeal{version:2,migration:plan.migration,manifest_sha256:Sha256::digest(encoded).into()};
    publish(&plan.state_dir.join("content-inventory-v2.json"),&serde_json::to_vec(&seal)?,0,0,false)
}
pub(super) fn verify_inventory(plan:&RootPlan)->Result<(),Error>{
    let seal:InventorySeal=serde_json::from_slice(&read_private(&plan.state_dir.join("content-inventory-v2.json"),4096,0)?)?;
    let encoded=read_private(&plan.state_dir.join("import-plan.json"),MAX_MANIFEST_BYTES,0)?;
    if seal.version!=2 || seal.migration!=plan.migration || seal.manifest_sha256!=<[u8;32]>::from(Sha256::digest(encoded)) {return Err(Error::Readback);}
    manifest(plan)?;Ok(())
}
fn proposals(plan:&RootPlan)->Result<Vec<ConfirmationItem>,Error>{
    let path=plan.legacy_home.join("migration-confirmations.db");
    if !path.try_exists()? {return Ok(vec![]);}
    path_metadata(&path,false,plan.gateway_uid)?;
    let items=crate::confirmation::ConfirmationStore::open_read_only(&path).map_err(|_|Error::Storage)?.list().map_err(|_|Error::Storage)?;
    verify_inventory(plan)?;
    let expected=manifest(plan)?;
    let mut latest=Vec::new();
    let mut expected_ids=std::collections::BTreeSet::new();
    // Keep the original normalized proposal as lineage, but materialize the
    // latest confirmed revision. Expired old revisions are not duplicate staff.
    for pending in &expected.pending_proposals {
        let hash=Sha256::digest(serde_json::to_vec(&(pending.migration_id,&pending.source_id,pending.sequence))?);
        let mut id=[0;16];id.copy_from_slice(&hash[..16]);expected_ids.insert(id);
        let mut fields=pending.fields.clone();fields.sort_by(|a,b|a.name.cmp(&b.name));
        let versions=items.iter().filter(|item|item.binding.operation_id.proposal_id==RequestId(id)).collect::<Vec<_>>();
        if !versions.iter().any(|item|item.binding.operation_id.revision==1 && item.category==pending.category && item.fields==fields)
            || versions.iter().any(|item|item.category!=pending.category) {return Err(Error::Conflict);}
        let item=versions.into_iter().max_by_key(|item|item.binding.operation_id.revision).ok_or(Error::Readback)?;
        latest.push(item.clone());
    }
    if items.iter().any(|item| !expected_ids.contains(&item.binding.operation_id.proposal_id.0)){return Err(Error::Conflict);}
    Ok(latest)
}
pub(super) fn proposals_terminal(plan:&RootPlan)->Result<bool,Error>{
    if !plan.state_dir.join("import-plan.json").try_exists()? {return Ok(false);}
    if !plan.legacy_home.join("migration-confirmations.db").try_exists()? {return Ok(false);}
    Ok(proposals(plan)?.iter().all(|p|matches!(p.status,ConfirmationStatus::Applied|ConfirmationStatus::Rejected|ConfirmationStatus::ExpiredRejected)))
}
pub(super) fn ready(plan:&RootPlan)->Result<bool,Error>{
    if !plan.state_dir.join("import-plan.json").try_exists()? || !plan.legacy_home.join("migration-confirmations.db").try_exists()? {return Ok(false);}
    let items=proposals(plan)?;
    Ok(items.iter().all(|p|matches!(p.status,ConfirmationStatus::Applied|ConfirmationStatus::Rejected|ConfirmationStatus::ExpiredRejected))
        || items.iter().any(|p|p.category==ConfirmationCategory::Employee && matches!(p.status,ConfirmationStatus::Applied|ConfirmationStatus::Rejected)))
}
pub(super) fn terminal(plan:&RootPlan)->Result<bool,Error>{
    if !proposals_terminal(plan)? {return Ok(false);}
    let receipt=plan.state_dir.join("materialization-receipt.json");
    if !receipt.try_exists()? {return Ok(false);}
    let value:ImportReceipt=serde_json::from_slice(&read_private(&receipt,4096,0)?)?;
    if !value.complete || !value.archived_complete {return Ok(false);}
    let path=plan.legacy_home.join("migration-materialization.db");
    if !path.try_exists()? {return Ok(false);}
    path_metadata(&path,false,plan.gateway_uid)?;
    let grants=manifest(plan)?.grants.into_iter().map(|g|g.grant).collect::<Vec<_>>();
    MaterializationStore::open_read_only(&path)?.all_terminal(&grants)
}
fn agent(item:&ConfirmationItem)->Result<&str,Error>{
    item.fields.iter().find_map(|f|match (&*f.name,&f.value){("employee_id",ProposalValue::Text{value})=>Some(value.as_str()),_=>None}).ok_or(Error::Invalid)
}
/// Signed platform protocol extension: it MUST inspect the live immutable
/// registry, stopped employee processes, mount/quota and proposal allocation.
/// Missing older adapters fail closed rather than guessing UID or paths.
pub(super) fn installed_allocation(plan:&RootPlan,item:&ConfirmationItem)->Result<ConfirmedAllocation,Error>{
    let mut input=tempfile::NamedTempFile::new_in(&plan.state_dir)?;
    input.write_all(&serde_json::to_vec(&serde_json::json!({"version":1,"action":"materialization-allocation","plan":plan,"proposal":item}))?)?;
    let output=tempfile::NamedTempFile::new_in(&plan.state_dir)?;
    let mut child=Command::new(installed("duduclaw-migration-platform")?).arg("migration-v1")
        .env_clear().stdin(input.reopen()?).stdout(output.reopen()?).stderr(Stdio::null()).spawn()?;
    let deadline=Instant::now()+Duration::from_secs(30);
    loop {
        if output.as_file().metadata()?.len()>65536 || Instant::now()>deadline {let _=child.kill();let _=child.wait();return Err(Error::Limit);}
        if let Some(status)=child.try_wait()? {if !status.success(){return Err(Error::Readback);}break;}
        std::thread::sleep(Duration::from_millis(10));
    }
    #[derive(Deserialize)] #[serde(deny_unknown_fields)]
    struct Reply {version:u16,allocation:ConfirmedAllocation,stopped:bool,quota_verified:bool}
    let mut bytes=Vec::new();output.reopen()?.take(65537).read_to_end(&mut bytes)?;
    if bytes.len()>65536{return Err(Error::Limit);}
    let reply:Reply=serde_json::from_slice(&bytes)?;
    if reply.version!=1||!reply.stopped||!reply.quota_verified||reply.allocation.proposal!=item.binding{return Err(Error::Readback);}
    Ok(reply.allocation)
}
fn copied_name(operation:RequestId)->String {format!("import-{}",operation.0.iter().map(|b|format!("{b:02x}")).collect::<String>())}
fn copy<P:MigrationPlatform>(platform:&mut P,plan:&RootPlan,item:&ConfirmationItem,allocation:&ConfirmedAllocation,
    grant:&ImportGrant,records:&[crate::contract::ImportRecord],inventory:&root_sources::Inventory,
    bytes:&mut u64,files:&mut u32)->Result<Vec<BlobCopyReceipt>,Error>{
    let mut result=vec![];
    let root=root_sources::open_root(&plan.state_dir.join("legacy"))?;
    for record in records {
        let (blob,work)=match &record.content {
            ImportedContent::Attachment{blob}=>(blob,false),ImportedContent::WorkProduct{blob}=>(blob,true),_=>continue,
        };
        let source=inventory.sources.iter().find(|source|root_sources::source_id(plan.migration,source)==grant.source_id).ok_or(Error::Readback)?;
        if blob.source_handle!=blob_handle(plan.migration,&grant.source_id) {return Err(Error::Readback);}
        let destination=if work {allocation.workspace.clone()} else {plan.legacy_home.join("agents").join(&allocation.agent).join("attachments")};
        let uid=if work {allocation.uid}else{plan.gateway_uid};
        // The platform stops all employee execution before giving this allocation.
        // Source paths come only from the sealed root inventory, never a label.
        if !work && !destination.try_exists()? {
            let parent=destination.parent().ok_or(Error::Invalid)?;
            path_metadata(parent,true,plan.gateway_uid)?;
            fs::create_dir(&destination)?;fs::set_permissions(&destination,fs::Permissions::from_mode(0o700))?;
            let dir=root_sources::open_root(&destination)?;
            rustix::fs::fchown(&dir,Some(rustix::process::Uid::from_raw(uid)),Some(rustix::process::Gid::from_raw(plan.gateway_gid))).map_err(|_|Error::Storage)?;
            sync_parent(&destination)?;
        }
        path_metadata(&destination,true,uid)?;
        let staging=destination.join(".migration-copy");
        use std::os::unix::fs::DirBuilderExt;
        match fs::DirBuilder::new().mode(0o700).create(&staging) {Ok(())=>{sync_parent(&staging)?;},Err(e) if e.kind()==std::io::ErrorKind::AlreadyExists=>{},Err(e)=>return Err(e.into())}
        if path_metadata(&staging,true,0)?.mode()&0o7777!=0o700{return Err(Error::Readback);}
        let operation=blob_handle(plan.migration,&format!("{}:{}:{}",grant.source_id,record.source_record_id,serde_json::to_string(allocation)?));
        // Reconstruct total reservations from the sealed inventory, not current
        // free-space heuristics; replay validates the existing target first.
        if *files>=100_000 || blob.size_bytes>allocation.quota_bytes.saturating_sub(*bytes){return Err(Error::Limit);}
        let source_path=source.relative.to_str().ok_or(Error::Invalid)?;
        let relative=if work {source_path.strip_prefix(&format!("agents/{}/",allocation.agent)).ok_or(Error::Invalid)?}else{source_path};
        let work_root=if work {Some(root_sources::open_root(&plan.state_dir.join("legacy/agents").join(&allocation.agent))?)}else{None};
        let name=crate::import_blobs::copy_blob(if work{crate::import_blobs::BlobDestination::WorkProduct}else{crate::import_blobs::BlobDestination::Attachment},
            work_root.as_ref().unwrap_or(&root),relative,&root_sources::open_root(&staging)?,&root_sources::open_root(&destination)?,
            operation,blob,uid,allocation.quota_bytes.saturating_sub(*bytes),100_000-*files)?;
        if name!=copied_name(operation)||platform.allocation(plan,item)?!=*allocation{return Err(Error::Readback);}
        *bytes=bytes.checked_add(blob.size_bytes).ok_or(Error::Limit)?;*files+=1;
        let receipt=BlobCopyReceipt{source_handle:blob.source_handle,destination:destination.join(name),sha256:blob.sha256,
            size_bytes:blob.size_bytes,allocation_digest:allocation.allocation_digest};
        let receipts=plan.state_dir.join("blob-receipts");private_dir(&receipts)?;
        publish(&receipts.join(copied_name(operation)),&serde_json::to_vec(&receipt)?,0,0,false)?;
        result.push(receipt);
    }
    Ok(result)
}
pub(super) fn run<P:MigrationPlatform>(platform:&mut P,op:StageOperation,plan:&RootPlan)->Result<(),Error>{
    if !ready(plan)? {return Err(Error::AwaitingConfirmation);}
    let items=proposals(plan)?;
    // Platform owns stopping the ordinary gateway and all executions. Its
    // allocation readbacks must continue to prove this until copying finishes.
    platform.perform(op,plan)?;
    let inventory:root_sources::Inventory=serde_json::from_slice(&read_private(&plan.state_dir.join("source-inventory.json"),MAX_MANIFEST_BYTES,0)?)?;
    let mut value=manifest(plan)?;value.completed=false;
    let previous_path=plan.state_dir.join("materialization-plan.json");
    let previous:Option<ImportManifest>=if previous_path.try_exists()? {
        let previous:ImportManifest=serde_json::from_slice(&read_private(&previous_path,MAX_MANIFEST_BYTES,0)?)?;
        previous.validate()?;
        if previous.migration_id!=value.migration_id || previous.home!=value.home || previous.gateway_uid!=value.gateway_uid
            || previous.pending_proposals!=value.pending_proposals || previous.grants.len()!=value.grants.len()
            || previous.grants.iter().zip(&value.grants).any(|(a,b)|a.grant!=b.grant){return Err(Error::Conflict);}
        Some(previous)
    }else{None};
    let ledger=MaterializationStore::open_read_only(&plan.legacy_home.join("migration-materialization.db"))?;
    let mut quota=std::collections::BTreeMap::<String,(u64,u32)>::new();
    for (index,entry) in value.grants.iter_mut().enumerate() {
        // A completed record is owned by the product now. Do not overwrite or
        // recopy it on a later employee's batch after users have edited it.
        if let Some(prior)=previous.as_ref().map(|v|&v.grants[index]) {
            if prior.materialization.is_some() && ledger.all_terminal(std::slice::from_ref(&entry.grant))? {
                if let Some(MaterializationDecision::Confirmed{allocation})=&prior.materialization {
                    let item=items.iter().find(|item|item.binding==allocation.proposal && item.status==ConfirmationStatus::Applied).ok_or(Error::Conflict)?;
                    if platform.allocation(plan,item)?!=*allocation{return Err(Error::Conflict);}
                    let (bytes,files)=quota.entry(allocation.agent.clone()).or_default();
                    for receipt in &prior.blob_receipts {
                        *bytes=bytes.checked_add(receipt.size_bytes).ok_or(Error::Limit)?;*files=files.checked_add(1).ok_or(Error::Limit)?;
                    }
                    if *bytes>allocation.quota_bytes || *files>100_000{return Err(Error::Limit);}
                }
                *entry=prior.clone();continue;
            }
        }
        let stream=read_private(&plan.state_dir.join("streams").join(&entry.grant.source_id),entry.grant.stream_bytes,0)?;
        let staged=StagedImport::receive(entry.grant.clone(),stream.as_slice(),true)?;
        let decision=if let Some(employee)=entry.grant.employee {
            let candidates=items.iter().filter(|item|item.category==ConfirmationCategory::Employee
                && agent(item).ok().is_some_and(|a|root_sources::employee_id(plan.migration,a)==employee)).collect::<Vec<_>>();
            if candidates.len()!=1{return Err(Error::Readback);}
            let item=candidates[0];
            if item.status==ConfirmationStatus::Rejected {
                MaterializationDecision::Rejected{proposal:item.binding}
            } else if item.status==ConfirmationStatus::Applied {
                let allocation=platform.allocation(plan,item)?;
                if allocation.agent!=agent(item)? || allocation.proposal!=item.binding || allocation.employee!=employee {return Err(Error::Readback);}
                let decision=MaterializationDecision::Confirmed{allocation:allocation.clone()};decision.validate(&entry.grant)?;
                // Commit allocation identity before the first filesystem effect.
                // Any changed UID, mount, quota or proposal refuses resume.
                let intents=plan.state_dir.join("materialization-intents");private_dir(&intents)?;
                let key=blob_handle(plan.migration,&serde_json::to_string(&allocation.proposal)?);
                publish(&intents.join(copied_name(key)),&serde_json::to_vec(&allocation)?,0,0,false)?;
                let (bytes,files)=quota.entry(allocation.agent.clone()).or_default();
                entry.blob_receipts=copy(platform,plan,item,&allocation,&entry.grant,staged.records(),&inventory,bytes,files)?;
                entry.target_agent=Some(allocation.agent.clone());decision
            }else{
                // Other employees remain inert while confirmed employees land.
                continue;
            }
        }else{
            if !matches!(entry.grant.class,ContentClass::TaskHistory|ContentClass::LegacyAuditArchive){return Err(Error::Unavailable);}
            MaterializationDecision::Retained{policy:"administrator_read_only_history".into()}
        };
        entry.materialization=Some(decision);
    }
    value.validate()?;
    // Seal the complete plan before exposing any product writes. On crash a
    // different allocation cannot reuse the gateway's per-record durable intent.
    publish(&plan.state_dir.join("materialization-plan.json"),&serde_json::to_vec(&value)?,0,0,true)?;
    start_import(plan,&value)?;
    let result=(||{
        for entry in &value.grants {
            // Previously landed content belongs to the product and may have
            // been edited or deleted since a partial batch. Its terminal receipt
            // is sufficient; only unfinished streams are replayed/read back.
            if ledger.all_terminal(std::slice::from_ref(&entry.grant))? {continue;}
            let bytes=read_private(&plan.state_dir.join("streams").join(&entry.grant.source_id),entry.grant.stream_bytes,0)?;
            send_import_stream(entry.grant.clone(),bytes.as_slice(),true,plan.gateway_uid)?;
        }
        let status=import_status(plan)?;
        if !status.archived_complete||status.archived_streams!=value.grants.len() as u64
            ||status.archived_records!=value.grants.iter().map(|g|g.grant.records).sum::<u64>() {return Err(Error::Readback);}
        value.completed=true;runtime_manifest(&value,plan.gateway_gid)?;
        control(plan,&["stop",SOCKET_UNIT,SERVICE_UNIT])?;
        if !inactive(plan)? {return Err(Error::Readback);}
        // Restore the dashboard after each partial batch so the administrator
        // can confirm remaining employees; this does not declare completion.
        let resume=StageOperation{migration:plan.migration,target:MigrationState::AwaitingAdministrator};
        platform.perform(resume,plan)?;
        platform.readback(resume,plan)?.ok_or(Error::Readback)?.validate(resume.target)?;
        // A durable batch receipt also proves that dashboard recovery finished;
        // a crash before this point must repeat the resume/readback operation.
        publish(&plan.state_dir.join("materialization-receipt.json"),&serde_json::to_vec(&status)?,0,0,true)?;
        if !status.complete || !proposals_terminal(plan)? {return Err(Error::AwaitingConfirmation);}
        Ok(())
    })();
    if result.is_err(){let _=control(plan,&["stop",SOCKET_UNIT,SERVICE_UNIT]);}
    result
}
