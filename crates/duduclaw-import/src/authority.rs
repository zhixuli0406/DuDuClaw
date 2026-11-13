//! Untrusted legacy authority extraction. These records are proposals only and
//! never use the content ImportRecord schema. Run exclusively in the fd worker.
use std::io::{Read, Write};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use duduclaw_isolation::{ids::RequestId, migration::{ConfirmationCategory as C, ProposalField, ProposalValue, MAX_PROPOSAL_VALUE_BYTES, MAX_IMPORT_STREAM_BYTES}};
use crate::{Error, Result, ImportRequest, SourceKind};
#[path = "authority_stores.rs"]
mod stores;
#[path = "authority_cron.rs"]
mod cron;
#[path = "authority_config.rs"]
mod config;
#[path = "authority_schedules.rs"]
mod schedules;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityProposal {
    pub migration_id: RequestId,
    pub source_id: String,
    pub sequence: u64,
    pub category: C,
    pub fields: Vec<ProposalField>,
}
fn field(name:&str,value:impl Into<String>)->ProposalField { ProposalField {name:name.into(),value:ProposalValue::Text {value:value.into()}} }
fn encoded(value:&Value)->Result<String> {serde_json::to_string(value).map_err(|_|Error::Content)}
fn member<'a>(value:&'a Value,name:&str)->Result<&'a str> {value.get(name).and_then(Value::as_str).ok_or(Error::Content)}
fn secret_name(name:&str)->bool {
    // Kept at least as restrictive as the normal raw-config editor. Container
    // names hide every nested key too, because header/env names can be secrets.
    let k=name.to_ascii_lowercase();
    k.ends_with("_enc") || k=="key" || k.ends_with("_key") || k=="api_hash"
        || matches!(k.as_str(),"headers"|"otlp_headers"|"env"|"authorization")
        || k.split(['_','-','.']).any(|p|matches!(p,"token"|"secret"|"password"|"passwd"|"apikey"|"credential"|"credentials"))
        || matches!(k.as_str(),"service_account_json"|"accesstoken"|"refreshtoken"|"clientsecret")
}
fn secret_url(value:&str)->bool {
    let Some((_,rest))=value.split_once("://") else {return false;};
    rest.split(['/', '?', '#']).next().is_some_and(|a|a.split_once('@').is_some())
        || rest.split_once('?').is_some() || rest.split_once('#').is_some()
}

fn redact(value:&mut Value,secrets:&mut serde_json::Map<String,Value>,prefix:&str) {
    match value {
        Value::Object(map)=>for (name,v) in map {
            let path=format!("{prefix}/{name}");
            if secret_name(name) {secrets.insert(path,v.clone());*v=Value::String("«set»".into());}
            else {redact(v,secrets,&path);}
        },
        Value::Array(items)=>for (index,v) in items.iter_mut().enumerate() {redact(v,secrets,&format!("{prefix}/{index}"));},
        Value::String(s) if secret_url(s)=>{secrets.insert(prefix.into(),Value::String(s.clone()));*s="«set»".into();},
        _=>{},
    }
}
/// Schema fields without an explicit migration reader are withheld. This also
/// prevents a future provider's differently-named credential from reaching UI.
fn redact_unknown(value:&mut Value,allowed:&[&str],secrets:&mut serde_json::Map<String,Value>) {
    match value {
        Value::Object(map)=>for (key,v) in map {
            if !allowed.iter().any(|known|*known==key) {secrets.insert(key.clone(),Value::Null);*v=Value::String("«set»".into());}
            else {redact_unknown(v,allowed,secrets);}
        },
        Value::Array(items)=>for v in items {redact_unknown(v,allowed,secrets);},_=>{},
    }
}
struct Builder<'a> {request:&'a ImportRequest, proposals:Vec<AuthorityProposal>}
impl Builder<'_> {
    fn push(&mut self,category:C,mut fields:Vec<ProposalField>,credential:Option<String>)->Result<()> {
        if self.proposals.len()>=2048 || fields.iter().any(|f|matches!(&f.value,ProposalValue::Text {value} if value.len()>MAX_PROPOSAL_VALUE_BYTES))
            || credential.as_ref().is_some_and(|s|s.len()>MAX_PROPOSAL_VALUE_BYTES) {return Err(Error::Limit);}
        if credential.is_some() && !fields.iter().any(|f|f.name=="requires_reentry") {fields.push(field("requires_reentry","true"));}
        self.proposals.push(AuthorityProposal {migration_id:self.request.migration_id,source_id:self.request.source_id.clone(),sequence:self.proposals.len() as u64,category,fields});Ok(())
    }
    fn settings(&mut self,category:C,name:&str,mut value:Value,extra:Vec<ProposalField>)->Result<()> {
        let mut secrets=serde_json::Map::new();redact(&mut value,&mut secrets,"");
        let settings=if matches!(category,C::ConfigurationSection|C::Policy) {toml::to_string(&value).map_err(|_|Error::Content)?}else{encoded(&value)?};
        let mut fields=vec![field(if category==C::ConfigurationSection {"section"} else {"name"},name),field(if category==C::Policy {"rules"} else {"settings"},settings)];
        fields.extend(extra);
        self.push(category,fields,if secrets.is_empty() {None}else{Some(encoded(&Value::Object(secrets))?)})
    }
}
/// The request title is a root-classified relative source path. Source ids and
/// employee names are display/linkage data, not destination filesystem paths.
pub fn extract(request:&ImportRequest,input:impl Read,wal:Option<&mut dyn Read>)->Result<Vec<AuthorityProposal>> {
    crate::catalog::clear_schema_refusal();
    request.validate()?;
    if request.has_wal != wal.is_some() {return Err(Error::InvalidRequest);}
    let mut b=Builder {request,proposals:vec![]};
    if matches!(request.kind,SourceKind::AuthorityUsersSqlite) {users_sqlite(input,wal,&mut b)?;return Ok(b.proposals);}
    if matches!(request.kind,SourceKind::AuthorityTasksSqlite) {tasks_sqlite(input,wal,&mut b)?;return Ok(b.proposals);}
    if matches!(request.kind,SourceKind::AuthorityApprovalsSqlite) {stores::approvals(input,wal,&mut b)?;return Ok(b.proposals);}
    if matches!(request.kind,SourceKind::AuthoritySessionsSqlite) {stores::channel_settings(input,wal,&mut b)?;return Ok(b.proposals);}
    if matches!(request.kind,SourceKind::AuthorityCronSqlite) {cron::sqlite(input,wal,&mut b)?;return Ok(b.proposals);}
    if matches!(request.kind,SourceKind::AuthorityCronJsonl) {if wal.is_some() {return Err(Error::InvalidRequest);} cron::jsonl(input,&mut b)?;return Ok(b.proposals);}
    if matches!(request.kind,SourceKind::AuthorityAutopilotSqlite|SourceKind::AuthorityWorkflowSqlite) {schedules::sqlite(input,wal,&mut b)?;return Ok(b.proposals);}
    if matches!(request.kind,SourceKind::AuthorityRemindersJsonl) {schedules::reminders(input,&mut b)?;return Ok(b.proposals);}
    if matches!(request.kind,SourceKind::AuthorityNotice) {stores::notice(&mut b)?;return Ok(b.proposals);}
    if matches!(request.kind,SourceKind::AuthorityMcpKeysSqlite) {stores::mcp_keys(input,wal,&mut b)?;return Ok(b.proposals);}
    if wal.is_some() {return Err(Error::InvalidRequest);}
    let mut bytes=Vec::new();input.take(1024*1024+1).read_to_end(&mut bytes)?;if bytes.len()>1024*1024 {return Err(Error::Limit);}
    let agent=request.legacy_agent.as_deref();
    if matches!(request.kind,SourceKind::AuthorityInstructions|SourceKind::AuthoritySkill) {
        let text=String::from_utf8(bytes).map_err(|_|Error::Content)?;
        let agent=agent.ok_or(Error::InvalidRequest)?;
        if matches!(request.kind,SourceKind::AuthorityInstructions) {
            b.push(C::EmployeeInstructions,vec![field("employee_id",agent),field("instruction_kind",&request.title),field("text",text)],None)?;
        } else {let digest=Sha256::digest(text.as_bytes()).iter().map(|x|format!("{x:02x}")).collect::<String>();
            b.push(C::Skill,vec![field("employee_id",agent),field("name",&request.title),field("artifact_digest",digest),field("text",text)],None)?;}
        return Ok(b.proposals);
    }
    let value:Value=if matches!(request.kind,SourceKind::AuthorityToml) {
        let table:toml::Table=std::str::from_utf8(&bytes).map_err(|_|Error::Content)?.parse().map_err(|_|Error::Content)?;
        serde_json::to_value(table).map_err(|_|Error::Content)?
    } else if matches!(request.kind,SourceKind::AuthorityJson) {serde_json::from_slice(&bytes).map_err(|_|Error::Content)?} else {return Err(Error::InvalidRequest);};
    if stores::json(&request.title,&value,&mut b)? {return Ok(b.proposals);}
    match request.title.as_str() {
        "config.toml"=>for (name,section) in value.as_object().ok_or(Error::Content)? {
            let category=match name.as_str() {"delegation"|"action_rules"=>C::Policy,"connectors"|"integrations"|"odoo"|"odoo_events"|"odoo_livechat"=>C::Connector,"data_sources"|"db_sources"=>C::DataSourceAuthorization,
                "cron"|"reminders"|"workflows"|"autopilot"|"responsibilities"=>C::Schedule,_=>C::ConfigurationSection};
            // Section schemas without a reviewed migration adapter are named
            // for re-entry, never recursively dumped into a public proposal.
            if config::reviewed_section(name,section) {
                b.settings(category,name,section.clone(),vec![])?;
            } else {
                let label=if !name.is_empty() && name.len()<=64 && name.bytes().all(|b|b.is_ascii_lowercase()||b==b'_') {name.as_str()}else{"unsupported_section"};
                b.push(category,vec![field(if category==C::ConfigurationSection {"section"}else{"name"},label),field(if category==C::Policy {"rules"}else{"settings"},""),field("requires_reentry","true")],None)?;
            }
        },
        "org.toml"=>stores::organization(&value,&mut b)?,
        ".scope.toml"=>b.settings(C::Policy,"wiki_scope",value,vec![field("employee_id",agent.ok_or(Error::InvalidRequest)?)])?,
        "agent.toml"=>{
            let agent=agent.ok_or(Error::InvalidRequest)?;
            let mut settings=value;config::agent_grants(&mut settings,agent,&mut b)?;let mut secrets=serde_json::Map::new();redact(&mut settings,&mut secrets,"");
            redact_unknown(&mut settings,&["name","display_name","description","runtime","model","capabilities","permissions","budget","account_pool","kind","provider","timeout_secs","minimal_context","allowed_tools","denied_tools","approval_required_tools","irreversible_tools","maybe_irreversible_tools","scoped_tools","sandbox","sandbox_level","read","write","execute","network","daily_cap_cents","monthly_limit_cents","hard_stop","when_unavailable"],&mut secrets);
            b.push(C::Employee,vec![field("employee_id",agent),field("settings",encoded(&settings)?),field("disk_bytes","0")],
                if secrets.is_empty(){None}else{Some(encoded(&Value::Object(secrets))?)})?;
        },
        ".mcp.json"=>{
            let servers=value.get("mcpServers").and_then(Value::as_object).ok_or(Error::Content)?;
            for (name,server) in servers {
                let mut server=server.clone();
                let unknown=server.as_object().ok_or(Error::Content)?.keys().any(|key|!matches!(key.as_str(),"type"|"command"|"args"|"url"|"env"));
                // McpServerDef serializes its default environment as {}. An
                // empty map carries no credential and remains actionable.
                let empty_env=server.get("env").and_then(Value::as_object).is_some_and(|env|env.is_empty());
                if empty_env {server.as_object_mut().ok_or(Error::Content)?.remove("env");}
                let mut secrets=serde_json::Map::new();redact(&mut server,&mut secrets,"");
                if unknown {secrets.insert("unsupported_server_field".into(),Value::Null);}
                if empty_env {server["env"]=serde_json::json!({});}
                let transport=server.get("type").and_then(Value::as_str).unwrap_or("stdio");
                let mut fields=vec![field("employee_id",agent.ok_or(Error::InvalidRequest)?),field("name",name),field("transport",transport)];
                for key in ["command","args","url","env"] {if let Some(v)=server.get(key) {fields.push(field(key,if let Some(s)=v.as_str(){s.into()}else{encoded(v)?}));}}
                b.push(C::McpServer,fields,if secrets.is_empty(){None}else{Some(encoded(&Value::Object(secrets))?)})?;
            }
        },
        "hooks.json"|"settings.json"=>{
            let hooks=value.get("hooks").unwrap_or(&value).as_object().ok_or(Error::Content)?;
            for (event,rules) in hooks {
                if !matches!(event.as_str(),"PreToolUse"|"PostToolUse"|"Stop"|"SubagentStop"|"SessionStart"|"SessionEnd"|"UserPromptSubmit"|"PreCompact"|"Notification") {return Err(Error::Content);}
                let mut rules=rules.clone();let mut secrets=serde_json::Map::new();redact(&mut rules,&mut secrets,"");
                redact_unknown(&mut rules,&["matcher","hooks","type","command","timeout"],&mut secrets);b.push(C::Hook,vec![field("employee_id",agent.ok_or(Error::InvalidRequest)?),field("name",event),field("event",event),field("settings",encoded(&rules)?)],if secrets.is_empty(){None}else{Some("redacted".into())})?;}
        },
        "approvals.json"=>for (index,_) in value.as_array().ok_or(Error::Content)?.iter().enumerate() {b.push(C::LegacyApproval,vec![field("reference",format!("{}:{index}",request.source_id))],None)?;},
        "roles.json"=>for role in value.as_array().ok_or(Error::Content)? {b.push(C::Role,vec![field("name",member(role,"name")?),field("permissions",encoded(role.get("permissions").ok_or(Error::Content)?)?)],None)?;},
        "departments.json"=>for dept in value.as_array().ok_or(Error::Content)? {let name=dept.as_str().or_else(||dept.get("name").and_then(Value::as_str)).ok_or(Error::Content)?;b.push(C::Department,vec![field("name",name)],None)?;},
        "channel_bindings.json"=>for binding in value.as_array().ok_or(Error::Content)? {b.push(C::ChannelBinding,vec![field("employee_id",member(binding,"agent_id")?),field("channel",member(binding,"channel")?),field("binding",encoded(&serde_json::json!({"channel":member(binding,"channel")?,"agent_id":member(binding,"agent_id")?}))?),field("requires_reentry","true")],None)?;},
        "connectors.json"=>for (name,settings) in value.as_object().ok_or(Error::Content)? {let _=settings;b.push(C::Connector,vec![field("name",name),field("settings",""),field("requires_reentry","true")],None)?;},
        "data_sources.json"=>for (name,settings) in value.as_object().ok_or(Error::Content)? {let _=settings;b.push(C::DataSourceAuthorization,vec![field("name",name),field("settings",""),field("requires_reentry","true")],None)?;},
        "schedules.json"|"cron.json"|"reminders.json"|"workflows.json"|"autopilot.json"|"responsibilities.json"=>{
            b.settings(C::Schedule,&request.title,value,vec![])?;
        },
        _=>return Err(Error::InvalidRequest),
    }
    Ok(b.proposals)
}
pub fn emit(request:&ImportRequest,input:impl Read,wal:Option<&mut dyn Read>,mut output:impl Write)->Result<u64> {
    let proposals=extract(request,input,wal)?;let mut bytes=0u64;
    for proposal in &proposals {let encoded=serde_json::to_vec(proposal).map_err(|_|Error::Content)?;
        bytes+=encoded.len() as u64+1;if encoded.len()>64*1024 || bytes>MAX_IMPORT_STREAM_BYTES {return Err(Error::Limit);}
        output.write_all(&encoded)?;output.write_all(b"\n")?;
    }output.flush()?;Ok(proposals.len() as u64)
}

fn users_sqlite(input:impl Read,wal:Option<&mut dyn Read>,builder:&mut Builder<'_>)->Result<()> {
    use rusqlite::{Connection,OpenFlags,hooks::{AuthAction,AuthContext,Authorization}};
    let temp=tempfile::tempdir()?;let path=temp.path().join("source.db");
    crate::sqlite::copy_bounded(input,&path)?;
    if let Some(wal)=wal {crate::sqlite::copy_bounded(wal,&temp.path().join("source.db-wal"))?;}
    let mut conn=Connection::open_with_flags(path,OpenFlags::SQLITE_OPEN_READ_WRITE|OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    crate::sqlite::harden(&mut conn)?;
    check_sqlite(&conn)?;
    let expected=Connection::open_in_memory()?;expected.execute_batch(include_str!("users-v1793.sql"))?;
    if crate::sqlite::schema(&conn)?!=crate::sqlite::schema(&expected)? {return Err(Error::Schema);}
    conn.authorizer(Some(|c:AuthContext<'_>|match c.action {
        AuthAction::Select=>Authorization::Allow,
        AuthAction::Read {table_name:"users",column_name:"id"|"email"|"display_name"|"role"|"department"}
        |AuthAction::Read {table_name:"user_agent_bindings",column_name:"user_id"|"agent_name"|"access_level"}
        |AuthAction::Read {table_name:"channel_identities",column_name:"user_id"|"channel"}
            if c.database_name==Some("main") && c.accessor.is_none()=>Authorization::Allow,
        _=>Authorization::Deny,
    }))?;
    let mut query=conn.prepare("SELECT email,display_name,role FROM users ORDER BY id")?;
    let mut rows=query.query([])?;
    while let Some(row)=rows.next()? {builder.push(C::User,vec![field("name",row.get::<_,String>(0)?),field("display_name",row.get::<_,String>(1)?),field("role",row.get::<_,String>(2)?)],None)?;}
    let mut query=conn.prepare("SELECT u.email,b.agent_name,b.access_level FROM user_agent_bindings b JOIN users u ON u.id=b.user_id ORDER BY b.user_id,b.agent_name")?;
    let mut rows=query.query([])?;
    while let Some(row)=rows.next()? {builder.push(C::UserAgentBinding,vec![field("owner_id",row.get::<_,String>(0)?),field("employee_id",row.get::<_,String>(1)?),field("binding_kind",format!("shared:{}",row.get::<_,String>(2)?))],None)?;}
    let mut query=conn.prepare("SELECT DISTINCT department FROM users WHERE department IS NOT NULL AND department<>'' ORDER BY department")?;
    let mut rows=query.query([])?;
    while let Some(row)=rows.next()? {builder.push(C::Department,vec![field("name",row.get::<_,String>(0)?)],None)?;}
    let mut query=conn.prepare("SELECT u.email,c.channel FROM channel_identities c JOIN users u ON u.id=c.user_id ORDER BY u.email,c.channel")?;
    let mut rows=query.query([])?;
    while let Some(row)=rows.next()? {builder.push(C::ChannelBinding,vec![field("channel",row.get::<_,String>(1)?),
        field("binding",encoded(&serde_json::json!({"owner":row.get::<_,String>(0)?,"requires_new_login":true}))?),field("requires_reentry","true")],None)?;}
    Ok(())
}

#[cfg(test)] mod tests {
    use super::*;
    fn request(kind:SourceKind,title:&str)->ImportRequest {ImportRequest {version:1,migration_id:RequestId([4;16]),employee:None,source_id:"old:authority".into(),kind,title:title.into(),legacy_agent:Some("sales".into()),has_wal:false,blob_handle:None}}
    #[test] fn authority_mcp_extracts_inert_full_command_and_separates_secret() {
        let p=extract(&request(SourceKind::AuthorityJson,".mcp.json"),br#"{"mcpServers":{"planted":{"command":"/bin/echo","args":["canary"],"env":{"API_KEY":"SECRET-CANARY"}}}}"#.as_slice(),None).unwrap();
        assert_eq!(p.len(),1);assert_eq!(p[0].category,C::McpServer);
        assert!(!serde_json::to_string(&p[0].fields).unwrap().contains("SECRET-CANARY"));
        assert!(!serde_json::to_string(&p[0]).unwrap().contains("SECRET-CANARY"));
        assert!(p[0].fields.iter().any(|f|f.name=="requires_reentry"));
    }
    #[test] fn authority_mcp_default_empty_env_remains_actionable() {
        let p=extract(&request(SourceKind::AuthorityJson,".mcp.json"),br#"{"mcpServers":{"ordinary":{"command":"/usr/bin/echo","args":["hello"],"env":{}}}}"#.as_slice(),None).unwrap();
        assert_eq!(p.len(),1);assert!(!p[0].fields.iter().any(|f|f.name=="requires_reentry"));
        assert!(p[0].fields.iter().any(|f|f.name=="env"&&matches!(&f.value,ProposalValue::Text{value} if value=="{}")));
    }
    #[test] fn authority_mcp_url_credentials_never_leave_worker() {
        for url in ["https://example.test/#access_token=SECRET-CANARY",
            "https://example.test/?token=SECRET-CANARY", "https://user:SECRET-CANARY@example.test/"] {
            let input=serde_json::json!({"mcpServers":{"remote":{"type":"http","url":url}}});
            let p=extract(&request(SourceKind::AuthorityJson,".mcp.json"),serde_json::to_vec(&input).unwrap().as_slice(),None).unwrap();
            assert_eq!(p.len(),1);
            assert!(p[0].fields.iter().any(|f|f.name=="requires_reentry"));
            assert!(!serde_json::to_string(&p).unwrap().contains("SECRET-CANARY"));
        }
    }
    #[test] fn authority_mcp_unknown_semantics_require_reentry() {
        for unknown in ["cwd","disabled","future_transport"] {
            let input=serde_json::json!({"mcpServers":{"ordinary":{"command":"/usr/bin/echo","args":[],"env":{},unknown:"SECRET-CANARY"}}});
            let p=extract(&request(SourceKind::AuthorityJson,".mcp.json"),serde_json::to_vec(&input).unwrap().as_slice(),None).unwrap();
            assert!(p[0].fields.iter().any(|f|f.name=="requires_reentry"));
            assert!(!serde_json::to_string(&p).unwrap().contains("SECRET-CANARY"));
        }
    }
    #[test] fn authority_users_never_emits_passwords_tokens_or_mfa() {
        let file=tempfile::NamedTempFile::new().unwrap();let conn=rusqlite::Connection::open(file.path()).unwrap();
        conn.execute_batch(include_str!("users-v1793.sql")).unwrap();
        conn.execute("INSERT INTO users(id,email,display_name,password_hash,role,created_at,updated_at) VALUES ('old-admin','admin@example.com','Planted','PASSWORD-CANARY','admin','now','now')",[]).unwrap();drop(conn);
        let p=extract(&request(SourceKind::AuthorityUsersSqlite,"users.db"),std::fs::File::open(file.path()).unwrap(),None).unwrap();
        assert_eq!(p.len(),1);assert_eq!(p[0].category,C::User);assert!(!serde_json::to_string(&p).unwrap().contains("PASSWORD-CANARY"));
    }
    #[test] fn authority_tasks_only_propose_scheduled_or_open_goal_work() {
        let file=tempfile::NamedTempFile::new().unwrap();let conn=rusqlite::Connection::open(file.path()).unwrap();
        conn.execute_batch(crate::catalog::TASKS_V180).unwrap();
        conn.execute_batch("INSERT INTO tasks(id,title,description,assigned_to,created_at,updated_at,kind,goal_mode) VALUES ('old-goal','Goal','Do work','sales','now','now','goal',1); INSERT INTO tasks(id,title,assigned_to,created_at,updated_at) VALUES ('plain','Historical task','sales','now','now'); INSERT INTO tasks(id,title,description,assigned_to,created_by,created_at,updated_at,kind,goal_mode,a2a_owner,created_via) VALUES ('a2a-goal','A2A-PRIVATE-CANARY','Do work','sales','a2a:user:alice','now','now','goal',1,'user:alice','a2a'); PRAGMA wal_checkpoint(TRUNCATE);").unwrap();drop(conn);
        let p=extract(&request(SourceKind::AuthorityTasksSqlite,"tasks.db"),std::fs::File::open(file.path()).unwrap(),None).unwrap();
        assert_eq!(p.len(),1);assert_eq!(p[0].category,C::Schedule);
        assert!(!serde_json::to_string(&p).unwrap().contains("A2A-PRIVATE-CANARY"));
        assert!(p[0].fields.iter().any(|f|f.name=="employee_id"));
    }
    #[test] fn a2a_goals_excluded_in_both_versions() {
        let insert="INSERT INTO tasks(id,title,description,assigned_to,created_by,created_at,updated_at,kind,goal_mode,created_via,parent_task_id) VALUES (?1,?2,'Do work','sales',?3,'now','now','goal',1,?4,?5)";
        // v1.79.3: no owner column or registry; A2A traces are the creator and channel.
        let file=tempfile::NamedTempFile::new().unwrap();let conn=rusqlite::Connection::open(file.path()).unwrap();
        conn.execute_batch(crate::catalog::TASKS_V1793).unwrap();
        for (id,title,creator,via,parent) in [("ordinary","Ordinary goal","system",None,None),
            ("a2a-creator","A2A-PRIVATE-CREATOR","a2a:user:alice",None,None),
            ("a2a-channel","A2A-PRIVATE-CHANNEL","system",Some("a2a"),None),
            ("a2a-child","A2A-PRIVATE-CHILD","system",None,Some("a2a-creator")),
            ("a2a-grandchild","A2A-PRIVATE-GRANDCHILD","system",None,Some("a2a-child"))] {
            conn.execute(insert,rusqlite::params![id,title,creator,via,parent]).unwrap();
        }
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();drop(conn);
        let p=extract(&request(SourceKind::AuthorityTasksSqlite,"tasks.db"),std::fs::File::open(file.path()).unwrap(),None).unwrap();
        assert_eq!(p.len(),1);assert!(serde_json::to_string(&p).unwrap().contains("Ordinary goal"));
        assert!(!serde_json::to_string(&p).unwrap().contains("A2A-PRIVATE"));
        // v1.80: owner column, registry and inherited owners as well.
        let file=tempfile::NamedTempFile::new().unwrap();let conn=rusqlite::Connection::open(file.path()).unwrap();
        conn.execute_batch(crate::catalog::TASKS_V180).unwrap();
        conn.execute("INSERT INTO tasks(id,title,description,assigned_to,created_by,created_at,updated_at,kind,goal_mode,a2a_owner,created_via) VALUES ('a2a-owned','A2A-PRIVATE-OWNED','Do work','sales','a2a:user:alice','now','now','goal',1,'user:alice','a2a')",[]).unwrap();
        for (id,title,creator,via,parent) in [("ordinary","Ordinary goal","system",None,None),
            ("a2a-child","A2A-PRIVATE-CHILD","system",None,Some("a2a-owned")),
            ("a2a-creator","A2A-PRIVATE-CREATOR","a2a:user:bob",None,None),
            ("a2a-channel","A2A-PRIVATE-CHANNEL","system",Some("a2a"),None)] {
            conn.execute(insert,rusqlite::params![id,title,creator,via,parent]).unwrap();
        }
        // A registry row alone (owner column cleared by a hostile writer) still excludes.
        conn.execute_batch("INSERT INTO tasks(id,title,assigned_to,created_at,updated_at,kind,goal_mode) VALUES ('registry-only','A2A-PRIVATE-REGISTRY','sales','now','now','goal',1);
            INSERT INTO task_access_principals VALUES ('registry-only','user:carol'); PRAGMA wal_checkpoint(TRUNCATE);").unwrap();drop(conn);
        let p=extract(&request(SourceKind::AuthorityTasksSqlite,"tasks.db"),std::fs::File::open(file.path()).unwrap(),None).unwrap();
        assert_eq!(p.len(),1);assert!(serde_json::to_string(&p).unwrap().contains("Ordinary goal"));
        assert!(!serde_json::to_string(&p).unwrap().contains("A2A-PRIVATE"));
    }
    #[test] fn authority_secrets_in_all_reviewed_containers_never_leave_worker() {
        let secret_keys=["bot_token","api_hash","signing_secret","app_token","channel_secret","openai_api_key","arbitrary_enc","Service_Account_Json"];
        for key in secret_keys {
            let text=format!(r#"{{"PreToolUse":[{{"matcher":"Read","hooks":[{{"type":"command","command":"node safe.js","{key}":"SECRET-CANARY"}}]}}]}}"#);
            let p=extract(&request(SourceKind::AuthorityJson,"hooks.json"),text.as_bytes(),None).unwrap();
            assert!(!serde_json::to_string(&p).unwrap().contains("SECRET-CANARY"));
            assert!(p[0].fields.iter().any(|f|f.name=="requires_reentry"));
        }
        for (title,kind,input) in [
            ("config.toml",SourceKind::AuthorityToml,"[unknown]
innocent='SECRET-CANARY'
"),
            ("connectors.json",SourceKind::AuthorityJson,r#"{"example":{"unknown":{"nested":"SECRET-CANARY"}}}"#),
            ("channel_bindings.json",SourceKind::AuthorityJson,r#"[{"agent_id":"sales","channel":"telegram","bot_token":"SECRET-CANARY"}]"#),
        ] {
            let p=extract(&request(kind,title),input.as_bytes(),None).unwrap();
            assert!(!serde_json::to_string(&p).unwrap().contains("SECRET-CANARY"));
            assert!(p[0].fields.iter().any(|f|f.name=="requires_reentry"));
        }
    }
    #[test] fn authority_users_and_tasks_refuse_version_views_and_triggers() {
        for (kind,title,schema) in [(SourceKind::AuthorityUsersSqlite,"users.db",include_str!("users-v1793.sql")),(SourceKind::AuthorityTasksSqlite,"tasks.db",crate::catalog::TASKS_V1793),(SourceKind::AuthorityTasksSqlite,"tasks.db",crate::catalog::TASKS_V180),(SourceKind::AuthorityTasksSqlite,"tasks.db",crate::catalog::TASKS_V181)] {
            for mutation in ["PRAGMA user_version=1", "CREATE VIEW planted AS SELECT 1", "CREATE TABLE planted_authority(token TEXT)"] {
                let file=tempfile::NamedTempFile::new().unwrap();let conn=rusqlite::Connection::open(file.path()).unwrap();conn.execute_batch(schema).unwrap();conn.execute_batch(mutation).unwrap();conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();drop(conn);
                assert!(extract(&request(kind,title),std::fs::File::open(file.path()).unwrap(),None).is_err());
            }
        }
    }
    #[test] fn authority_unknown_source_and_oversize_refuse() {
        assert!(extract(&request(SourceKind::AuthorityJson,"unclassified.json"),b"{}".as_slice(),None).is_err());
        assert!(extract(&request(SourceKind::AuthorityInstructions,"SOUL.md"),vec![b'x';65537].as_slice(),None).is_err());
    }
}

fn tasks_sqlite(input:impl Read,wal:Option<&mut dyn Read>,builder:&mut Builder<'_>)->Result<()> {
    use rusqlite::{Connection,OpenFlags,hooks::{AuthAction,AuthContext,Authorization}};
    let temp=tempfile::tempdir()?;let path=temp.path().join("source.db");
    crate::sqlite::copy_bounded(input,&path)?;
    if let Some(wal)=wal {crate::sqlite::copy_bounded(wal,&temp.path().join("source.db-wal"))?;}
    let mut conn=Connection::open_with_flags(path,OpenFlags::SQLITE_OPEN_READ_WRITE|OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    crate::sqlite::harden(&mut conn)?;
    // The reviewed task authority trigger covers more than 32 fields. This
    // fixed bound admits that catalog while retaining a small parser ceiling.
    conn.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_EXPR_DEPTH,128)?;
    check_sqlite(&conn)?;
    let version=crate::catalog::detect(&conn,crate::catalog::Store::Tasks,None)?.version;
    // V181 is a sessions.db-only mapping (the v1.81 tasks catalog maps as
    // V180); a tasks store that ever yielded it is refused (fail closed).
    let query_sql=task_goal_query(version)?;
    conn.authorizer(Some(move |c:AuthContext<'_>| {
        // SQLite names the FROM-clause subquery being resolved as the accessor,
        // so reads inside the CTE body arrive with accessor `a2a`, not None.
        // `a2a` is the fixed name of the CTE in `task_goal_query`. Views and
        // triggers are disabled and refused by the catalog check, so no source
        // object can borrow the name. Only the trace columns get this grant.
        let top=c.accessor.is_none();
        let a2a_cte=c.accessor==Some("a2a");
        match c.action {
        // The A2A exclusion follows parent links with a recursive CTE.
        AuthAction::Select|AuthAction::Recursive=>Authorization::Allow,
        AuthAction::Read {table_name:"tasks",column_name:"id"|"title"|"description"|"assigned_to"|"kind"|"goal_mode"|"status"|"acceptance_criteria"|"created_via"|"created_by"|"parent_task_id"}
        |AuthAction::Read {table_name:"assign_schedules",column_name:"id"|"agent_id"|"title"|"payload_json"|"schedule_json"|"status"|"timezone"}
        |AuthAction::Read {table_name:"responsibilities",column_name:"responsibility_id"|"owner_agent_id"|"objective"|"acceptance_template"|"scope_json"|"notification_policy_json"|"schedule_json"|"occurrence_hours"|"occurrence_cost_cap_cents"|"budget_period"|"budget_timezone"|"period_cost_limit_cents"|"period_occurrence_limit"|"min_wake_interval_secs"|"max_consecutive_failures"|"stop_at"|"state"}
            if c.database_name==Some("main") && top=>Authorization::Allow,
        // Columns the CTE body reads: the task link and the A2A traces.
        AuthAction::Read {table_name:"tasks",column_name:"id"|"created_via"|"created_by"|"parent_task_id"}
            if c.database_name==Some("main") && a2a_cte=>Authorization::Allow,
        // Owner columns exist only in the v1.80 catalog (and the v1.81 one,
        // which maps as V180; V181 itself is refused above).
        AuthAction::Read {table_name:"tasks",column_name:"a2a_owner"}
        |AuthAction::Read {table_name:"task_access_principals",column_name:"task_id"}
            if version==crate::catalog::SchemaVersion::V180 && c.database_name==Some("main") && (top||a2a_cte)=>Authorization::Allow,
        _=>Authorization::Deny,
        }
    }))?;
    // A2A-owned work is private to its external principal; republishing it as
    // ordinary employee goals would drop that owner restriction. It stays quarantined.
    let mut query=conn.prepare(query_sql)?;
    let mut rows=query.query([])?;
    while let Some(row)=rows.next()? {
        let settings=serde_json::json!({"kind":"goal","title":row.get::<_,String>(1)?,"description":row.get::<_,String>(2)?,"acceptance_criteria":row.get::<_,Option<String>>(4)?});
        builder.settings(C::Schedule,&row.get::<_,String>(0)?,settings,vec![field("employee_id",row.get::<_,String>(3)?)])?;
    }
    let mut query=conn.prepare("SELECT id,agent_id,title,payload_json,schedule_json,timezone FROM assign_schedules WHERE status NOT IN ('done','removed') ORDER BY id")?;
    let mut rows=query.query([])?;
    while let Some(row)=rows.next()? {
        let settings=serde_json::json!({"kind":"assign_schedule","title":row.get::<_,String>(2)?,"payload":serde_json::from_str::<Value>(&row.get::<_,String>(3)?).map_err(|_|Error::Content)?,"schedule":serde_json::from_str::<Value>(&row.get::<_,String>(4)?).map_err(|_|Error::Content)?,"timezone":row.get::<_,Option<String>>(5)?});
        builder.settings(C::Schedule,&row.get::<_,String>(0)?,settings,vec![field("employee_id",row.get::<_,String>(1)?)])?;
    }
    let mut query=conn.prepare("SELECT responsibility_id,owner_agent_id,objective,acceptance_template,scope_json,notification_policy_json,schedule_json,occurrence_hours,occurrence_cost_cap_cents,budget_period,budget_timezone,period_cost_limit_cents,period_occurrence_limit,min_wake_interval_secs,max_consecutive_failures,stop_at FROM responsibilities WHERE state!='expired' ORDER BY responsibility_id")?;
    let mut rows=query.query([])?;
    while let Some(row)=rows.next()? {
        let mut settings=serde_json::Map::new();settings.insert("kind".into(),Value::String("responsibility".into()));
        for (index,name) in [(2,"objective"),(3,"acceptance_template"),(9,"budget_period"),(10,"budget_timezone"),(15,"stop_at")] {settings.insert(name.into(),Value::String(row.get(index)?));}
        for (index,name) in [(4,"scope"),(5,"notification_policy"),(6,"schedule")] {settings.insert(name.into(),match row.get::<_,Option<String>>(index)? {Some(v)=>serde_json::from_str(&v).map_err(|_|Error::Content)?,None=>Value::Null});}
        for (index,name) in [(7,"occurrence_hours"),(8,"occurrence_cost_cap_cents"),(11,"period_cost_limit_cents"),(12,"period_occurrence_limit"),(13,"min_wake_interval_secs"),(14,"max_consecutive_failures")] {settings.insert(name.into(),Value::from(row.get::<_,i64>(index)?));}
        builder.settings(C::Schedule,&row.get::<_,String>(0)?,Value::Object(settings),vec![field("employee_id",row.get::<_,String>(1)?)])?;
    }
    Ok(())
}

/// Open goals that become Schedule proposals, per source version. A goal is
/// A2A-owned (and excluded) when it, or any ancestor through
/// `parent_task_id`, carries an A2A trace: v1.80 `a2a_owner`, a
/// `task_access_principals` row, `created_via='a2a'`, or an `a2a:` creator
/// (the gateway's own pre-registry rule). v1.79.3 has neither owner column nor
/// registry, so only the last two traces apply. The prefix test is a range
/// comparison so no SQL function needs authorizing. `V181` never comes from
/// tasks.db (its v1.81 catalog maps as V180) and is refused: fail closed.
fn task_goal_query(version:crate::catalog::SchemaVersion)->Result<&'static str> {
    use crate::catalog::SchemaVersion as V;
    Ok(match version {
        V::V181=>return Err(Error::Schema),
        V::V1793=>"WITH RECURSIVE a2a(id) AS (
                SELECT id FROM main.tasks WHERE created_via IS 'a2a' OR (created_by>='a2a:' AND created_by<'a2a;')
                UNION SELECT t.id FROM main.tasks AS t JOIN a2a ON t.parent_task_id=a2a.id)
            SELECT id,title,description,assigned_to,acceptance_criteria FROM main.tasks AS g
            WHERE (kind='goal' OR goal_mode=1) AND status NOT IN ('done','cancelled','failed')
                AND NOT EXISTS (SELECT 1 FROM a2a WHERE a2a.id=g.id) ORDER BY id",
        // The v1.81 tasks catalog maps as V180.
        V::V180=>"WITH RECURSIVE a2a(id) AS (
                SELECT id FROM main.tasks WHERE a2a_owner IS NOT NULL OR created_via IS 'a2a' OR (created_by>='a2a:' AND created_by<'a2a;')
                    OR id IN (SELECT task_id FROM main.task_access_principals)
                UNION SELECT t.id FROM main.tasks AS t JOIN a2a ON t.parent_task_id=a2a.id)
            SELECT id,title,description,assigned_to,acceptance_criteria FROM main.tasks AS g
            WHERE (kind='goal' OR goal_mode=1) AND status NOT IN ('done','cancelled','failed')
                AND NOT EXISTS (SELECT 1 FROM a2a WHERE a2a.id=g.id) ORDER BY id",
    })
}

fn check_sqlite(conn:&rusqlite::Connection)->Result<()> {
    let version:u32=conn.query_row("PRAGMA user_version",[],|r|r.get(0))?;
    if version!=0 {return Err(Error::Schema);}
    let check:String=conn.query_row("PRAGMA quick_check",[],|r|r.get(0))?;
    if check!="ok" {
        // Test builds only: the first quick_check finding behind `Content`.
        #[cfg(test)] eprintln!("legacy quick_check: {check}");
        return Err(Error::Content);
    }
    Ok(())
}
