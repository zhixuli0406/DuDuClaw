//! Read-only SQL source grants through the normal source validator and employee
//! profile writer, including its lifecycle barrier, history and registry reload.
use super::*;
use super::categories::LiveAuthority;
#[derive(Serialize,Deserialize)]
pub(super) struct GrantPlan {agent:String,source:String,before:String,after:String,receipt:String}
pub(super) fn grant_plan(home:&Path,item:&ConfirmationItem,live:&LiveAuthority)->Result<GrantPlan> {
    let agent=text(item,"employee_id")?;let source=text(item,"name")?;
    if !duduclaw_core::is_valid_agent_id(agent) || text(item,"grants")?!="read_only" || item.fields.iter().any(|f|f.name=="settings") {return Err(Error::Invalid);}
    let path=home.join("agents").join(agent).join("agent.toml");validate_parents(home,&path)?;regular(&path)?;
    let normalized=bounded(&live.handle,crate::db_source_grants::validate_capability_grants(home,&serde_json::json!([source])))?.map_err(|_|Error::Denied)?;
    if normalized!=vec![source.to_string()] {return Err(Error::Invalid);}
    let before=read_optional(&path)?;let mut table:toml::Table=before.parse().map_err(|_|Error::Invalid)?;
    let config: duduclaw_core::types::AgentConfig=toml::Value::Table(table.clone()).try_into().map_err(|_|Error::Invalid)?;
    if config.capabilities.db_source_granted(source) {return Err(Error::Stale);}
    crate::db_source_grants::apply_grant_to_table(&mut table,source,true).map_err(|_|Error::Invalid)?;
    let after=config_commit::render_preserving(&before,&table).map_err(|_|Error::Invalid)?;
    Ok(GrantPlan {agent:agent.into(),source:source.into(),before,after,receipt:format!("{}:{}",operation(&item.binding),hex::encode(item.binding.proposal_digest.0))})
}
impl GrantPlan {
    fn owns(&self,home:&Path)->bool {
        crate::agent_files::history::last(home,&self.agent,&crate::agent_files::FileKey::AgentToml)
            .is_some_and(|entry|entry.approval_id.as_deref()==Some(&self.receipt) && entry.sha256==config_commit::content_hash(&self.after))
    }
    fn apply(&self,home:&Path,live:&LiveAuthority,rollback:bool)->Result<()> {
        let path=home.join("agents").join(&self.agent).join("agent.toml");
        let (expected,target)=if rollback {(&self.after,&self.before)} else {(&self.before,&self.after)};
        let current=read_optional(&path)?;
        if current==*target {return if rollback||self.owns(home) {Ok(())} else {Err(Error::Stale)};}
        if current!=*expected || (rollback&&!self.owns(home)) {return Err(Error::Stale);}
        if !rollback {bounded(&live.handle,crate::db_source_grants::validate_capability_grants(home,&serde_json::json!([self.source])))?.map_err(|_|Error::Denied)?;}
        bounded(&live.handle,async {
            let mut meta=crate::agent_files::writer::CommitMeta::new(&live.context.user_id,"migration.confirmations");meta.approval_id=Some(self.receipt.clone());
            let outcome=crate::agent_files::writer::commit(&live.registry,crate::agent_files::writer::Commit {home,agent_id:&self.agent,key:crate::agent_files::FileKey::AgentToml,
                expected_hash:Some(&config_commit::content_hash(expected)),meta,baseline:crate::agent_files::writer::Baseline::Accept},
                |_|Ok(target.clone()),|| {
                    if rollback {if !self.owns(home) {return Err("migration grant ownership changed".into());}}
                    else {let ctx=crate::agent_files::flow::require_live_agent_access(home,&live.context,&self.agent,duduclaw_auth::AccessLevel::Owner)?;if !ctx.is_admin() {return Err("migration grant requires admin".into());}}
                    Ok(())
                }).await.map_err(|_|Error::Apply)?;
            if !outcome.registry_reloaded {return Err(Error::Apply);}Ok(())
        })?
    }
    pub(super) fn publish(&self,home:&Path,live:&LiveAuthority)->Result<()> {self.apply(home,live,false)}
    pub(super) fn read_back(&self,home:&Path,live:&LiveAuthority,rollback:bool)->Result<()> {
        let expected=if rollback {&self.before} else {&self.after};
        if read_optional(&home.join("agents").join(&self.agent).join("agent.toml"))?!=*expected || (!rollback&&!self.owns(home)) {return Err(Error::Apply);}
        if !rollback {bounded(&live.handle,crate::db_source_grants::validate_capability_grants(home,&serde_json::json!([self.source])))?.map_err(|_|Error::Apply)?;}
        let disk=duduclaw_core::agent_toml::load(&home.join("agents").join(&self.agent));
        if disk.capabilities.db_source_granted(&self.source)==rollback {return Err(Error::Apply);}
        bounded(&live.handle,async {
            let mut reg=live.registry.write().await;reg.scan().await.map_err(|_|Error::Apply)?;
            if reg.get(&self.agent).ok_or(Error::Apply)?.config.capabilities.db_source_granted(&self.source)==rollback {return Err(Error::Apply);}Ok(())
        })?
    }
    pub(super) fn disable(&self,home:&Path,live:&LiveAuthority)->Result<()> {self.apply(home,live,true)?;self.read_back(home,live,true)}
}

#[cfg(test)] mod tests {
    use super::*;
    #[tokio::test] async fn migration_authority_data_grant_normal_writer_live_gate_and_compensation() {
        let temp=tempfile::tempdir().unwrap();let home=temp.path().canonicalize().unwrap();
        let handler=crate::handlers::MethodHandler::new(home.clone()).await;
        let result=handler.handle_agents_create_as(serde_json::json!({"name":"sales","display_name":"Sales"}),Some(&duduclaw_auth::UserContext::admin_fallback())).await;
        assert!(matches!(result,crate::protocol::WsFrame::Response {ok:true,..}));
        std::fs::write(home.join("config.toml"),"[db_sources.crm]\ndriver='sqlite'\nurl='/tmp/migration-fixture.db'\nallowed_tables=['notes']\n").unwrap();
        let db=duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();let user=db.create_user("data-admin@example.com","Admin","migration-test-password",duduclaw_auth::UserRole::Admin).unwrap();
        let live=LiveAuthority {registry:handler.registry.clone(),context:db.live_user_context(&user.id).unwrap().unwrap(),handle:tokio::runtime::Handle::current(),scheduler:None};
        let request=duduclaw_import::ImportRequest {version:1,migration_id:duduclaw_migration::ids::RequestId([1;16]),employee:None,source_id:"old-agent".into(),kind:duduclaw_import::SourceKind::AuthorityToml,title:"agent.toml".into(),legacy_agent:Some("sales".into()),has_wal:false,blob_handle:None};
        let old=b"[capabilities]\ndb_sources=['crm']\n";
        let proposal=duduclaw_import::authority::extract(&request,&old[..],None).unwrap().into_iter().find(|p|p.category==ConfirmationCategory::DataSourceAuthorization).unwrap();
        let item=ConfirmationItem {binding:ProposalBinding {operation_id:ProposalOperationId {proposal_id:duduclaw_migration::ids::RequestId([2;16]),revision:1},proposal_digest:Sha256Digest([3;32])},category:proposal.category,status:ConfirmationStatus::Applying,high_risk:true,fields:proposal.fields};
        tokio::task::spawn_blocking(move || {
            let plan=grant_plan(&home,&item,&live).unwrap();
            assert!(!duduclaw_core::agent_toml::load(&home.join("agents/sales")).capabilities.db_source_granted("crm"));
            plan.publish(&home,&live).unwrap();plan.publish(&home,&live).unwrap();plan.read_back(&home,&live,false).unwrap();
            plan.disable(&home,&live).unwrap();assert!(!duduclaw_core::agent_toml::load(&home.join("agents/sales")).capabilities.db_source_granted("crm"));
            plan.publish(&home,&live).unwrap();
            live.handle.block_on(crate::channel_reply::update_agent_toml_with(&live.registry,"sales",|table| {table.insert("normal_edit".into(),toml::Value::Boolean(true));Ok(())})).unwrap();
            assert!(plan.disable(&home,&live).is_err());assert!(duduclaw_core::agent_toml::load(&home.join("agents/sales")).capabilities.db_source_granted("crm"));
        }).await.unwrap();
    }
}
