//! Approved skill and hook publication through the normal parser, scanner,
//! owner-only atomic writer and runtime loaders. Old paths never shape targets.
use super::*;
use super::categories::LiveAuthority;
use serde_json::Value;

#[derive(Serialize,Deserialize)]
pub(super) enum ArtifactKind { Skill {name:String}, Hook {event:String} }
#[derive(Serialize,Deserialize)]
pub(super) struct ArtifactPlan {agent:String,relative:String,before:Option<String>,after:String,kind:ArtifactKind,prepared:Option<super::artifact_receipt::Prepared>}
fn employee(home:&Path,id:&str)->Result<PathBuf> {
    if !duduclaw_core::is_valid_agent_id(id) {return Err(Error::Invalid);}
    let dir=home.join("agents").join(id);validate_parents(home,&dir.join("agent.toml"))?;regular(&dir.join("agent.toml"))?;Ok(dir)
}
pub(super) fn optional(path:&Path)->Result<Option<String>> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file()=>read_optional(path).map(Some),
        Err(e) if e.kind()==std::io::ErrorKind::NotFound=>Ok(None),_=>Err(Error::Storage),
    }
}
pub(super) fn skill_plan(home:&Path,item:&ConfirmationItem)->Result<ArtifactPlan> {
    use sha2::{Digest,Sha256};
    let agent=text(item,"employee_id")?;let dir=employee(home,agent)?;
    let content=text(item,"text")?;let requested=text(item,"name")?;
    let name=duduclaw_agent::skill_loader::install_skill_name(content,requested).map_err(|_|Error::Invalid)?;
    // Every field used by the loader must have been visible in the approved item.
    if name!=requested || hex::encode(Sha256::digest(content.as_bytes()))!=text(item,"artifact_digest")? {return Err(Error::Invalid);}
    let scan=crate::skill_lifecycle::security_scanner::scan_skill(content,None);
    if !scan.passed || scan.findings.iter().any(|f|f.category==crate::skill_lifecycle::security_scanner::FindingCategory::SecretLeak) {return Err(Error::Denied);}
    // Canonical directory layout also keeps a later ordinary flat install
    // separate, so compensation cannot remove that independently accepted copy.
    let skills=dir.join("SKILLS");if skills.exists() {plain_directory(&skills)?;}
    if optional(&skills.join(format!("{name}.md")))?.is_some() {return Err(Error::Stale);}
    let relative=format!("SKILLS/{name}/SKILL.md");let path=dir.join(&relative);
    if let Some(parent)=path.parent() {if parent.exists() {plain_directory(parent)?;}}
    let before=optional(&path)?;
    if before.is_some() {return Err(Error::Stale);}
    Ok(ArtifactPlan {agent:agent.into(),relative,before,after:content.into(),kind:ArtifactKind::Skill {name},prepared:None})
}
#[derive(Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct HookGroup {
    #[serde(default,skip_serializing_if="Option::is_none")]
    matcher:Option<String>,
    hooks:Vec<HookCommand>,
}
#[derive(Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct HookCommand {
    #[serde(rename="type")]
    kind:String,
    command:String,
    #[serde(default,skip_serializing_if="Option::is_none")]
    timeout:Option<u64>,
}
fn event_valid(event:&str)->bool {matches!(event,"PreToolUse"|"PostToolUse"|"Stop"|"SubagentStop"|"SessionStart"|"SessionEnd"|"UserPromptSubmit"|"PreCompact"|"Notification")}
fn guards(home:&Path,agent:&str,root:&mut Value)->Result<()> {
    if !root.is_object() || root.get("disableAllHooks").is_some_and(|v|v!=&Value::Bool(false)) {return Err(Error::Denied);}
    root["disableAllHooks"]=Value::Bool(false);
    let bin=duduclaw_core::resolve_duduclaw_bin();
    crate::agent_hook_installer::merge_agent_file_guard_hook(root,&bin,agent,Some(home));
    crate::agent_hook_installer::merge_data_file_guard_hook(root,&bin);
    crate::agent_hook_installer::merge_egress_guard_hook(root,&bin,agent,Some(home));
    Ok(())
}
pub(super) fn hook_plan(home:&Path,item:&ConfirmationItem)->Result<ArtifactPlan> {
    let agent=text(item,"employee_id")?;let dir=employee(home,agent)?;
    let event=text(item,"event")?;if !event_valid(event) || text(item,"name")?!=event {return Err(Error::Invalid);}
    let groups:Vec<HookGroup>=serde_json::from_str(text(item,"settings")?).map_err(|_|Error::Invalid)?;
    if groups.is_empty() || groups.len()>32 {return Err(Error::Limit);}
    for group in &groups {
        if group.matcher.as_ref().is_some_and(|m|m.len()>512 || m.chars().any(char::is_control)) || group.hooks.is_empty() || group.hooks.len()>16 {return Err(Error::Invalid);}
        for command in &group.hooks {
            if command.kind!="command" || command.command.trim().is_empty() || command.command.len()>16*1024 || command.command.chars().any(|c|c=='\0')
                || command.timeout.is_some_and(|t|t==0||t>600) {return Err(Error::Invalid);}
            // Known credentials are refused rather than written to CLI settings.
            if duduclaw_security::credential_detect::mask_document(&command.command).map_err(|_|Error::Denied)?!=command.command {return Err(Error::Denied);}
        }
    }
    let relative=".claude/settings.json".to_string();let path=dir.join(&relative);
    if let Some(parent)=path.parent() {if parent.exists() {plain_directory(parent)?;}}
    let before=optional(&path)?;
    let mut root=match &before {Some(text)=>serde_json::from_str(text).map_err(|_|Error::Invalid)?,None=>serde_json::json!({})};
    guards(home,agent,&mut root)?;
    let hooks=root.get_mut("hooks").and_then(Value::as_object_mut).ok_or(Error::Invalid)?;
    let entries=hooks.entry(event).or_insert_with(||serde_json::json!([])).as_array_mut().ok_or(Error::Invalid)?;
    let extra=serde_json::to_value(groups).map_err(|_|Error::Invalid)?;
    for group in extra.as_array().ok_or(Error::Invalid)? {if entries.iter().any(|v|v==group) {return Err(Error::Stale);}entries.push(group.clone());}
    let after=serde_json::to_string_pretty(&root).map_err(|_|Error::Invalid)?;
    Ok(ArtifactPlan {agent:agent.into(),relative,before,after,kind:ArtifactKind::Hook {event:event.into()},prepared:None})
}
impl ArtifactPlan {
    pub(super) fn prepare(&mut self,home:&Path,item:&ConfirmationItem)->Result<()> {
        self.prepared=Some(super::artifact_receipt::Prepared::new(home,item,&self.after)?);Ok(())
    }
    pub(super) fn publish(&self,home:&Path,live:&LiveAuthority)->Result<()> {
        let dir=employee(home,&self.agent)?;let path=dir.join(&self.relative);
        let parent=path.parent().ok_or(Error::Invalid)?;
        if matches!(&self.kind,ArtifactKind::Skill {..}) {let skills=dir.join("SKILLS");if !skills.exists() {create_private_dir(&skills)?;}plain_directory(&skills)?;}
        if !parent.exists() {create_private_dir(parent)?;}plain_directory(parent)?;
        let prepared=self.prepared.as_ref().ok_or(Error::Storage)?;
        let current=optional(&path)?;if current.as_deref()==Some(&self.after) && prepared.owns(&path) {return Ok(());}
        if current!=self.before {return Err(Error::Stale);}
        if let ArtifactKind::Skill {name}=&self.kind {
            // Run the same install API used by skills.install against a private
            // staging directory. Only its reviewed single Markdown output is published.
            let stage=tempfile::tempdir_in(home.join("migration-authority")).map_err(|_|Error::Storage)?;
            let source=stage.path().join(format!("{name}.md"));std::fs::write(&source,&self.after).map_err(|_|Error::Storage)?;
            let output=stage.path().join("installed");let quarantine=stage.path().join("quarantine");
            let parsed=live.handle.block_on(duduclaw_agent::skill_loader::install_skill(&source,&output,&quarantine)).map_err(|_|Error::Apply)?;
            if parsed.meta.name!=*name || std::fs::read_to_string(output.join(format!("{name}.md"))).map_err(|_|Error::Storage)?!=self.after {return Err(Error::Apply);}
        }
        prepared.publish(home,&path,self.before.as_deref(),&self.after)
    }
    pub(super) fn read_back(&self,home:&Path,live:&LiveAuthority)->Result<()> {
        let dir=employee(home,&self.agent)?;
        let path=dir.join(&self.relative);
        if !self.prepared.as_ref().ok_or(Error::Storage)?.owns(&path) || optional(&path)?.as_deref()!=Some(&self.after) {return Err(Error::Apply);}
        match &self.kind {
            ArtifactKind::Skill {name}=>live.handle.block_on(async {
                let mut registry=live.registry.write().await;registry.scan().await.map_err(|_|Error::Apply)?;
                let employee=registry.get(&self.agent).ok_or(Error::Apply)?;
                if employee.skills.iter().filter(|s|s.name==*name && s.content==self.after).count()!=1 {return Err(Error::Apply);}Ok(())
            }),
            ArtifactKind::Hook {..}=>{
                let root:Value=serde_json::from_str(&self.after).map_err(|_|Error::Apply)?;let mut effective=root.clone();guards(home,&self.agent,&mut effective)?;
                if root!=effective {return Err(Error::Apply);}Ok(())
            },
        }
    }
    pub(super) fn disable(&self,home:&Path,live:&LiveAuthority)->Result<()> {
        let path=employee(home,&self.agent)?.join(&self.relative);
        self.prepared.as_ref().ok_or(Error::Storage)?.restore(home,&path,self.before.as_deref(),&self.after)?;
        if optional(&path)?!=self.before {return Err(Error::Compensation);}
        if let ArtifactKind::Skill {name}=&self.kind {
            live.handle.block_on(async {let mut registry=live.registry.write().await;registry.scan().await.map_err(|_|Error::Compensation)?;
                if registry.get(&self.agent).is_some_and(|a|a.skills.iter().any(|s|s.name==*name && s.content==self.after)) {return Err(Error::Compensation);}Ok(())})?;
        }
        Ok(())
    }
}

#[cfg(all(test,unix))] mod tests {
    use super::*;
    use sha2::{Digest,Sha256};
    async fn fixture()->(tempfile::TempDir,PathBuf,std::sync::Arc<LiveAuthority>) {
        let tmp=tempfile::tempdir().unwrap();let home=tmp.path().canonicalize().unwrap();
        let handler=crate::handlers::MethodHandler::new(home.clone()).await;
        let response=handler.handle_agents_create_as(serde_json::json!({"name":"sales","display_name":"Sales"}),Some(&duduclaw_auth::UserContext::admin_fallback())).await;
        assert!(matches!(response,crate::protocol::WsFrame::Response {ok:true,..}));
        let db=duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();let user=db.create_user("artifact-admin@example.com","Admin","migration-test-password",duduclaw_auth::UserRole::Admin).unwrap();
        let live=std::sync::Arc::new(LiveAuthority {registry:handler.registry.clone(),context:db.live_user_context(&user.id).unwrap().unwrap(),handle:tokio::runtime::Handle::current(),scheduler:None});
        (tmp,home,live)
    }
    fn item(category:ConfirmationCategory,fields:Vec<(&str,String)>)->ConfirmationItem {
        ConfirmationItem {binding:ProposalBinding {operation_id:ProposalOperationId {proposal_id:duduclaw_migration::ids::RequestId([7;16]),revision:1},proposal_digest:Sha256Digest([8;32])},
            category,status:ConfirmationStatus::Applying,high_risk:true,fields:fields.into_iter().map(|(k,v)|ProposalField {name:k.into(),value:ProposalValue::Text {value:v}}).collect()}
    }
    #[tokio::test]
    async fn migration_authority_skill_staged_install_reads_live_registry_and_compensates() {
        let (_tmp,home,live)=fixture().await;
        let content="---\nname: migration-notes\ndescription: Write clear notes.\n---\n\nWrite clear notes.\n";
        let p=item(ConfirmationCategory::Skill,vec![("employee_id","sales".into()),("name","migration-notes".into()),("text",content.into()),("artifact_digest",hex::encode(Sha256::digest(content.as_bytes())))]);
        tokio::task::spawn_blocking(move || {
            let mut a=GatewayAuthority::new(home.clone()).unwrap().with_live(live);let path=home.join("agents/sales/SKILLS/migration-notes/SKILL.md");
            a.stage(&p).unwrap();assert!(!path.exists());a.publish(&p).unwrap();a.read_back(&p).unwrap();a.publish(&p).unwrap();
            {use std::os::unix::fs::MetadataExt;assert_eq!(std::fs::metadata(&path).unwrap().nlink(),1);}
            a.disable_and_read_back(&p).unwrap();assert!(!path.exists());a.publish(&p).unwrap();a.read_back(&p).unwrap();
        }).await.unwrap();
    }
    #[tokio::test]
    async fn migration_authority_same_content_replacement_is_not_owned_or_compensated() {
        let (_tmp,home,live)=fixture().await;
        let content="---\nname: migration-retry\ndescription: Write notes.\n---\nWrite notes.\n";
        let p=item(ConfirmationCategory::Skill,vec![("employee_id","sales".into()),("name","migration-retry".into()),("text",content.into()),("artifact_digest",hex::encode(Sha256::digest(content.as_bytes())))]);
        tokio::task::spawn_blocking(move || {
            let mut a=GatewayAuthority::new(home.clone()).unwrap().with_live(live.clone());
            a.stage(&p).unwrap();a.publish(&p).unwrap();drop(a);
            let mut retry=GatewayAuthority::new(home.clone()).unwrap().with_live(live);
            retry.publish(&p).unwrap();retry.read_back(&p).unwrap();
            let path=home.join("agents/sales/SKILLS/migration-retry/SKILL.md");
            config_commit::replace_owner_only(&path,content).unwrap();
            assert!(retry.read_back(&p).is_err());assert!(retry.publish(&p).is_err());assert!(retry.disable_and_read_back(&p).is_err());
            assert_eq!(std::fs::read_to_string(path).unwrap(),content);
        }).await.unwrap();
    }
    #[tokio::test]
    async fn migration_authority_hook_preserves_guards_and_refuses_concurrent_compensation() {
        let (_tmp,home,live)=fixture().await;
        let p=item(ConfirmationCategory::Hook,vec![("employee_id","sales".into()),("name","Stop".into()),("event","Stop".into()),("settings",r#"[{"hooks":[{"type":"command","command":"printf done","timeout":10}]}]"#.into())]);
        tokio::task::spawn_blocking(move || {
            let mut a=GatewayAuthority::new(home.clone()).unwrap().with_live(live);let path=home.join("agents/sales/.claude/settings.json");let before=optional(&path).unwrap();
            a.stage(&p).unwrap();assert_eq!(optional(&path).unwrap(),before);a.publish(&p).unwrap();a.read_back(&p).unwrap();
            let mut doc:Value=serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();assert_eq!(doc["disableAllHooks"],false);assert!(doc["hooks"]["PreToolUse"].as_array().unwrap().len()>=3);
            doc["userChanged"]=Value::Bool(true);std::fs::write(&path,serde_json::to_vec(&doc).unwrap()).unwrap();assert!(a.disable_and_read_back(&p).is_err());
            assert!(serde_json::from_str::<Value>(&std::fs::read_to_string(&path).unwrap()).unwrap()["userChanged"].as_bool().unwrap());
        }).await.unwrap();
    }
}
