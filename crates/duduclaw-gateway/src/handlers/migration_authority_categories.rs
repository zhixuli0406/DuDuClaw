//! Category-specific normal product validation and publication. These helpers
//! execute only after the durable confirmation store has recorded approval.
use super::*;
use duduclaw_agent::mcp_template::{self, McpServerDef};

#[derive(Serialize,Deserialize)]
pub(super) struct McpPlan { agent:String, name:String, definition:McpServerDef, before:String, receipt:String }
fn safe_name(name:&str)->bool {!name.is_empty() && name.len()<=128 && name.bytes().all(|b|b.is_ascii_alphanumeric()||matches!(b,b'-'|b'_'|b'.'))}
fn agent_dir(home:&Path,agent:&str)->Result<PathBuf> {
    if !duduclaw_core::is_valid_agent_id(agent) {return Err(Error::Invalid);}
    let dir=home.join("agents").join(agent);validate_parents(home,&dir.join("agent.toml"))?;
    regular(&dir.join("agent.toml"))?;Ok(dir)
}
pub(super) fn mcp_plan(home:&Path,item:&ConfirmationItem)->Result<McpPlan> {
    let agent=text(item,"employee_id")?;let name=text(item,"name")?;
    if !safe_name(name) || name=="duduclaw" || text(item,"transport")?!="stdio"
        || item.fields.iter().any(|f|matches!(f.name.as_str(),"credential"|"requires_reentry"|"url")) {return Err(Error::Denied);}
    agent_dir(home,agent)?;
    let args=match item.fields.iter().find(|f|f.name=="args") {Some(_)=>serde_json::from_str(text(item,"args")?).map_err(|_|Error::Invalid)?,None=>vec![]};
    // Re-entering env-based credentials requires the normal encrypted MCP form.
    // The old environment is never silently inherited by the migrated process.
    if item.fields.iter().any(|f|f.name=="env") && text(item,"env")?!="{}" {return Err(Error::Denied);}
    let definition=McpServerDef {command:text(item,"command")?.into(),args,env:Default::default()};
    if !crate::mcp_scan::scan_mcp_server_def(name,&definition).passed {return Err(Error::Denied);}
    if crate::remote_mcp::bridge_def::is_bridge_def(&definition) {return Err(Error::Denied);}
    let before=read_optional(&home.join("agents").join(agent).join(".mcp.json"))?;
    let receipt=format!("{}:{}",operation(&item.binding),hex::encode(item.binding.proposal_digest.0));
    Ok(McpPlan {agent:agent.into(),name:name.into(),definition,before,receipt})
}
fn same(a:&McpServerDef,b:&McpServerDef)->bool {a.command==b.command&&a.args==b.args&&a.env==b.env}
impl McpPlan {
    pub(super) fn check_absent(&self,home:&Path)->Result<()> {
        let dir=agent_dir(home,&self.agent)?;
        let config=mcp_template::read_mcp_config(&dir).map_err(|_|Error::Apply)?;
        if config.mcp_servers.contains_key(&self.name) {return Err(Error::Stale);}Ok(())
    }
    pub(super) fn publish(&self,home:&Path)->Result<()> {
        let dir=agent_dir(home,&self.agent)?;let path=dir.join(".mcp.json");
        mcp_template::with_mcp_config_lock(&path,|| {
            let current=read_optional(&path).map_err(|_|"migration MCP read failed".to_string())?;
            let mut doc:serde_json::Value=if current.is_empty(){serde_json::json!({"mcpServers":{}})}else{serde_json::from_str(&current).map_err(|_|"migration MCP parse failed".to_string())?};
            let mut config=mcp_template::read_mcp_config(&dir)?;
            if let Some(old)=config.mcp_servers.get(&self.name) {
                return if same(old,&self.definition) && doc.get("_migration_receipts").and_then(|r|r.get(&self.name)).and_then(serde_json::Value::as_str)==Some(self.receipt.as_str()) {Ok(())}else{Err("migration MCP conflict".into())};
            }
            if current!=self.before {return Err("migration MCP baseline changed".into());}
            if !crate::mcp_scan::scan_mcp_server_def(&self.name,&self.definition).passed {return Err("migration MCP scan refused".into());}
            config.mcp_servers.insert(self.name.clone(),self.definition.clone());
            // Same locked, owner-only atomic writer used by normal MCP installs.
            doc["mcpServers"]=serde_json::to_value(&config.mcp_servers).map_err(|_|"migration MCP encoding failed".to_string())?;
            if doc.get("_migration_receipts").is_some_and(|v|!v.is_object()) {return Err("migration MCP receipt invalid".into());}
            if doc.get("_migration_receipts").is_none() {doc["_migration_receipts"]=serde_json::json!({});}
            doc["_migration_receipts"][&self.name]=serde_json::Value::String(self.receipt.clone());
            let bytes=serde_json::to_vec_pretty(&doc).map_err(|_|"migration MCP encoding failed".to_string())?;
            mcp_template::write_mcp_config_atomic(&path,&bytes)?;
            std::fs::File::open(&dir).and_then(|f|f.sync_all()).map_err(|_|"migration MCP sync failed".to_string())
        }).map_err(|_|Error::Apply)
    }
    pub(super) fn read_back(&self,home:&Path)->Result<()> {
        let dir=agent_dir(home,&self.agent)?;
        // Ordinary runtime spawn still runs the MCP gate. Migration validates its
        // pure structural gate here without spawning the configured command.
        if !crate::mcp_scan::scan_mcp_server_def(&self.name,&self.definition).passed {return Err(Error::Apply);}
        let config=mcp_template::read_mcp_config(&dir).map_err(|_|Error::Apply)?;
        let doc:serde_json::Value=serde_json::from_str(&read_optional(&dir.join(".mcp.json"))?).map_err(|_|Error::Apply)?;
        if doc.get("_migration_receipts").and_then(|r|r.get(&self.name)).and_then(serde_json::Value::as_str)!=Some(self.receipt.as_str()) {return Err(Error::Stale);}
        if !config.mcp_servers.get(&self.name).is_some_and(|v|same(v,&self.definition)) {return Err(Error::Apply);}Ok(())
    }
    pub(super) fn disable(&self,home:&Path)->Result<()> {
        let dir=agent_dir(home,&self.agent)?;let path=dir.join(".mcp.json");
        mcp_template::with_mcp_config_lock(&path,|| {
            let mut config=mcp_template::read_mcp_config(&dir)?;
            let Some(current)=config.mcp_servers.get(&self.name) else {return Ok(());};
            let mut doc:serde_json::Value=serde_json::from_str(&read_optional(&path).map_err(|_|"migration MCP read failed".to_string())?).map_err(|_|"migration MCP parse failed".to_string())?;
            if !same(current,&self.definition) || doc.get("_migration_receipts").and_then(|r|r.get(&self.name)).and_then(serde_json::Value::as_str)!=Some(self.receipt.as_str()) {return Err("migration MCP changed".into());}
            config.mcp_servers.remove(&self.name);
            doc["mcpServers"]=serde_json::to_value(&config.mcp_servers).map_err(|_|"migration MCP encoding failed".to_string())?;
            doc.get_mut("_migration_receipts").and_then(serde_json::Value::as_object_mut).ok_or("migration MCP receipt missing")?.remove(&self.name);
            mcp_template::write_mcp_config_atomic(&path,&serde_json::to_vec_pretty(&doc).map_err(|_|"migration MCP encoding failed".to_string())?)?;
            if mcp_template::read_mcp_config(&dir)?.mcp_servers.contains_key(&self.name) {return Err("migration MCP disable failed".into());}
            std::fs::File::open(&dir).and_then(|f|f.sync_all()).map_err(|_|"migration MCP sync failed".to_string())
        }).map_err(|_|Error::Compensation)
    }
}

#[cfg(test)] mod tests {
    use super::*;
    use duduclaw_migration::{confirmation::{ConfirmationStore,ApprovalSession},import_manifest::PendingProposal,ids::RequestId};
    #[test]
    fn migration_authority_planted_admin_and_mcp_inactive_until_individual_confirmation() {
        let home=tempfile::tempdir().unwrap();
        let db=duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        std::fs::create_dir_all(home.path().join("agents/sales")).unwrap();
        std::fs::write(home.path().join("agents/sales/agent.toml"),"name='sales'\n").unwrap();
        let old=tempfile::tempdir().unwrap();
        let old_db=rusqlite::Connection::open(old.path().join("users.db")).unwrap();
        old_db.execute_batch(include_str!("../../../duduclaw-import/src/users-v1793.sql")).unwrap();
        old_db.execute("INSERT INTO users(id,email,display_name,password_hash,role,created_at,updated_at) VALUES ('old-admin','planted@example.com','Planted','OLD-PASSWORD-CANARY','admin','now','now')",[]).unwrap();drop(old_db);
        // Normal McpServerDef serialization includes an empty env object.
        std::fs::write(old.path().join(".mcp.json"),br#"{"mcpServers":{"planted":{"command":"node","args":["server.js"],"env":{}}}}"#).unwrap();
        let mut planted:Vec<PendingProposal>=vec![];
        for (file,kind) in [("users.db",duduclaw_import::SourceKind::AuthorityUsersSqlite),(".mcp.json",duduclaw_import::SourceKind::AuthorityJson)] {
            let request=duduclaw_import::ImportRequest {version:1,migration_id:RequestId([3;16]),employee:None,source_id:file.into(),kind,title:file.into(),legacy_agent:Some("sales".into()),has_wal:false,blob_handle:None};
            let extracted=duduclaw_import::authority::extract(&request,std::fs::File::open(old.path().join(file)).unwrap(),None).unwrap();
            let bytes=serde_json::to_vec(&extracted).unwrap();assert!(!String::from_utf8_lossy(&bytes).contains("OLD-PASSWORD-CANARY"));
            planted.extend(serde_json::from_slice::<Vec<PendingProposal>>(&bytes).unwrap());
        }
        stage_import_proposals(home.path(),&planted).unwrap();
        assert!(db.get_user_by_email("planted@example.com").unwrap().is_none());
        assert!(!home.path().join("agents/sales/.mcp.json").exists());
        let mut store=ConfirmationStore::open_existing(&home.path().join("migration-confirmations.db")).unwrap();
        let mut authority=GatewayAuthority::new(home.path().into()).unwrap();
        let pending=store.list().unwrap();
        for item in &pending {assert_eq!(item.status,ConfirmationStatus::Pending);assert!(store.apply_next(&item.binding,&mut authority).is_err());}
        assert!(db.get_user_by_email("planted@example.com").unwrap().is_none());
        assert!(!home.path().join("agents/sales/.mcp.json").exists());
        let session=ApprovalSession::from_verified_session(true,true,Some(100),[8;32],RequestId([9;16]));
        assert!(store.approve(&pending.iter().map(|p|p.binding).collect::<Vec<_>>(),&session,[8;32],100,&authority).is_err());
        for item in pending {
            store.approve(&[item.binding],&session,[8;32],100,&authority).unwrap();
            for _ in 0..3 {store.apply_next(&item.binding,&mut authority).unwrap();}
            authority.read_back(&item).unwrap();
        }
        let user=db.get_user_by_email("planted@example.com").unwrap().unwrap();assert_eq!(user.role,duduclaw_auth::UserRole::Admin);assert!(user.must_change_password);
        assert!(mcp_template::read_mcp_config(&home.path().join("agents/sales")).unwrap().mcp_servers.contains_key("planted"));
        // Replaying the root proposal stream cannot reset approved decisions.
        stage_import_proposals(home.path(),&planted).unwrap();
        assert!(store.list().unwrap().iter().all(|p|p.status==ConfirmationStatus::Applied));
    }
}

pub(crate) struct LiveAuthority {
    pub registry:std::sync::Arc<tokio::sync::RwLock<duduclaw_agent::registry::AgentRegistry>>,
    pub context:duduclaw_auth::UserContext,
    pub handle:tokio::runtime::Handle,
    pub scheduler:Option<std::sync::Arc<crate::cron_scheduler::CronScheduler>>,
}
#[derive(Serialize,Deserialize)]
pub(super) struct ProfilePlan {agent:String,key:String,before:String,after:String,receipt:String}
pub(super) fn profile_plan(home:&Path,item:&ConfirmationItem)->Result<ProfilePlan> {
    let agent=text(item,"employee_id")?;agent_dir(home,agent)?;
    let key=match text(item,"instruction_kind")? {"SOUL.md"=>"soul","IDENTITY.md"=>"identity","INSTRUCTIONS.md"=>"instructions","CONTRACT.toml"=>"contract",_=>return Err(Error::Invalid)};
    let key_value=crate::agent_files::FileKey::parse(key).map_err(|_|Error::Invalid)?;
    let path=crate::agent_files::file_path(home,agent,&key_value).map_err(|_|Error::Invalid)?;
    let before=crate::agent_files::read_or_empty(&path).map_err(|_|Error::Storage)?;
    let after=text(item,"text")?.to_string();
    let findings=crate::agent_files::rules::check(&crate::agent_files::rules::RuleInput {key:&key_value,old:&before,new:&after,is_admin:true,must_not:&[],full_scan:true});
    if findings.iter().any(|f|f.level==crate::agent_files::verdict::RuleLevel::Reject) {return Err(Error::Invalid);}
    Ok(ProfilePlan {agent:agent.into(),key:key.into(),before,after,receipt:format!("{}:{}",operation(&item.binding),hex::encode(item.binding.proposal_digest.0))})
}
impl ProfilePlan {
    fn owns(&self,home:&Path,key:&crate::agent_files::FileKey)->bool {
        crate::agent_files::history::last(home,&self.agent,key).is_some_and(|entry|entry.approval_id.as_deref()==Some(self.receipt.as_str()) && entry.sha256==crate::agent_files::hash_of(&self.after))
    }
    fn apply(&self,home:&Path,live:&LiveAuthority,rollback:bool)->Result<()> {
        use crate::agent_files::{self,flow::{self,Caller,SaveInput,Action}};
        let (before,after)=if rollback {(&self.after,&self.before)}else{(&self.before,&self.after)};
        let key=agent_files::FileKey::parse(&self.key).map_err(|_|Error::Invalid)?;
        let path=agent_files::file_path(home,&self.agent,&key).map_err(|_|Error::Invalid)?;
        let current=agent_files::read_or_empty(&path).map_err(|_|Error::Storage)?;
        if current==*after {return if rollback||self.owns(home,&key) {Ok(())}else{Err(Error::Stale)};}if current!=*before || (rollback&&!self.owns(home,&key)) {return Err(Error::Stale);}
        bounded(&live.handle,async {
            tokio::time::timeout(std::time::Duration::from_secs(30),async {
                let fresh=flow::require_live_agent_access(home,&live.context,&self.agent,duduclaw_auth::AccessLevel::Owner).map_err(|_|Error::Denied)?;
                if !fresh.is_admin() {return Err(Error::Denied);}
                let caller=Caller {home,registry:&live.registry,live:&fresh,session:&live.context};
                let mut input=SaveInput {agent_id:self.agent.clone(),key,content:after.clone(),base_hash:agent_files::hash_of(before),action:Action::Check,
                    analysis_id:None,reason:self.receipt.clone(),acknowledge_risk:true,source:if rollback {"migration.rollback"}else{"migration"}};
                let result=flow::save(&caller,input.clone()).await.map_err(|_|Error::Apply)?;
                if matches!(result.get("status").and_then(serde_json::Value::as_str),Some("applied"|"unchanged")) {return Ok(());}
                input.analysis_id=result.get("review").and_then(|v|v.get("analysis_id")).and_then(serde_json::Value::as_str).map(str::to_owned);
                if input.analysis_id.is_none() {return Err(Error::Apply);}
                input.action=Action::Confirm;
                let result=flow::save(&caller,input).await.map_err(|_|Error::Apply)?;
                if matches!(result.get("status").and_then(serde_json::Value::as_str),Some("applied"|"unchanged")) {Ok(())}else{Err(Error::Apply)}
            }).await.map_err(|_|Error::Apply)?
        })?
    }
    pub(super) fn publish(&self,home:&Path,live:&LiveAuthority)->Result<()> {self.apply(home,live,false)}
    pub(super) fn read_back(&self,home:&Path,live:&LiveAuthority,rollback:bool)->Result<()> {
        let expected=if rollback {&self.before}else{&self.after};
        let key=crate::agent_files::FileKey::parse(&self.key).map_err(|_|Error::Invalid)?;
        let path=crate::agent_files::file_path(home,&self.agent,&key).map_err(|_|Error::Invalid)?;
        if crate::agent_files::read_or_empty(&path).map_err(|_|Error::Storage)?!=*expected || (!rollback&&!self.owns(home,&key)) {return Err(Error::Apply);}
        bounded(&live.handle,async {
            let mut registry=live.registry.write().await;registry.scan().await.map_err(|_|Error::Apply)?;
            let agent=registry.get(&self.agent).ok_or(Error::Apply)?;
            let actual=match self.key.as_str() {"soul"=>agent.soul.as_deref(),"identity"=>agent.identity.as_deref(),"instructions"=>agent.instructions.as_deref(),
                "contract"=>{let contract:duduclaw_agent::contract::Contract=toml::from_str(expected).map_err(|_|Error::Apply)?;if serde_json::to_value(&contract).map_err(|_|Error::Apply)?!=serde_json::to_value(&agent.contract).map_err(|_|Error::Apply)? {return Err(Error::Apply);}return Ok(());},_=>return Err(Error::Invalid)};
            if actual.unwrap_or("")!=expected {return Err(Error::Apply);}Ok(())
        })?
    }
    pub(super) fn disable(&self,home:&Path,live:&LiveAuthority)->Result<()> {self.apply(home,live,true)?;self.read_back(home,live,true)}
}

#[cfg(test)] mod schema_tests {
    #[test]
    fn migration_authority_task_schema_matches_fresh_store() {
        let home=tempfile::tempdir().unwrap();let _store=crate::task_store::TaskStore::open(home.path()).unwrap();
        let actual=rusqlite::Connection::open(home.path().join("tasks.db")).unwrap();let expected=rusqlite::Connection::open_in_memory().unwrap();
        expected.execute_batch(include_str!("../../../duduclaw-import/src/tasks-v180.sql")).unwrap();
        // v1.81 P1e: the one-time goal-creator backfill marker table (task_store/goal_creator_backfill.rs).
        // The importer's v1.80 catalog stays as is: it fingerprints older source stores.
        expected.execute_batch(concat!("CREATE TABLE IF NOT EXISTS task_store_markers (\n",
            "                 name       TEXT PRIMARY KEY,\n",
            "                 applied_at TEXT NOT NULL\n",
            "             );")).unwrap();
        fn schema(c:&rusqlite::Connection)->Vec<(String,String,String,Option<String>)> {c.prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name,tbl_name,sql").unwrap().query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap().map(Result::unwrap).collect()}
        assert_eq!(schema(&actual),schema(&expected));
    }
}

#[cfg(test)] mod profile_tests {
    use super::*;
    #[tokio::test]
    async fn migration_authority_profile_uses_normal_review_writer_and_live_registry() {
        let temp=tempfile::tempdir().unwrap();let home=temp.path().canonicalize().unwrap();
        let handler=crate::handlers::MethodHandler::new(home.clone()).await;
        let response=handler.handle_agents_create_as(serde_json::json!({"name":"sales","display_name":"Sales"}),Some(&duduclaw_auth::UserContext::admin_fallback())).await;
        assert!(matches!(response,crate::protocol::WsFrame::Response {ok:true,..}));
        let db=duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();
        let user=db.create_user("reviewer@example.com","Reviewer","migration-test-password",duduclaw_auth::UserRole::Admin).unwrap();
        let context=db.live_user_context(&user.id).unwrap().unwrap();
        crate::agent_files::review::test_seam::set(&home, |_|Ok(r#"{"verdict":"pass","summary_zh":"通過","findings":[]}"#.into()));
        let live=std::sync::Arc::new(LiveAuthority {registry:handler.registry.clone(),context,handle:tokio::runtime::Handle::current(),scheduler:None});
        let text="I am Sales, the helpful assistant.";
        let path=home.join("agents/sales/SOUL.md");let before=std::fs::read_to_string(&path).unwrap();
        let item=ConfirmationItem {binding:ProposalBinding {operation_id:ProposalOperationId {proposal_id:duduclaw_migration::ids::RequestId([2;16]),revision:1},proposal_digest:Sha256Digest([5;32])},
            category:ConfirmationCategory::EmployeeInstructions,status:ConfirmationStatus::Applying,high_risk:true,
            fields:[("employee_id","sales"),("instruction_kind","SOUL.md"),("text",text)].into_iter().map(|(k,v)|ProposalField {name:k.into(),value:ProposalValue::Text {value:v.into()}}).collect()};
        let home_copy=home.clone();tokio::task::spawn_blocking(move || {
            let mut a=GatewayAuthority::new(home_copy.clone()).unwrap().with_live(live.clone());
            a.stage(&item).unwrap();assert_eq!(std::fs::read_to_string(home_copy.join("agents/sales/SOUL.md")).unwrap(),before);
            a.publish(&item).unwrap();a.read_back(&item).unwrap();a.publish(&item).unwrap();
            a.disable_and_read_back(&item).unwrap();assert_eq!(std::fs::read_to_string(home_copy.join("agents/sales/SOUL.md")).unwrap(),before);
            // An independently approved equal value is not our publication.
            live.handle.block_on(crate::agent_files::writer::commit(&live.registry,crate::agent_files::writer::Commit {
                home:&home_copy,agent_id:"sales",key:crate::agent_files::FileKey::Soul,expected_hash:Some(&crate::agent_files::hash_of(&before)),
                meta:crate::agent_files::writer::CommitMeta::new(&live.context.user_id,"dashboard"),baseline:crate::agent_files::writer::Baseline::Accept},
                |_|Ok(text.into()),||Ok(()))).unwrap();
            assert!(a.publish(&item).is_err());assert!(a.read_back(&item).is_err());assert!(a.disable_and_read_back(&item).is_err());
            assert_eq!(std::fs::read_to_string(home_copy.join("agents/sales/SOUL.md")).unwrap(),text);
        }).await.unwrap();
        assert!(crate::agent_files::review::test_seam::calls(&home)>0);
    }
}
