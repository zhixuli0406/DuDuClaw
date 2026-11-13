//! Installed root coordinator for the G-owned stages. F/B/H own signed activation
//! and final platform readback; their adapter is checked before stopping services.
use std::{fs::{self},io::{Read,Write},path::{Path,PathBuf},os::{fd::AsRawFd,unix::{fs::{MetadataExt,PermissionsExt},net::UnixStream}},process::{Command,Stdio},time::{Duration,Instant}};
use serde::{Serialize,Deserialize};
use sha2::{Digest,Sha256};
use super::{RootMigrationIntegration,RootPlan,native_stop::NativeIntegration,Error,Evidence,StageOperation,MigrationState,
    private_dir,publish,read_private,trusted_parents,path_metadata,sync_parent};
use crate::{root_sources::{self,Source},import_manifest::{ImportManifest,ManifestGrant,PendingProposal,IMPORT_MANIFEST,DURABLE_IMPORT_MANIFEST,MAX_MANIFEST_BYTES},
    import_receiver::{ImportGrant,ContentClass,StagedImport,send_import_stream},ids::Sha256Digest};

const SOCKET_UNIT:&str="duduclaw-migrate-import.socket";
const SERVICE_UNIT:&str="duduclaw-migrate-import.service";
const INSTALLED:&str="/usr/lib/duduclaw/current";

/// The signed installer supplies this narrow integration with packages F/B/H.
/// Evidence must come from live product readback, never a copied journal receipt.
/// A missing platform is detected before any outage. It cannot be replaced by a
/// dashboard parameter, executable from the legacy tree or arbitrary shell text.
pub trait MigrationPlatform {
    fn preflight(&mut self,plan:&RootPlan)->Result<(),Error>;
    /// Must return a live, stopped allocation bound to the approved operation.
    fn allocation(&mut self, _plan:&RootPlan, _item:&crate::contract::ConfirmationItem)
        ->Result<crate::materialization::ConfirmedAllocation,Error>{Err(Error::Unavailable)}
    fn perform(&mut self,op:StageOperation,plan:&RootPlan)->Result<(),Error>;
    fn readback(&mut self,op:StageOperation,plan:&RootPlan)->Result<Option<Evidence>,Error>;
}
/// Fixed installed adapter protocol. argv is fixed; bounded JSON uses stdin and
/// stdout. It is deliberately separate from the legacy parser and never handles
/// old DBs. Package F installs this adapter in its verified generation.
pub struct InstalledPlatform;
#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct PlatformCall<'a> { version:u16, action:&'a str, operation:Option<StageOperation>,plan:&'a RootPlan }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlatformReply { version:u16,ready:bool,evidence:Option<Evidence> }
#[path = "root_materialization.rs"]
mod materialize;
fn installed(name:&str)->Result<PathBuf,Error> {
    let path=fs::canonicalize(Path::new(INSTALLED).join(name))?;
    trusted_parents(&path)?;
    if path_metadata(&path,false,0)?.mode()&0o111==0 {return Err(Error::Invalid);}
    Ok(path)
}
fn platform_call(action:&str,operation:Option<StageOperation>,plan:&RootPlan)->Result<PlatformReply,Error> {
    let mut input=tempfile::NamedTempFile::new_in(&plan.state_dir)?;
    input.write_all(&serde_json::to_vec(&PlatformCall{version:1,action,operation,plan})?)?;
    let output=tempfile::NamedTempFile::new_in(&plan.state_dir)?;
    let mut child=Command::new(installed("duduclaw-migration-platform")?).arg("migration-v1")
        .env_clear().stdin(input.reopen()?).stdout(output.reopen()?).stderr(Stdio::null()).spawn()?;
    let deadline=Instant::now()+Duration::from_secs(30);
    loop {
        if output.as_file().metadata()?.len()>65536 || Instant::now()>deadline {let _=child.kill();let _=child.wait();return Err(Error::Limit);}
        if let Some(status)=child.try_wait()? {if !status.success(){return Err(Error::Readback);}break;}
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut bytes=Vec::new();output.reopen()?.take(65537).read_to_end(&mut bytes)?;
    if bytes.len()>65536 {return Err(Error::Limit);}
    let reply:PlatformReply=serde_json::from_slice(&bytes)?;
    if reply.version!=1 || !reply.ready {return Err(Error::Unavailable);}Ok(reply)
}
impl MigrationPlatform for InstalledPlatform {
    fn allocation(&mut self,plan:&RootPlan,item:&crate::contract::ConfirmationItem)->Result<crate::materialization::ConfirmedAllocation,Error>{
        materialize::installed_allocation(plan,item)
    }
    fn preflight(&mut self,plan:&RootPlan)->Result<(),Error>{platform_call("preflight",None,plan)?;Ok(())}
    fn perform(&mut self,op:StageOperation,plan:&RootPlan)->Result<(),Error>{platform_call("perform",Some(op),plan)?;Ok(())}
    fn readback(&mut self,op:StageOperation,plan:&RootPlan)->Result<Option<Evidence>,Error>{Ok(platform_call("readback",Some(op),plan)?.evidence)}
}

pub struct InstalledIntegration<P=InstalledPlatform> { platform:P }
impl Default for InstalledIntegration<InstalledPlatform> {fn default()->Self{Self{platform:InstalledPlatform}}}
impl<P:MigrationPlatform> InstalledIntegration<P> {pub fn new(platform:P)->Self{Self{platform}}}
#[derive(Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportReceipt { complete:bool,archived_complete:bool,proposal_count:u64,pending_count:u64,expired_approvals:u64,authority_inactive:bool,archived_streams:u64,archived_records:u64,imported_records:u64 }
fn control(plan:&RootPlan,args:&[&str])->Result<String,Error>{super::native_stop::checked(plan,"systemctl",args)}
fn inactive(plan:&RootPlan)->Result<bool,Error>{
    for unit in [SERVICE_UNIT,SOCKET_UNIT] {
        let value=control(plan,&["show",unit,"--property=ActiveState","--value"])?;
        if !matches!(value.trim(),"inactive"|"failed") {return Ok(false);}
    }Ok(true)
}
fn manifest(plan:&RootPlan)->Result<ImportManifest,Error>{
    let value:ImportManifest=serde_json::from_slice(&read_private(&plan.state_dir.join("import-plan.json"),MAX_MANIFEST_BYTES,0)?)?;
    value.validate()?;if value.migration_id!=plan.migration || value.home!=plan.legacy_home || value.gateway_uid!=plan.gateway_uid {return Err(Error::Conflict);}Ok(value)
}
fn publish_boot_manifest(path:&Path,value:&ImportManifest,gateway_gid:u32)->Result<(),Error>{
    let dir=path.parent().ok_or(Error::Invalid)?;
    trusted_parents(dir)?;
    match fs::create_dir(dir){Ok(())=>fs::set_permissions(dir,fs::Permissions::from_mode(0o755))?,Err(e) if e.kind()==std::io::ErrorKind::AlreadyExists=>{},Err(e)=>return Err(e.into())}
    path_metadata(dir,true,0)?;
    if dir.metadata()?.mode()&0o005!=0o005 {return Err(Error::Invalid);}
    let bytes=serde_json::to_vec(value)?;if bytes.len() as u64>MAX_MANIFEST_BYTES{return Err(Error::Limit);}
    // Avoid a chmod-after-rename window: the gateway sees either complete old or
    // complete new root-owned readable bytes.
    let mut stage=tempfile::NamedTempFile::new_in(dir)?;stage.write_all(&bytes)?;
    // Only the fresh gateway group may read old instruction/proposal text.
    // SAFETY: fchown operates on the owned staging file, never a path from old data.
    if unsafe {libc::fchown(stage.as_file().as_raw_fd(),0,gateway_gid)}!=0 {return Err(Error::Storage);}
    stage.as_file().set_permissions(fs::Permissions::from_mode(0o640))?;stage.as_file().sync_all()?;
    stage.persist(path).map_err(|_|Error::Storage)?;sync_parent(path)
}
fn runtime_manifest(value:&ImportManifest,gateway_gid:u32)->Result<(),Error>{
    // Persist before /run: a reboot can never reopen ordinary admin bootstrap.
    publish_boot_manifest(Path::new(DURABLE_IMPORT_MANIFEST),value,gateway_gid)?;
    publish_boot_manifest(Path::new(IMPORT_MANIFEST),value,gateway_gid)
}
fn ensure_boot_fence(plan:&RootPlan)->Result<(),Error>{
    let path=Path::new(DURABLE_IMPORT_MANIFEST);
    match fs::symlink_metadata(path) {
        Ok(meta)=>{
            trusted_parents(path)?;path_metadata(path,false,0)?;
            if meta.len()>MAX_MANIFEST_BYTES || meta.mode()&0o7777!=0o640 {return Err(Error::Invalid);}
            let bytes=fs::read(path)?;
            let existing:ImportManifest=serde_json::from_slice(&bytes)?;existing.validate()?;
            if existing.migration_id!=plan.migration || existing.home!=plan.legacy_home
                || existing.gateway_uid!=plan.gateway_uid {return Err(Error::Conflict);}
            Ok(())
        }
        Err(error) if error.kind()==std::io::ErrorKind::NotFound=>{
            let value=ImportManifest{schema_version:1,migration_id:plan.migration,home:plan.legacy_home.clone(),
                gateway_uid:plan.gateway_uid,grants:vec![],pending_proposals:vec![],completed:false};
            publish_boot_manifest(path,&value,plan.gateway_gid)
        }
        Err(error)=>Err(error.into()),
    }
}
fn worker_shape(source:&Source)->Result<(String,String),Error>{
    let basename=source.relative.file_name().and_then(|s|s.to_str()).ok_or(Error::Invalid)?;
    if source.kind!="authority" {return Ok((source.kind.clone(),basename.into()));}
    let parts:Vec<_>=source.relative.iter().map(|p|p.to_str().ok_or(Error::Invalid)).collect::<Result<_,_>>()?;
    let tail=if source.agent.is_some(){parts.get(2..).ok_or(Error::Invalid)?}else{parts.as_slice()};
    if matches!(tail,["cron_tasks.db"]){return Ok(("authority_cron_sqlite".into(),"cron_tasks.db".into()));}
    if matches!(tail,["cron_tasks.jsonl"]){return Ok(("authority_cron_jsonl".into(),"cron_tasks.jsonl".into()));}
    if matches!(tail,["workflow.db"]){return Ok(("authority_workflow_sqlite".into(),basename.into()));}
    if matches!(tail,["autopilot.db"]){return Ok(("authority_autopilot_sqlite".into(),basename.into()));}
    if matches!(tail,["reminders.jsonl"]){return Ok(("authority_reminders_jsonl".into(),basename.into()));}
    if matches!(tail,["KILLSWITCH.toml"|"USER.md"]|["redaction","profiles",..]|["shared","wiki",".scope.toml"]|["shared","wiki","departments",_,".scope.toml"]){
        return Ok(("authority_notice".into(),"notice:policy".into()));
    }
    if source.agent.is_none() && matches!(tail,["skills"|"SKILLS",..]|["shared","skills",..]) {
        return Ok(("authority_notice".into(),"notice:skill".into()));
    }
    if matches!(tail,["skills"|"SKILLS",_]) && source.relative.extension().is_some_and(|e|e=="md") {
        let name=source.relative.file_stem().and_then(|s|s.to_str()).ok_or(Error::Invalid)?;
        return Ok(("authority_skill".into(),name.into()));
    }
    if matches!(tail,["skills"|"SKILLS",..]) && !matches!(tail,["skills"|"SKILLS",_,"SKILL.md"]) {
        return Ok(("authority_notice".into(),"notice:skill".into()));
    }
    // Encrypted stores and old integrity baselines never become trusted state.
    // The worker emits a named re-entry obligation without reading their bytes.
    if matches!(tail,["secrets","website_credentials.enc"]){return Ok(("authority_notice".into(),"website_credentials.enc".into()));}
    if matches!(tail,[".soul_hash"]|[".soul_hashes",..]|[".soul_history",..]|["soul_hashes",..]){
        return Ok(("authority_notice".into(),"soul_baseline".into()));
    }
    if matches!(tail,["remote_mcp","servers.json"]|["person_credentials","store.json"]){
        return Ok(("authority_json".into(),tail.join("/")));
    }
    let notice=match tail {
        ["hooks",..]=>Some("notice:hook"),["policies",..]=>Some("notice:policy"),
        ["connectors",..]=>Some("notice:connector"),["data-sources",..]=>Some("notice:data_source"),
        ["workflows"|"responsibilities"|"autopilot",..]=>Some("notice:schedule"),
        _=>None,
    };
    if let Some(title)=notice{return Ok(("authority_notice".into(),title.into()));}
    let kind=match basename {
        "users.db"=>"authority_users_sqlite",
        "tasks.db"=>"authority_tasks_sqlite",
        "approvals.db"=>"authority_approvals_sqlite",
        // RM-134 (v1.82): hashed MCP key store; every live key is a re-issue obligation.
        "mcp_keys.db"=>"authority_mcp_keys_sqlite",
        "sessions.db"=>"authority_sessions_sqlite",
        "SOUL.md"|"IDENTITY.md"|"INSTRUCTIONS.md"|"CONTRACT.toml"=>"authority_instructions",
        "SKILL.md"=>"authority_skill",
        _ if source.relative.extension().is_some_and(|e|e=="toml")=>"authority_toml",
        _ if source.relative.extension().is_some_and(|e|e=="json")=>"authority_json",
        _=>return Err(Error::Invalid),
    };
    let title=if kind=="authority_skill" {source.relative.parent().and_then(Path::file_name).and_then(|v|v.to_str()).ok_or(Error::Invalid)?}else{basename};
    Ok((kind.into(),title.into()))
}
fn class(kind:&str)->Result<ContentClass,Error>{Ok(match kind{
    "memory_text"|"memory_sqlite"=>ContentClass::Memory,"sessions_sqlite"|"conversation_text"=>ContentClass::Conversation,
    "attachment"=>ContentClass::Attachment,"work_product"=>ContentClass::WorkProduct,
    "wiki_text"=>ContentClass::WikiPage,"task_archive"=>ContentClass::TaskHistory,"audit_archive"=>ContentClass::LegacyAuditArchive,_=>return Err(Error::Invalid),
})}
fn prepare_sources(plan:&RootPlan)->Result<(),Error>{
    if plan.state_dir.join("import-plan.json").try_exists()? {materialize::verify_inventory(plan)?;return Ok(());}
    let root=root_sources::open_root(&plan.state_dir.join("legacy"))?;
    let inventory=root_sources::inventory(&root)?;
    let streams=plan.state_dir.join("streams");private_dir(&streams)?;
    let mut result=ImportManifest{schema_version:1,migration_id:plan.migration,home:plan.legacy_home.clone(),gateway_uid:plan.gateway_uid,
        grants:vec![],pending_proposals:vec![],completed:false};
    // Empty departments are represented by directory names, not a legacy file
    // for the parser. A validated label is still only an inert proposal here.
    for department in &inventory.departments {
        let source=Source{relative:Path::new("shared/wiki/departments").join(department),kind:"department_directory".into(),agent:None};
        result.pending_proposals.push(PendingProposal{migration_id:plan.migration,source_id:root_sources::source_id(plan.migration,&source),sequence:0,
            category:crate::contract::ConfirmationCategory::Department,fields:vec![crate::contract::ProposalField{name:"name".into(),
                value:crate::contract::ProposalValue::Text{value:department.clone()}}]});
    }
    result.validate()?;
    let worker=installed("duduclaw-import")?;let mut total=0u64;let mut authority_bytes=0u64;
    let mut rejected=Vec::<serde_json::Value>::new();
    for source in &inventory.sources {
        let id=root_sources::source_id(plan.migration,source);let (kind,title)=worker_shape(source)?;
        let file=root_sources::open_source(&root,&source.relative)?;
        let wal_path=source.relative.with_file_name(format!("{}-wal",source.relative.file_name().and_then(|s|s.to_str()).ok_or(Error::Invalid)?));
        let sqlite=matches!(kind.as_str(),"sessions_sqlite"|"memory_sqlite"|"authority_users_sqlite"|"authority_tasks_sqlite"|"authority_approvals_sqlite"|"authority_sessions_sqlite"|"authority_cron_sqlite"|"authority_workflow_sqlite"|"authority_autopilot_sqlite"|"authority_mcp_keys_sqlite");
        // Only ENOENT means no WAL. A link, permission or traversal failure is
        // never silently treated as a clean database.
        let wal=if sqlite {
            match rustix::fs::statat(&root,&wal_path,rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
                Ok(_)=>Some(root_sources::open_source(&root,&wal_path)?),
                Err(rustix::io::Errno::NOENT)=>None,Err(_)=>return Err(Error::Storage),
            }
        }else{None};
        let request=serde_json::json!({"version":1,"migration_id":plan.migration,"employee":source.agent.as_ref().map(|a|root_sources::employee_id(plan.migration,a)),
            "source_id":id,"kind":kind,"title":title,"legacy_agent":source.agent,"has_wal":wal.is_some(),"blob_handle":if matches!(source.kind.as_str(),"attachment"|"work_product"){Some(materialize::blob_handle(plan.migration,&id))}else{None}});
        let mut fds=vec![&file];if let Some(wal)=&wal{fds.push(wal);}
        let bytes=match crate::import_launcher::launch_protected(&worker,&serde_json::to_vec(&request)?,&fds,&[&plan.legacy_home,&plan.state_dir]) {
            Ok(bytes)=>bytes,
            Err(Error::SourceRejected)=>{
                rejected.push(serde_json::json!({"source_id":id,"reason":"unsupported_or_invalid_legacy_source"}));
                continue;
            }
            Err(error)=>return Err(error),
        };
        total=total.checked_add(bytes.len() as u64).ok_or(Error::Limit)?;
        if total>plan.snapshot_encoded_limit {return Err(Error::Limit);}
        if source.kind=="authority" {
            authority_bytes=authority_bytes.checked_add(bytes.len() as u64).ok_or(Error::Limit)?;
            if authority_bytes>MAX_MANIFEST_BYTES{return Err(Error::Limit);}
            if bytes.last().is_some_and(|b|*b!=b'\n'){return Err(Error::Invalid);}
            for (sequence,line) in bytes.split_inclusive(|b|*b==b'\n').enumerate(){
                if line.len()>crate::import_manifest::MAX_AUTHORITY_RECORD_BYTES+1 {return Err(Error::Limit);}
                let proposal:PendingProposal=serde_json::from_slice(line)?;
                if proposal.migration_id!=plan.migration || proposal.source_id!=id || proposal.sequence!=sequence as u64 {return Err(Error::Invalid);}
                result.pending_proposals.push(proposal);
            }
        }else{
            let grant=ImportGrant{migration_id:plan.migration,employee:source.agent.as_ref().map(|a|root_sources::employee_id(plan.migration,a)),
                source_id:id.clone(),class:class(&source.kind)?,stream_bytes:bytes.len() as u64,
                records:bytes.split_inclusive(|b|*b==b'\n').count() as u64,stream_sha256:Sha256Digest(Sha256::digest(&bytes).into())};
            StagedImport::receive(grant.clone(),bytes.as_slice(),true)?;
            publish(&streams.join(&id),&bytes,0,0,false)?;
            result.grants.push(ManifestGrant{grant,target_agent:None,materialization:None,blob_receipts:vec![]});
        }
        result.validate()?;
    }
    let employees=result.pending_proposals.iter().filter(|p|p.category==crate::contract::ConfirmationCategory::Employee).count();
    if employees>crate::contract::DEFAULT_EMPLOYEE_PROPOSAL_LIMIT as usize{return Err(Error::Limit);}
    let encoded=serde_json::to_vec(&result)?;if encoded.len() as u64>MAX_MANIFEST_BYTES{return Err(Error::Limit);}
    publish(&plan.state_dir.join("source-inventory.json"),&serde_json::to_vec(&inventory)?,0,0,false)?;
    publish(&plan.state_dir.join("source-rejections.json"),&serde_json::to_vec(&rejected)?,0,0,false)?;
    // Version 2 inventories include attachment and work-product streams. Old
    // archive-only manifests cannot authorize deletion of unenumerated blobs.
    materialize::seal_inventory(plan,&encoded)?;
    publish(&plan.state_dir.join("import-plan.json"),&encoded,0,0,false)
}
fn unit_path(value:&Path)->Result<&str,Error>{let s=value.to_str().ok_or(Error::Invalid)?;
    if !s.bytes().all(|b|b.is_ascii_alphanumeric()||b"/_-.".contains(&b)){return Err(Error::Invalid);}Ok(s)}
fn import_layout(plan:&RootPlan)->Result<(),Error>{
    unit_path(&plan.legacy_home)?;unit_path(&plan.state_dir)?;
    // ProtectHome=yes masks these parents; ReadWritePaths cannot undo it. Reject
    // such plans before stopping the old service, not halfway through import.
    if ["/home","/root","/run/user"].iter().any(|path|plan.legacy_home.starts_with(path)) {
        return Err(Error::Invalid);
    }
    Ok(())
}
fn start_import(plan:&RootPlan,value:&ImportManifest)->Result<(),Error>{
    runtime_manifest(value,plan.gateway_gid)?;
    let binary=installed("duduclaw")?;
    let home=unit_path(&plan.legacy_home)?;let binary=unit_path(&binary)?;
    let service=format!("[Unit]\nDescription=DuDuClaw migration import only\n[Service]\nType=simple\nUser={}\nGroup={}\nExecStart={binary} run --yes\nEnvironment=DUDUCLAW_HOME={home}\nEnvironment=DUDUCLAW_MIGRATION_IMPORT_ONLY=1\nNoNewPrivileges=yes\nPrivateNetwork=yes\nProtectSystem=strict\nProtectHome=yes\nReadWritePaths={home}\nInaccessiblePaths={}\nStandardOutput=null\nStandardError=null\nRestart=no\n",plan.gateway_uid,plan.gateway_gid,unit_path(&plan.state_dir)?);
    let socket=format!("[Unit]\nDescription=DuDuClaw root migration endpoint\n[Socket]\nListenStream={}\nSocketUser={}\nSocketGroup={}\nSocketMode=0600\nRemoveOnStop=yes\nService={SERVICE_UNIT}\n",crate::ids::IMPORT_SOCKET,plan.gateway_uid,plan.gateway_gid);
    for (name,text) in [(SERVICE_UNIT,service),(SOCKET_UNIT,socket)] {
        let path=Path::new("/run/systemd/system").join(name);trusted_parents(&path)?;publish(&path,text.as_bytes(),0,0,true)?;
    }
    control(plan,&["daemon-reload"])?;control(plan,&["start",SOCKET_UNIT])?;Ok(())
}
fn import_status(plan:&RootPlan)->Result<ImportReceipt,Error>{
    let mut stream=UnixStream::connect(crate::ids::IMPORT_SOCKET)?;
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    crate::import_receiver::authenticate_gateway_stream(&mut stream,plan.gateway_uid)?;
    stream.write_all(b"{\"status\":true}\n")?;stream.shutdown(std::net::Shutdown::Write)?;
    let mut bytes=Vec::new();stream.take(4097).read_to_end(&mut bytes)?;if bytes.len()>4096{return Err(Error::Limit);}
    Ok(serde_json::from_slice(&bytes)?)
}
fn import(plan:&RootPlan)->Result<(),Error>{
    let mut value=manifest(plan)?;
    // Interrupted import reuses the same source/grant digests and gateway ledger.
    start_import(plan,&value)?;
    let result=(||{
        for item in &value.grants {
            let bytes=read_private(&plan.state_dir.join("streams").join(&item.grant.source_id),item.grant.stream_bytes,0)?;
            send_import_stream(item.grant.clone(),bytes.as_slice(),true,plan.gateway_uid)?;
        }
        let status=import_status(plan)?;
        if !status.archived_complete||!status.authority_inactive||status.proposal_count!=value.pending_proposals.len() as u64
            ||status.expired_approvals!=value.pending_proposals.iter().filter(|p|p.category==crate::contract::ConfirmationCategory::LegacyApproval).count() as u64
            ||status.pending_count+status.expired_approvals!=status.proposal_count
            ||status.archived_streams!=value.grants.len() as u64||status.archived_records!=value.grants.iter().map(|g|g.grant.records).sum::<u64>() {return Err(Error::Readback);}
        value.completed=true;runtime_manifest(&value,plan.gateway_gid)?;
        control(plan,&["stop",SOCKET_UNIT,SERVICE_UNIT])?;
        if !inactive(plan)?{return Err(Error::Readback);}
        publish(&plan.state_dir.join("import-receipt.json"),&serde_json::to_vec(&status)?,0,0,false)
    })();
    if result.is_err(){let _=control(plan,&["stop",SOCKET_UNIT,SERVICE_UNIT]);}
    result
}
impl<P:MigrationPlatform> RootMigrationIntegration for InstalledIntegration<P> {
    fn materialization_ready(&mut self,plan:&RootPlan)->Result<bool,Error>{materialize::ready(plan)}
    fn retention_ready(&mut self,plan:&RootPlan)->Result<bool,Error>{materialize::terminal(plan)}
    fn preflight(&mut self,plan:&RootPlan)->Result<(),Error>{
        import_layout(plan)?;installed("duduclaw-import")?;installed("duduclaw")?;
        self.platform.preflight(plan)?;
        ensure_boot_fence(plan)
    }
    fn gateway_denied_legacy(&mut self,plan:&RootPlan)->Result<bool,Error>{NativeIntegration.gateway_denied_legacy(plan)}
    fn perform(&mut self,op:StageOperation,plan:&RootPlan)->Result<(),Error>{match op.target{
        MigrationState::OldEnvironmentStopped=>NativeIntegration.perform(op,plan),
        MigrationState::TokenRevocationRecorded=>{
            // The snapshot contains no live credentials in the new home. External
            // revocations/rotation remain explicit obligations, not claims of success.
            let report=serde_json::json!({"migration":plan.migration,"cli_tokens_copied":false,"external_credentials":"reconnect_required","provider_and_channel_secrets":"manual_rotation_required"});
            publish(&plan.state_dir.join("credential-obligations.json"),&serde_json::to_vec(&report)?,0,0,false)
        }
        MigrationState::EmployeeProposalsStaged=>prepare_sources(plan),
        MigrationState::ContentArchived=>import(plan),
        MigrationState::ContentImported=>materialize::run(&mut self.platform,op,plan),
        MigrationState::ConfirmationReady=>if self.readback(op,plan)?.is_some(){Ok(())}else{Err(Error::Readback)},
        MigrationState::Complete=>{if !materialize::terminal(plan)? {return Err(Error::Readback);} self.platform.perform(op,plan)},
        MigrationState::IsolationVerified|MigrationState::AwaitingAdministrator=>self.platform.perform(op,plan),
        _=>Err(Error::Invalid),
    }}
    fn readback(&mut self,op:StageOperation,plan:&RootPlan)->Result<Option<Evidence>,Error>{match op.target{
        MigrationState::OldEnvironmentStopped=>NativeIntegration.readback(op,plan),
        MigrationState::TokenRevocationRecorded=>{
            let path=plan.state_dir.join("credential-obligations.json");if !path.try_exists()?{return Ok(None);}
            let actual:serde_json::Value=serde_json::from_slice(&read_private(&path,4096,0)?)?;
            if actual["migration"]!=serde_json::to_value(plan.migration)?||actual["cli_tokens_copied"]!=false
                ||actual["external_credentials"]!="reconnect_required"||actual["provider_and_channel_secrets"]!="manual_rotation_required"{return Err(Error::Readback);}
            Ok(Some(Evidence::Revocations{obligations_recorded:true,reconnect_required:true,cli_tokens_absent:true}))
        }
        MigrationState::EmployeeProposalsStaged=>{
            if !plan.state_dir.join("import-plan.json").try_exists()?{return Ok(None);}
            let value=manifest(plan)?;let count=value.pending_proposals.iter().filter(|p|p.category==crate::contract::ConfirmationCategory::Employee).count() as u32;
            if plan.legacy_home.join("agents").try_exists()? {return Err(Error::Readback);}
            Ok(Some(Evidence::Employees{count,no_resources_allocated:true}))
        }
        MigrationState::ContentArchived|MigrationState::ConfirmationReady=>{
            let path=plan.state_dir.join("import-receipt.json");if !path.try_exists()?{return Ok(None);}
            let receipt:ImportReceipt=serde_json::from_slice(&read_private(&path,4096,0)?)?;
            if !receipt.archived_complete||!receipt.authority_inactive||!inactive(plan)?{return Err(Error::Readback);}
            Ok(Some(if op.target==MigrationState::ContentArchived {Evidence::Content{root_only_endpoint:true,source_labels_stamped:true,import_mode_stopped:true}}
                else{Evidence::Confirmation{authority_inactive:true,legacy_approvals_expired:true}}))
        }
        MigrationState::ContentImported=>if materialize::terminal(plan)? {Ok(Some(Evidence::Materialized{all_records_terminal:true}))}else{Ok(None)},
        MigrationState::Complete=>if materialize::terminal(plan)? {self.platform.readback(op,plan)}else{Ok(None)},
        MigrationState::IsolationVerified|MigrationState::AwaitingAdministrator=>self.platform.readback(op,plan),
        _=>Err(Error::Invalid),
    }}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migration_pipeline_home_visibility_is_checked_before_outage() {
        let mut plan=RootPlan {migration:crate::ids::RequestId([7;16]),state_dir:"/var/lib/duduclaw-migrate/test".into(),
            legacy_home:"/var/lib/duduclaw/.duduclaw".into(),root_key_path:"/etc/duduclaw/root.key".into(),
            gateway_uid:991,gateway_gid:991,snapshot_content_limit:1024,snapshot_encoded_limit:2048,expires_at_unix:1};
        assert!(import_layout(&plan).is_ok());
        for hidden in ["/home/old/.duduclaw","/root/.duduclaw","/run/user/991/duduclaw"] {
            plan.legacy_home=hidden.into();assert!(import_layout(&plan).is_err());
        }
        plan.legacy_home="/var/lib/home/duduclaw".into();assert!(import_layout(&plan).is_ok());
    }
    #[test]
    fn migration_pipeline_routes_authority_by_exact_components() {
        for (path,agent,kind,title) in [
            ("approvals.db",None,"authority_approvals_sqlite","approvals.db"),
            ("mcp_keys.db",None,"authority_mcp_keys_sqlite","mcp_keys.db"),
            ("remote_mcp/servers.json",None,"authority_json","remote_mcp/servers.json"),
            ("person_credentials/store.json",None,"authority_json","person_credentials/store.json"),
            ("secrets/website_credentials.enc",None,"authority_notice","website_credentials.enc"),
            ("agents/sales/.soul_hash",Some("sales"),"authority_notice","soul_baseline"),
            ("agents/sales/.soul_history/a.md",Some("sales"),"authority_notice","soul_baseline"),
            ("agents/sales/SKILLS/report/SKILL.md",Some("sales"),"authority_skill","report"),
            ("agents/sales/SKILLS/report.md",Some("sales"),"authority_skill","report"),
            ("agents/sales/SKILLS/report/scripts/run.sh",Some("sales"),"authority_notice","notice:skill"),
            ("agents/sales/SKILLS/report/assets/example.png",Some("sales"),"authority_notice","notice:skill"),
            ("agents/sales/.mcp.json",Some("sales"),"authority_json",".mcp.json"),
            ("connectors/legacy.enc",None,"authority_notice","notice:connector"),
            ("agents/sales/workflows/job.json",Some("sales"),"authority_notice","notice:schedule"),
            ("workflow.db",None,"authority_workflow_sqlite","workflow.db"),
            ("autopilot.db",None,"authority_autopilot_sqlite","autopilot.db"),
            ("reminders.jsonl",None,"authority_reminders_jsonl","reminders.jsonl"),
            ("cron_tasks.db",None,"authority_cron_sqlite","cron_tasks.db"),
            ("cron_tasks.jsonl",None,"authority_cron_jsonl","cron_tasks.jsonl"),
            ("shared/wiki/.scope.toml",None,"authority_notice","notice:policy"),
            ("agents/sales/USER.md",Some("sales"),"authority_notice","notice:policy"),
        ] {
            let source=Source{relative:path.into(),kind:"authority".into(),agent:agent.map(str::to_owned)};
            assert_eq!(worker_shape(&source).unwrap(),(kind.into(),title.into()));
        }
        let spoof=Source{relative:"secrets/website_credentials.enc.bak".into(),kind:"authority".into(),agent:None};
        assert!(worker_shape(&spoof).is_err());
    }
}
