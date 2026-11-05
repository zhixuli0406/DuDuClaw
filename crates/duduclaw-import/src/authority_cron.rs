//! Actual CronStore / pre-SQLite JSONL readers. Only pure time schedules become
//! actionable proposals. Execution history, actor grants and delivery routing
//! never cross this boundary; workflow and command triggers need fresh setup.
use super::*;

fn propose(builder:&mut Builder<'_>,name:&str,owner:&str,schedule:&str,objective:&str,timezone:Option<&str>,pure_time:bool)->Result<()> {
    if name.is_empty() || name.len()>200 || name.chars().any(char::is_control) {return Err(Error::Content);}
    if !pure_time {
        return builder.push(C::Schedule,vec![field("name",name),field("settings",""),field("requires_reentry","true")],None);
    }
    if owner.is_empty() || owner.len()>64 || !owner.bytes().all(|b|b.is_ascii_alphanumeric()||matches!(b,b'-'|b'_'))
        || schedule.is_empty() || schedule.len()>256 || schedule.chars().any(char::is_control)
        || objective.trim().is_empty() || objective.len()>32*1024 || objective.chars().any(|c|c=='\0')
        || timezone.is_some_and(|tz|tz.is_empty()||tz.len()>128||tz.chars().any(char::is_control)) {return Err(Error::Content);}
    let mut settings=serde_json::json!({"kind":"time"});
    if let Some(timezone)=timezone {settings["timezone"]=Value::String(timezone.into());}
    builder.push(C::Schedule,vec![field("employee_id",owner),field("name",name),field("schedule",schedule),
        field("objective",objective),field("settings",encoded(&settings)?)],None)
}

pub(super) fn sqlite(input:impl Read,wal:Option<&mut dyn Read>,builder:&mut Builder<'_>)->Result<()> {
    use rusqlite::{Connection,OpenFlags,hooks::{AuthAction,AuthContext,Authorization}};
    let temp=tempfile::tempdir()?;let path=temp.path().join("source.db");
    crate::sqlite::copy_bounded(input,&path)?;
    if let Some(wal)=wal {crate::sqlite::copy_bounded(wal,&temp.path().join("source.db-wal"))?;}
    let conn=Connection::open_with_flags(path,OpenFlags::SQLITE_OPEN_READ_WRITE|OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    crate::sqlite::harden(&conn)?;check_sqlite(&conn)?;
    let expected=Connection::open_in_memory()?;expected.execute_batch(include_str!("cron-v1793.sql"))?;
    if crate::sqlite::schema(&conn)?!=crate::sqlite::schema(&expected)? {return Err(Error::Schema);}
    conn.authorizer(Some(|c:AuthContext<'_>|match c.action {
        AuthAction::Select=>Authorization::Allow,
        AuthAction::Read {table_name:"cron_tasks",column_name:"id"|"name"|"agent_id"|"cron"|"task"|"cron_timezone"|"trigger_kind"|"condition_script"|"watch_command"}
        |AuthAction::Read {table_name:"cron_workflow_bindings",column_name:"cron_id"}
            if c.database_name==Some("main") && c.accessor.is_none()=>Authorization::Allow,
        _=>Authorization::Deny,
    }));
    let mut query=conn.prepare("SELECT t.name,t.agent_id,t.cron,t.task,t.cron_timezone,t.trigger_kind,
        CASE WHEN t.condition_script IS NOT NULL AND t.condition_script<>'' THEN 1 ELSE 0 END,
        CASE WHEN t.watch_command IS NOT NULL AND t.watch_command<>'' THEN 1 ELSE 0 END,
        b.cron_id FROM main.cron_tasks t LEFT JOIN main.cron_workflow_bindings b ON b.cron_id=t.id ORDER BY t.id")?;
    let mut rows=query.query([])?;
    while let Some(row)=rows.next()? {
        let name:String=row.get(0)?;let owner:String=row.get(1)?;let schedule:String=row.get(2)?;let objective:String=row.get(3)?;
        let timezone:Option<String>=row.get(4)?;let trigger:Option<String>=row.get(5)?;
        let condition:bool=row.get(6)?;let watch:bool=row.get(7)?;let workflow:Option<String>=row.get(8)?;
        let pure_time=matches!(trigger.as_deref(),None|Some("time")) && !condition && !watch && workflow.is_none();
        propose(builder,&name,&owner,&schedule,&objective,timezone.as_deref().filter(|tz|!tz.is_empty()),pure_time)?;
    }
    Ok(())
}

fn string<'a>(value:&'a Value,key:&str)->Result<Option<&'a str>> {
    match value.get(key) {None|Some(Value::Null)=>Ok(None),Some(Value::String(v))=>Ok(Some(v)),_=>Err(Error::Content)}
}
pub(super) fn jsonl(input:impl Read,builder:&mut Builder<'_>)->Result<()> {
    use std::io::{BufRead,BufReader};
    let mut input=BufReader::new(input.take(crate::MAX_SOURCE_BYTES+1));let mut total=0u64;
    let mut seen=std::collections::BTreeSet::new();
    loop {
        let mut bytes=Vec::new();
        (&mut input).take(64*1024+1).read_until(b'\n',&mut bytes)?;
        if bytes.is_empty() {break;}
        total=total.checked_add(bytes.len() as u64).ok_or(Error::Limit)?;
        if bytes.len()>64*1024 || total>crate::MAX_SOURCE_BYTES {return Err(Error::Limit);}
        if bytes.iter().all(u8::is_ascii_whitespace) {continue;}
        let value:Value=serde_json::from_slice(&bytes).map_err(|_|Error::Content)?;
        let object=value.as_object().ok_or(Error::Content)?;
        // Only fields emitted by CronTaskRow and the reviewed JSONL aliases
        // may be ignored. Future trigger semantics require a fresh setup.
        let known_fields=object.keys().all(|key|matches!(key.as_str(),
            "id"|"name"|"agent_id"|"cron"|"schedule"|"task"|"description"|"action"|
            "enabled"|"created_at"|"updated_at"|"last_run_at"|"last_status"|"last_error"|
            "run_count"|"failure_count"|"notify_channel"|"notify_chat_id"|"notify_thread_id"|
            "cron_timezone"|"trigger_kind"|"condition_script"|"condition_state"|"watch_command"|
            "on_behalf_of"|"workflow_id"|"activation_id"|"workflow_binding"));
        if let Some(id)=string(&value,"id")? {
            if id.is_empty() || id.len()>128 || !seen.insert(id.to_string()) {return Err(Error::Content);}
        }
        let name=string(&value,"name")?.unwrap_or("unnamed");
        let owner=string(&value,"agent_id")?.unwrap_or("default");
        let schedule=string(&value,"cron")?.or(string(&value,"schedule")?).ok_or(Error::Content)?;
        let objective=string(&value,"task")?.or(string(&value,"description")?).or(string(&value,"action")?).ok_or(Error::Content)?;
        let timezone=string(&value,"cron_timezone")?.filter(|tz|!tz.is_empty());
        let trigger=string(&value,"trigger_kind")?;
        let pure_time=known_fields && matches!(trigger,None|Some("time"))
            && string(&value,"condition_script")?.is_none_or(str::is_empty)
            && string(&value,"watch_command")?.is_none_or(str::is_empty)
            && !["workflow_id","activation_id","workflow_binding"].iter().any(|key|value.get(key).is_some_and(|v|!v.is_null()));
        propose(builder,name,owner,schedule,objective,timezone,pure_time)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(kind:SourceKind)->ImportRequest {ImportRequest {version:1,migration_id:RequestId([4;16]),employee:None,source_id:"old:cron".into(),kind,title:"cron_tasks".into(),legacy_agent:None,has_wal:false,blob_handle:None}}
    fn value<'a>(proposal:&'a AuthorityProposal,name:&str)->&'a str {
        match &proposal.fields.iter().find(|f|f.name==name).unwrap().value {ProposalValue::Text{value}=>value,_=>panic!("text")}
    }
    fn db()->tempfile::NamedTempFile {
        let file=tempfile::NamedTempFile::new().unwrap();let conn=rusqlite::Connection::open(file.path()).unwrap();
        conn.execute_batch(include_str!("cron-v1793.sql")).unwrap();drop(conn);file
    }
    #[test]
    fn authority_cron_sqlite_emits_actionable_time_schedule_without_old_state() {
        let file=db();let conn=rusqlite::Connection::open(file.path()).unwrap();
        conn.execute_batch("INSERT INTO cron_tasks(id,name,agent_id,cron,task,created_at,updated_at,enabled,cron_timezone,trigger_kind,last_error,notify_chat_id,on_behalf_of,condition_state) VALUES ('old','Daily report','sales','0 9 * * *','Prepare report','now','now',0,'Asia/Taipei','time','SECRET-ERROR','SECRET-CHAT','SECRET-PRINCIPAL','SECRET-STATE');").unwrap();drop(conn);
        let p=extract(&request(SourceKind::AuthorityCronSqlite),std::fs::File::open(file.path()).unwrap(),None).unwrap();
        assert_eq!(p.len(),1);assert_eq!(p[0].category,C::Schedule);assert_eq!(p[0].fields.len(),5);
        assert_eq!(value(&p[0],"employee_id"),"sales");assert_eq!(value(&p[0],"schedule"),"0 9 * * *");
        assert_eq!(serde_json::from_str::<Value>(value(&p[0],"settings")).unwrap(),serde_json::json!({"kind":"time","timezone":"Asia/Taipei"}));
        assert!(!serde_json::to_string(&p).unwrap().contains("SECRET"));
        let conn=rusqlite::Connection::open(file.path()).unwrap();assert!(!conn.query_row("SELECT enabled FROM cron_tasks",[],|r|r.get::<_,bool>(0)).unwrap());
    }
    #[test]
    fn authority_cron_sqlite_never_downgrades_workflow_or_command_triggers() {
        let file=db();let conn=rusqlite::Connection::open(file.path()).unwrap();
        for (id,kind,script,watch) in [("condition","condition","SECRET-COMMAND",""),("watch","on_exit","","SECRET-WATCH"),("unknown","future","",""),("workflow","time","","")] {
            conn.execute("INSERT INTO cron_tasks(id,name,agent_id,cron,task,created_at,updated_at,trigger_kind,condition_script,watch_command) VALUES (?1,?1,'sales','0 * * * *','task','now','now',?2,?3,?4)",[id,kind,script,watch]).unwrap();
        }
        conn.execute_batch("INSERT INTO cron_workflow_bindings VALUES('workflow','old-grant','old-hash');").unwrap();drop(conn);
        let p=extract(&request(SourceKind::AuthorityCronSqlite),std::fs::File::open(file.path()).unwrap(),None).unwrap();
        assert_eq!(p.len(),4);assert!(p.iter().all(|p|value(p,"requires_reentry")=="true"));
        assert!(!serde_json::to_string(&p).unwrap().contains("SECRET"));
    }
    #[test]
    fn authority_cron_jsonl_legacy_aliases_match_actionable_adapter_shape() {
        let raw=br#"{"id":"old","name":"Daily report","agent_id":"sales","schedule":"0 9 * * *","description":"Prepare report","cron_timezone":"Asia/Taipei","notify_chat_id":"SECRET-CHAT","on_behalf_of":"SECRET-PRINCIPAL","last_error":"SECRET-ERROR"}
"#;
        let p=extract(&request(SourceKind::AuthorityCronJsonl),raw.as_slice(),None).unwrap();
        assert_eq!(p.len(),1);assert_eq!(p[0].fields.len(),5);assert_eq!(value(&p[0],"employee_id"),"sales");
        assert_eq!(value(&p[0],"objective"),"Prepare report");assert_eq!(value(&p[0],"schedule"),"0 9 * * *");
        assert!(!serde_json::to_string(&p).unwrap().contains("SECRET"));
    }
    #[test]
    fn authority_cron_jsonl_unknown_triggers_duplicate_ids_and_limits_fail_closed() {
        let raw=br#"{"id":"old","name":"Condition","agent_id":"sales","cron":"0 9 * * *","task":"work","trigger_kind":"condition","condition_script":"SECRET-SCRIPT"}"#;
        let p=extract(&request(SourceKind::AuthorityCronJsonl),raw.as_slice(),None).unwrap();
        assert_eq!(value(&p[0],"requires_reentry"),"true");assert!(!serde_json::to_string(&p).unwrap().contains("SECRET"));
        let duplicate=[raw.as_slice(),b"\n",raw.as_slice()].concat();
        assert!(extract(&request(SourceKind::AuthorityCronJsonl),duplicate.as_slice(),None).is_err());
        assert!(extract(&request(SourceKind::AuthorityCronJsonl),vec![b' ';65537].as_slice(),None).is_err());
    }
    #[test]
    fn authority_cron_jsonl_unknown_semantics_require_reentry() {
        let raw=br#"{"name":"Future","cron":"0 9 * * *","task":"work","new_trigger":"SECRET-CONDITION"}"#;
        let p=extract(&request(SourceKind::AuthorityCronJsonl),raw.as_slice(),None).unwrap();
        assert_eq!(value(&p[0],"requires_reentry"),"true");
        assert!(!serde_json::to_string(&p).unwrap().contains("SECRET"));
    }
    #[test]
    fn authority_cron_sqlite_rejects_unknown_catalog_and_user_version() {
        for mutation in ["CREATE TABLE extra(authority TEXT)","PRAGMA user_version=1"] {
            let file=db();let conn=rusqlite::Connection::open(file.path()).unwrap();conn.execute_batch(mutation).unwrap();drop(conn);
            assert!(extract(&request(SourceKind::AuthorityCronSqlite),std::fs::File::open(file.path()).unwrap(),None).is_err());
        }
    }
}
