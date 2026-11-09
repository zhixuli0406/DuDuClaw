//! Fresh time-triggered schedules. Old run cursors, command triggers and channel
//! delivery grants are never silently restored alongside the reviewed cron job.
use super::*;
use super::categories::LiveAuthority;
#[derive(Serialize,Deserialize)]
pub(super) struct SchedulePlan {row:crate::cron_store::CronTaskRow,operation:String,digest:String}
#[derive(Default,Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {#[serde(default)] kind:Option<String>,#[serde(default)] timezone:Option<String>}
pub(super) fn schedule_plan(home:&Path,item:&ConfirmationItem,live:&LiveAuthority)->Result<SchedulePlan> {
    live.scheduler.as_ref().ok_or(Error::Denied)?;
    let agent=text(item,"employee_id")?;
    if !duduclaw_core::is_valid_agent_id(agent) || !config_commit::is_plain_text(text(item,"name")?,200) {return Err(Error::Invalid);}
    let path=home.join("agents").join(agent).join("agent.toml");validate_parents(home,&path)?;regular(&path)?;
    let settings:Settings=match item.fields.iter().find(|f|f.name=="settings") {Some(_)=>serde_json::from_str(text(item,"settings")?).map_err(|_|Error::Invalid)?,None=>Settings::default()};
    if settings.kind.as_deref().is_some_and(|v|v!="cron"&&v!="time") {return Err(Error::Denied);}
    if settings.timezone.as_ref().is_some_and(|tz|tz.parse::<chrono_tz::Tz>().is_err()) {return Err(Error::Invalid);}
    let cron=crate::cron_scheduler::normalise_cron(text(item,"schedule")?);
    cron.parse::<cron::Schedule>().map_err(|_|Error::Invalid)?;
    let objective=text(item,"objective")?;if objective.trim().is_empty() || objective.len()>32*1024 || objective.chars().any(|c|c=='\0') {return Err(Error::Invalid);}
    let operation=operation(&item.binding);let digest=hex::encode(item.binding.proposal_digest.0);
    let id=format!("migration-{operation}-{digest}");
    let mut row=crate::cron_store::CronTaskRow::new(id,text(item,"name")?.into(),agent.into(),cron,objective.into());
    row.cron_timezone=settings.timezone;
    // Fresh admin acceptance is the host-carried human origin; old actor IDs
    // and execution state have no authority in the new installation.
    row.on_behalf_of=Some(live.context.user_id.clone());
    Ok(SchedulePlan {row,operation,digest})
}
impl SchedulePlan {
    fn store(&self,home:&Path)->Result<crate::cron_store::CronStore> {crate::cron_store::CronStore::open(home).map_err(|_|Error::Storage)}
    pub(super) fn publish(&self,home:&Path,live:&LiveAuthority)->Result<()> {
        live.scheduler.as_ref().ok_or(Error::Denied)?;
        bounded(&live.handle,self.store(home)?.migration_publish(&self.row,&self.operation,&self.digest))?.map_err(|_|Error::Apply)
    }
    pub(super) fn read_back(&self,home:&Path,live:&LiveAuthority)->Result<()> {
        let scheduler=live.scheduler.as_ref().ok_or(Error::Denied)?;
        bounded(&live.handle,async {self.store(home)?.migration_verify(&self.row,&self.operation,&self.digest,true).await.map_err(|_|Error::Apply)?;
            scheduler.migration_reload_readback(&self.row,true).await.map_err(|_|Error::Apply)})?
    }
    pub(super) fn disable(&self,home:&Path,live:&LiveAuthority)->Result<()> {
        let scheduler=live.scheduler.as_ref().ok_or(Error::Compensation)?;
        bounded(&live.handle,async {self.store(home)?.migration_disable(&self.row,&self.operation,&self.digest).await.map_err(|_|Error::Compensation)?;
            scheduler.migration_reload_readback(&self.row,false).await.map_err(|_|Error::Compensation)})?
    }
}

#[cfg(test)] mod tests {
    use super::*;
    #[tokio::test] async fn migration_authority_schedule_live_cache_readback_and_compensation() {
        let temp=tempfile::tempdir().unwrap();let home=temp.path().canonicalize().unwrap();
        std::fs::create_dir_all(home.join("agents/sales")).unwrap();std::fs::write(home.join("agents/sales/agent.toml"),"name='sales'\n").unwrap();
        let handler=crate::handlers::MethodHandler::new(home.clone()).await;
        let store=std::sync::Arc::new(crate::cron_store::CronStore::open(&home).unwrap());
        let scheduler=std::sync::Arc::new(crate::cron_scheduler::CronScheduler::new(home.clone(),store.clone(),handler.registry.clone()));
        let live=LiveAuthority {registry:handler.registry.clone(),context:duduclaw_auth::UserContext::admin_fallback(),handle:tokio::runtime::Handle::current(),scheduler:Some(scheduler)};
        let old=crate::cron_store::CronTaskRow::new("old-cron".into(),"hourly notes".into(),"sales".into(),"0 * * * *".into(),"write local notes".into());
        let old_json=serde_json::to_vec(&old).unwrap();
        let request=duduclaw_import::ImportRequest {version:1,migration_id:duduclaw_migration::ids::RequestId([9;16]),employee:None,
            source_id:"legacy-cron".into(),kind:duduclaw_import::SourceKind::AuthorityCronJsonl,title:"cron_tasks.jsonl".into(),legacy_agent:None,has_wal:false,blob_handle:None};
        let mut proposals=duduclaw_import::authority::extract(&request,old_json.as_slice(),None).unwrap();
        assert_eq!(proposals.len(),1);let proposal=proposals.remove(0);
        assert_eq!(proposal.category,ConfirmationCategory::Schedule);
        let item=ConfirmationItem {binding:ProposalBinding {operation_id:ProposalOperationId {proposal_id:duduclaw_migration::ids::RequestId([9;16]),revision:1},proposal_digest:Sha256Digest([6;32])},category:ConfirmationCategory::Schedule,status:ConfirmationStatus::Applying,high_risk:true,
            fields:proposal.fields};
        tokio::task::spawn_blocking(move || {
            let plan=schedule_plan(&home,&item,&live).unwrap();assert!(live.handle.block_on(store.list_enabled()).unwrap().is_empty());
            plan.publish(&home,&live).unwrap();plan.read_back(&home,&live).unwrap();plan.disable(&home,&live).unwrap();assert!(live.handle.block_on(store.list_enabled()).unwrap().is_empty());
            plan.publish(&home,&live).unwrap();plan.read_back(&home,&live).unwrap();
        }).await.unwrap();
    }
}
