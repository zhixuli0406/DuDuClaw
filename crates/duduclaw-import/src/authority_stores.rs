//! Readers for actual v1.79.3 authority stores. Credential material is never
//! decrypted, emitted, or used as evidence of authority in the new installation.
use super::*;

fn label(value:&Value,key:&str)->Result<String> {
    let value=member(value,key)?;
    if value.is_empty() || value.len()>128 || value.chars().any(char::is_control) {return Err(Error::Content);}
    Ok(value.into())
}
fn agent(value:&Value,key:&str)->Result<String> {
    let value=label(value,key)?;
    if value.len()>64 || !value.bytes().all(|b|b.is_ascii_alphanumeric()||matches!(b,b'-'|b'_')) {return Err(Error::Content);}
    Ok(value)
}
fn reentry(builder:&mut Builder<'_>,category:C,name:&str,mut extra:Vec<ProposalField>)->Result<()> {
    extra.push(field("name",name));
    extra.push(field(if category==C::Policy {"rules"}else{"settings"},""));
    extra.push(field("requires_reentry","true"));
    builder.push(category,extra,None)
}
/// A named obligation for a recognized store whose secrets, trust baselines or
/// historical schema must be recreated. Deliberately reads none of its bytes.
pub(super) fn notice(builder:&mut Builder<'_>)->Result<()> {
    let title=builder.request.title.as_str();
    let (category,name)=match title {
        "website_credentials.enc"=>(C::Connector,"website_credentials"),
        "soul_baseline"=>(C::Policy,"soul_baseline"),
        "notice:hook"=>(C::Hook,"legacy_hooks"),
        "notice:policy"=>(C::Policy,"legacy_policy"),
        "notice:connector"=>(C::Connector,"legacy_connector"),
        "notice:data_source"=>(C::DataSourceAuthorization,"legacy_data_source"),
        "notice:schedule"=>(C::Schedule,"legacy_schedule"),
        "notice:skill"=>(C::Skill,"legacy_skill_artifact"),
        _=>return Err(Error::InvalidRequest),
    };
    let mut fields=vec![field("name",name),field("requires_reentry","true")];
    if matches!(category,C::Hook|C::Policy|C::DataSourceAuthorization|C::Schedule|C::Skill) {
        if let Some(agent)=builder.request.legacy_agent.as_deref() {fields.push(field("employee_id",agent));}
    }
    match category {
        C::Policy=>fields.push(field("rules","")),
        C::Skill=>{fields.push(field("artifact_digest",""));fields.push(field("text",""));},
        C::Hook=>{fields.push(field("event","manual_review"));fields.push(field("settings",""));},
        _=>fields.push(field("settings","")),
    }
    builder.push(category,fields,None)
}

pub(super) fn json(title:&str,value:&Value,builder:&mut Builder<'_>)->Result<bool> {
    match title {
        "remote_mcp/servers.json"=>{
            if value.get("version").and_then(Value::as_u64).unwrap_or(1)!=1 {return Err(Error::Schema);}
            for server in value.get("servers").and_then(Value::as_array).ok_or(Error::Content)? {
                let owner=agent(server,"agent_id")?;let name=label(server,"server")?;
                if !matches!(member(server,"auth")?,"none"|"bearer"|"oauth") {return Err(Error::Content);}
                // Full URL, credentials and header values are sealed together.
                // Reconnecting uses fresh values and a new owner confirmation.
                builder.push(C::McpServer,vec![field("employee_id",owner),field("name",name),field("transport","http"),
                    field("url",""),field("requires_reentry","true")],None)?;
            }
        }
        "person_credentials/store.json"=>{
            if value.get("version").and_then(Value::as_u64)!=Some(1) {return Err(Error::Schema);}
            for record in value.get("records").and_then(Value::as_array).ok_or(Error::Content)? {
                if member(record,"owner_kind")?!="user" {return Err(Error::Content);}
                let provider=label(record,"provider")?;let owner=label(record,"owner_id")?;
                reentry(builder,C::Connector,&format!("personal:{provider}"),vec![field("owner_id",owner),field("binding_kind","personal_requires_login")])?;
            }
        }
        "agent_bindings.json"=>{
            for (channel,users) in value.get("bindings").and_then(Value::as_object).ok_or(Error::Content)? {
                if channel.is_empty() || channel.len()>64 || !channel.bytes().all(|b|b.is_ascii_lowercase()||b==b'_') {return Err(Error::Content);}
                for (user,target) in users.as_object().ok_or(Error::Content)? {
                    let owner=agent(&serde_json::json!({"agent":target}),"agent")?;
                    if user.is_empty() || user.len()>128 || user.chars().any(char::is_control) {return Err(Error::Content);}
                    let mut fields=vec![field("employee_id",owner),field("channel",channel),field("binding",encoded(&serde_json::json!({"external_user_id":user}))?)];
                    if channel!="telegram" {fields.push(field("requires_reentry","true"));}
                    builder.push(C::ChannelBinding,fields,None)?;
                }
            }
            // Pending bind-token digests are deliberately never inspected.
        }
        "access_control.json"=>{reentry(builder,C::Policy,"channel_pairing",vec![])?;}
        "mcp-oauth-configs.json"|"mcp-oauth-tokens.json"=>{
            for record in value.as_array().ok_or(Error::Content)? {
                let provider=label(record,"provider_id")?;
                reentry(builder,C::Connector,&format!("oauth:{provider}"),vec![field("binding_kind","shared_reconnect")])?;
            }
        }
        _=>return Ok(false),
    }
    Ok(true)
}

pub(super) fn organization(value:&Value,builder:&mut Builder<'_>)->Result<()> {
    if value.get("schema").and_then(Value::as_u64).unwrap_or(1)!=1 {return Err(Error::Schema);}
    let mut clean=serde_json::Map::new();let mut departments=std::collections::BTreeSet::new();
    let mut unknown=value.as_object().ok_or(Error::Content)?.keys().any(|k|!matches!(k.as_str(),"schema"|"agents"));
    if let Some(agents)=value.get("agents") {
        for (id,entry) in agents.as_object().ok_or(Error::Content)? {
            let id=agent(&serde_json::json!({"agent":id}),"agent")?;
            let object=entry.as_object().ok_or(Error::Content)?;
            unknown|=object.keys().any(|k|!matches!(k.as_str(),"reports_to"|"department"));
            let department=entry.get("department").map(|v|v.as_str().ok_or(Error::Content)).transpose()?.unwrap_or("");
            let manager=entry.get("reports_to").map(|v|v.as_str().ok_or(Error::Content)).transpose()?.unwrap_or("");
            if department.len()>128 || manager.len()>64 {return Err(Error::Limit);}
            if !department.is_empty() {departments.insert(department.to_string());}
            clean.insert(id,serde_json::json!({"reports_to":manager,"department":department}));
        }
    }
    builder.settings(C::Policy,"org",serde_json::json!({"schema":1,"agents":clean}),if unknown {vec![field("requires_reentry","true")]} else {vec![]})?;
    for department in departments {builder.push(C::Department,vec![field("name",department)],None)?;}
    Ok(())
}

pub(super) fn channel_settings(input:impl Read,wal:Option<&mut dyn Read>,builder:&mut Builder<'_>)->Result<()> {
    use rusqlite::{Connection,OpenFlags,hooks::{AuthAction,AuthContext,Authorization}};
    let temp=tempfile::tempdir()?;let path=temp.path().join("source.db");
    crate::sqlite::copy_bounded(input,&path)?;
    if let Some(wal)=wal {crate::sqlite::copy_bounded(wal,&temp.path().join("source.db-wal"))?;}
    let conn=Connection::open_with_flags(path,OpenFlags::SQLITE_OPEN_READ_WRITE|OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    crate::sqlite::harden(&conn)?;check_sqlite(&conn)?;
    if !crate::sqlite::session_schema_variant(&conn)? {return Ok(());}
    conn.authorizer(Some(|c:AuthContext<'_>|match c.action {
        AuthAction::Select=>Authorization::Allow,
        AuthAction::Read {table_name:"channel_settings",column_name:"channel_type"}
            if c.database_name==Some("main") && c.accessor.is_none()=>Authorization::Allow,
        _=>Authorization::Deny,
    }));
    let mut query=conn.prepare("SELECT DISTINCT channel_type FROM main.channel_settings ORDER BY channel_type")?;
    let mut rows=query.query([])?;
    while let Some(row)=rows.next()? {
        let channel:String=row.get(0)?;
        if channel.is_empty() || channel.len()>64 || !channel.bytes().all(|b|b.is_ascii_lowercase()||b==b'_') {return Err(Error::Content);}
        reentry(builder,C::Policy,&format!("channel_settings:{channel}"),vec![])?;
    }
    Ok(())
}

pub(super) fn approvals(input:impl Read,wal:Option<&mut dyn Read>,builder:&mut Builder<'_>)->Result<()> {
    use rusqlite::{Connection,OpenFlags,hooks::{AuthAction,AuthContext,Authorization}};
    let temp=tempfile::tempdir()?;let path=temp.path().join("source.db");
    crate::sqlite::copy_bounded(input,&path)?;
    if let Some(wal)=wal {crate::sqlite::copy_bounded(wal,&temp.path().join("source.db-wal"))?;}
    let conn=Connection::open_with_flags(path,OpenFlags::SQLITE_OPEN_READ_WRITE|OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    crate::sqlite::harden(&conn)?;check_sqlite(&conn)?;
    let expected=Connection::open_in_memory()?;
    expected.execute_batch(include_str!("approvals-v1793.sql"))?;
    if crate::sqlite::schema(&conn)?!=crate::sqlite::schema(&expected)? {return Err(Error::Schema);}
    conn.authorizer(Some(|c:AuthContext<'_>|match c.action {
        AuthAction::Select=>Authorization::Allow,
        AuthAction::Read {table_name:"approvals",column_name:"id"}
        |AuthAction::Read {table_name:"approval_authority_schema",column_name:"version"}
            if c.database_name==Some("main") && c.accessor.is_none()=>Authorization::Allow,
        _=>Authorization::Deny,
    }));
    let versions:Vec<i64>=conn.prepare("SELECT version FROM main.approval_authority_schema")?.query_map([],|r|r.get(0))?.collect::<std::result::Result<_,_>>()?;
    if versions!=[1] {return Err(Error::Schema);}
    let mut query=conn.prepare("SELECT id FROM main.approvals ORDER BY id")?;
    let mut rows=query.query([])?;
    while rows.next()?.is_some() {
        // Do not retain even old ids: summary, payload, grants, tokens, status,
        // decisions and executing-operation leases all stay in quarantine.
        builder.push(C::LegacyApproval,vec![field("reference",format!("{}:approval:{}",builder.request.source_id,builder.proposals.len()))],None)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(kind:SourceKind,title:&str)->ImportRequest {ImportRequest {version:1,migration_id:RequestId([4;16]),employee:None,source_id:"old:store".into(),kind,title:title.into(),legacy_agent:Some("sales".into()),has_wal:false,blob_handle:None}}
    fn approval_db()->tempfile::NamedTempFile {
        let file=tempfile::NamedTempFile::new().unwrap();let conn=rusqlite::Connection::open(file.path()).unwrap();
        conn.execute_batch(include_str!("approvals-v1793.sql")).unwrap();
        for (id,status) in [("a","pending"),("b","approved"),("c","denied")] {
            conn.execute("INSERT INTO approvals(id,agent_id,action_kind,summary,payload,status,created_at) VALUES (?1,'sales','SECRET-KIND','SECRET-SUMMARY','{\"token\":\"SECRET-PAYLOAD\"}',?2,'now')",[id,status]).unwrap();
        }
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();drop(conn);file
    }
    #[test]
    fn authority_approvals_db_all_statuses_become_inert_expired_candidates() {
        let file=approval_db();let p=extract(&request(SourceKind::AuthorityApprovalsSqlite,"approvals.db"),std::fs::File::open(file.path()).unwrap(),None).unwrap();
        assert_eq!(p.len(),3);assert!(p.iter().all(|p|p.category==C::LegacyApproval&&p.fields.len()==1&&p.fields[0].name=="reference"));
        assert!(!serde_json::to_string(&p).unwrap().contains("SECRET"));
        let conn=rusqlite::Connection::open(file.path()).unwrap();
        assert_eq!(conn.query_row("SELECT count(*) FROM approvals WHERE status='approved'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    }
    #[test]
    fn authority_approvals_db_rejects_added_trigger_and_version_drift() {
        for extra in ["CREATE TRIGGER malicious AFTER INSERT ON approvals BEGIN SELECT 1; END;","UPDATE approval_authority_schema SET version=2;"] {
            let file=approval_db();let conn=rusqlite::Connection::open(file.path()).unwrap();conn.execute_batch(extra).unwrap();drop(conn);
            assert!(extract(&request(SourceKind::AuthorityApprovalsSqlite,"approvals.db"),std::fs::File::open(file.path()).unwrap(),None).is_err());
        }
    }
    #[test]
    fn authority_connector_stores_keep_only_reentry_metadata() {
        let fixtures=[
            ("remote_mcp/servers.json",serde_json::json!({"version":1,"servers":[{"agent_id":"sales","server":"crm","auth":"oauth","host":"SECRET-HOST","secret_enc":"SECRET-CIPHER","header_names":["SECRET-HEADER"]}]}),C::McpServer),
            ("person_credentials/store.json",serde_json::json!({"version":1,"records":[{"owner_kind":"user","owner_id":"old-user","provider":"google","secret":"SECRET-CIPHER","access_token":"SECRET-TOKEN"}]}),C::Connector),
            ("mcp-oauth-configs.json",serde_json::json!([{"provider_id":"google","client_id":"SECRET-CLIENT","client_secret":"SECRET-CLIENT-SECRET"}]),C::Connector),
            ("mcp-oauth-tokens.json",serde_json::json!([{"provider_id":"notion","access_token":"SECRET-ACCESS","refresh_token":"SECRET-REFRESH"}]),C::Connector),
        ];
        for (title,value,category) in fixtures {
            let p=extract(&request(SourceKind::AuthorityJson,title),serde_json::to_vec(&value).unwrap().as_slice(),None).unwrap();
            assert_eq!(p.len(),1);assert_eq!(p[0].category,category);
            assert!(p[0].fields.iter().any(|f|f.name=="requires_reentry"));
            assert!(!serde_json::to_string(&p).unwrap().contains("SECRET"));
        }
    }
    #[test]
    fn authority_real_channel_bindings_omit_one_time_tokens() {
        let raw=serde_json::json!({"bindings":{"telegram":{"123":"sales"}},"tokens":{"SECRET-TOKEN":{"agent_id":"admin"}}});
        let p=extract(&request(SourceKind::AuthorityJson,"agent_bindings.json"),serde_json::to_vec(&raw).unwrap().as_slice(),None).unwrap();
        assert_eq!(p.len(),1);assert_eq!(p[0].category,C::ChannelBinding);
        assert!(!serde_json::to_string(&p).unwrap().contains("SECRET-TOKEN"));
        assert!(p[0].fields.iter().any(|f|f.name=="binding" && matches!(&f.value,ProposalValue::Text {value} if value=="{\"external_user_id\":\"123\"}")));
    }
    #[test]
    fn authority_session_channel_settings_need_reentry_without_exporting_values() {
        let file=tempfile::NamedTempFile::new().unwrap();let conn=rusqlite::Connection::open(file.path()).unwrap();
        conn.execute_batch(crate::sqlite::SESSION_SCHEMA).unwrap();conn.execute_batch(crate::sqlite::CHANNEL_SETTINGS_SCHEMA).unwrap();
        conn.execute_batch("INSERT INTO channel_settings VALUES('telegram','SECRET-USER','allowed_users','SECRET-VALUE','now'); PRAGMA wal_checkpoint(TRUNCATE);").unwrap();drop(conn);
        let p=extract(&request(SourceKind::AuthoritySessionsSqlite,"sessions.db"),std::fs::File::open(file.path()).unwrap(),None).unwrap();
        assert_eq!(p.len(),1);assert_eq!(p[0].category,C::Policy);assert!(!serde_json::to_string(&p).unwrap().contains("SECRET"));
        // This real channel-bearing catalog is also accepted by the content
        // importer; policy rows themselves never become conversation content.
        let mut r=request(SourceKind::SessionsSqlite,"sessions.db");r.employee=Some(duduclaw_isolation::ids::EmployeeId([5;16]));
        let mut output=Vec::new();assert_eq!(crate::import(&r,std::fs::File::open(file.path()).unwrap(),None,&mut output).unwrap(),0);
        assert!(output.is_empty());
    }
    #[test]
    fn authority_actual_org_departments_and_user_channel_identities_are_proposals() {
        let p=extract(&request(SourceKind::AuthorityToml,"org.toml"),b"schema=1\n[agents.sales]\nreports_to='boss'\ndepartment='sales'\nunknown='SECRET-UNKNOWN'\n".as_slice(),None).unwrap();
        assert_eq!(p.len(),2);assert!(p.iter().any(|p|p.category==C::Department));assert!(!serde_json::to_string(&p).unwrap().contains("SECRET-UNKNOWN"));
        let file=tempfile::NamedTempFile::new().unwrap();let conn=rusqlite::Connection::open(file.path()).unwrap();
        conn.execute_batch(include_str!("users-v1793.sql")).unwrap();
        conn.execute_batch("INSERT INTO users(id,email,display_name,password_hash,role,created_at,updated_at,department) VALUES('u','u@example.com','User','SECRET-PASSWORD','admin','now','now','sales'); INSERT INTO channel_identities(user_id,channel,channel_user_id,verified,created_at) VALUES('u','telegram','SECRET-OLD-LOGIN',1,'now'); PRAGMA wal_checkpoint(TRUNCATE);").unwrap();drop(conn);
        let p=extract(&request(SourceKind::AuthorityUsersSqlite,"users.db"),std::fs::File::open(file.path()).unwrap(),None).unwrap();
        assert_eq!(p.len(),3);assert!(p.iter().any(|p|p.category==C::Department));assert!(p.iter().any(|p|p.category==C::ChannelBinding));
        assert!(!serde_json::to_string(&p).unwrap().contains("SECRET"));
    }
    #[test]
    fn authority_notice_never_reads_credential_or_baseline_bytes() {
        struct NeverRead;
        impl Read for NeverRead {fn read(&mut self,_:&mut [u8])->std::io::Result<usize>{panic!("notice must not read secret bytes")}}
        for title in ["website_credentials.enc","soul_baseline","notice:hook","notice:policy","notice:connector","notice:data_source","notice:schedule","notice:skill"] {
            let p=extract(&request(SourceKind::AuthorityNotice,title),NeverRead,None).unwrap();
            assert_eq!(p.len(),1);assert!(p[0].fields.iter().any(|f|f.name=="requires_reentry"));
        }
        assert!(extract(&request(SourceKind::AuthorityNotice,"arbitrary"),NeverRead,None).is_err());
    }
}
