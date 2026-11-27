//! RM-147: gateway-owned consent requests, never a login or send approval.
pub(crate) mod store;
pub(crate) mod routes;
pub(crate) mod resume;
pub(crate) mod url;
pub(crate) mod render;
pub(crate) mod turn;
#[cfg(test)] mod tests;

use std::path::Path;
use duduclaw_auth::{AccessLevel, UserDb};
use duduclaw_core::google_identity::{self as gi, Effective};
use duduclaw_knowledge::requester::RequesterIdentity;
use store::{Request, Store, REFUSED};

pub(crate) const DASHBOARD_HINT: &str = "請登入 Dashboard，開啟「我的連線」確認 Google 授權，再回來繼續。";

/// Closed host mapping: a model cannot choose scopes, subjects or URLs.
fn purpose(tool: &str) -> Option<&'static str> {
    if !crate::google_personal::scopes::token_tool(tool) { return None; }
    Some(match tool {
        "drive_search" | "drive_read" => "drive_tools",
        "google_chat_spaces" | "google_chat_messages" => "chat_read",
        "google_remote_mcp" => "workspace",
        _ => "base",
    })
}

fn config_version(home: &Path, agent: &str) -> Result<String,String> {
    use sha2::{Digest,Sha256};
    if !duduclaw_core::is_valid_agent_id(agent) { return Err(REFUSED.into()); }
    let mut digest=Sha256::new();
    for path in [home.join("config.toml"), home.join("agents").join(agent).join("agent.toml")] {
        let bytes=std::fs::read(path).map_err(|_| REFUSED)?;
        digest.update((bytes.len() as u64).to_be_bytes()); digest.update(bytes);
    }
    if let Some(client)=crate::mcp_oauth::get_client_config(home,"google") {
        for value in [&client.client_id,&client.client_secret] {
            digest.update((value.len() as u64).to_be_bytes()); digest.update(value.as_bytes());
        }
    }
    match std::fs::read(crate::google_identity::delegation_key::path(home)) {
        Ok(bytes)=>{ digest.update((bytes.len() as u64).to_be_bytes()); digest.update(bytes); }
        Err(e) if e.kind()==std::io::ErrorKind::NotFound=>digest.update(0u64.to_be_bytes()),
        Err(_)=>return Err(REFUSED.into()),
    }
    Ok(hex::encode(digest.finalize()))
}

fn mapped_member(home: &Path, db: &UserDb, channel: &str, sender: &str) -> Result<String,String> {
    // The first release requires an explicit verified binding even when
    // Google Chat's optional sub-derived identity lookup is enabled.
    let id=crate::googlechat_identity::verified_user_id(home,db,channel,sender)?.ok_or(REFUSED)?;
    let sender=if channel=="googlechat" {
        crate::googlechat_identity::normalize_user_id(sender).ok_or(REFUSED)?
    } else { sender.to_owned() };
    if db.find_verified_user_id_by_channel(channel,&sender)?.as_deref()!=Some(&id) { return Err(REFUSED.into()); }
    crate::google_personal::flow::active_user(db,&id)?;
    Ok(id)
}

fn expected_account(home: &Path, db: &UserDb, user: &str, agent: &str) -> Result<(String,Option<String>),String> {
    if let Some(record)=crate::person_credentials::get(home,user)? {
        if record.metadata.google_sub.is_empty() { return Err(REFUSED.into()); }
        return Ok((record.metadata.account_email,Some(record.metadata.google_sub)));
    }
    // A self-edited login email is not evidence of Google ownership.
    let attestation=db.google_delegate_attestation(agent)?.ok_or(REFUSED)?;
    let email=attestation.attestation.subject;
    require_attested(home,db,user,agent,&email)?;
    Ok((email,None))
}
fn require_attested(home:&Path, db:&UserDb, user:&str, agent:&str, email:&str) -> Result<(),String> {
    let accounts=crate::google_identity::tenant_source::subject_accounts(db,agent,
        crate::google_identity::delegate_owner::incarnation(home,agent),email).map_err(|_|REFUSED)?;
    if !matches!(accounts.as_slice(), [a] if a.active && a.attested && a.user_id==user) { return Err(REFUSED.into()); }
    Ok(())
}

pub(crate) fn validate(home: &Path, db: &UserDb, row: &Request) -> Result<(),String> {
    if row.home!=home.canonicalize().map_err(|_|REFUSED)? || row.expires<=chrono::Utc::now().timestamp()
        || db.acl_generation()?!=row.acl_version || config_version(home,&row.agent)?!=row.config_version {
        return Err(REFUSED.into());
    }
    crate::google_personal::flow::active_user(db,&row.user)?;
    if db.check_agent_access(&row.user,&row.agent)?!=Some(AccessLevel::Owner) { return Err(REFUSED.into()); }
    if row.channel!="webchat" && mapped_member(home,db,&row.channel,&row.sender)?!=row.user { return Err(REFUSED.into()); }
    if !crate::google_workspace::integration_enabled(home) { return Err(REFUSED.into()); }
    match crate::google_identity::delegate_owner::effective_for(home,Some(&row.agent)).effective {
        Effective::Personal { selector } if row.mode=="oauth" => {
            let user=crate::google_personal::access::select_user(db,&row.agent,&selector).map_err(|_|REFUSED)?;
            if user.id!=row.user { return Err(REFUSED.into()); }
            if row.google_sub.is_none() { require_attested(home,db,&row.user,&row.agent,&row.email)?; }
            if let Some(record)=crate::person_credentials::get(home,&row.user)? {
                if !account_matches(row,&record.metadata.google_sub,&record.metadata.account_email) { return Err(REFUSED.into()); }
            } else if row.google_sub.is_some() { return Err(REFUSED.into()); }
        }
        Effective::Delegate { email } if row.mode=="delegate" && email==row.email => {
            require_attested(home,db,&row.user,&row.agent,&email)?;
            let key=crate::google_identity::delegation_key::info(home)?.ok_or(REFUSED)?;
            if key.client_id.as_deref().is_none_or(str::is_empty) { return Err(REFUSED.into()); }
            let people=crate::google_identity::requester_gate::people_for(home,None);
            crate::google_identity::resolve::precheck(home,&email,people.as_ref()).map_err(|_|REFUSED)?;
            crate::google_identity::delegated_scopes::for_tool(home,&row.tool).map_err(|_|REFUSED)?;
        }
        _ => return Err(REFUSED.into()),
    }
    if row.purpose=="chat_read" && !crate::google_workspace::chat_read_enabled(home) { return Err(REFUSED.into()); }
    Ok(())
}
pub(crate) fn account_matches(row:&Request, sub:&str, email:&str) -> bool {
    !sub.is_empty() && gi::normalize_email(email).ok()==gi::normalize_email(&row.email).ok()
        && gi::normalize_email(email).is_ok()
        && row.google_sub.as_ref().is_none_or(|expected| expected==sub)
}

/// Metadata-only capability probe. Never mints or refreshes a token, and never
/// falls back to another person's account when the member lacks a purpose.
fn needs_connection(home:&Path,db:&UserDb,row:&Request) -> Result<bool,String> {
    if db.google_consent(&row.user,&row.agent)?==Some(false) { return Err(REFUSED.into()); }
    if row.mode=="delegate" { return Ok(db.google_consent(&row.user,&row.agent)?!=Some(true)); }
    let record=crate::person_credentials::get(home,&row.user)?;
    Ok(record.is_none_or(|r| !crate::google_personal::scopes::permitted_for(&row.tool,&r.metadata))
        || db.google_consent(&row.user,&row.agent)?!=Some(true))
}

pub(crate) fn issue_for_turn(home:&Path,db:&UserDb,agent:&str,tool:&str,text:&str,session_id:&str,
    requester:&RequesterIdentity,source:Option<serde_json::Value>,session_generation:Option<String>) -> Result<Option<String>,String> {
    let purpose=purpose(tool).ok_or(REFUSED)?;
    let (channel,sender,conversation,user)=match requester {
        RequesterIdentity::DashboardUser {user_id} => ("webchat".to_owned(),user_id.clone(),session_id.to_owned(),user_id.clone()),
        RequesterIdentity::ChannelUser {channel,chat,sender,user_id:Some(id),..} => {
            if mapped_member(home,db,channel,sender)?!=*id { return Err(REFUSED.into()); }
            (channel.clone(),sender.clone(),chat.clone(),id.clone())
        }
        _ => return Err(REFUSED.into()),
    };
    if text.is_empty() || text.len()>32768 { return Err(REFUSED.into()); }
    let (mode,email,sub)=match crate::google_identity::delegate_owner::effective_for(home,Some(agent)).effective {
        Effective::Personal{selector} => {
            if crate::google_personal::access::select_user(db,agent,&selector).map_err(|_|REFUSED)?.id!=user { return Err(REFUSED.into()); }
            crate::google_personal::flow::active_user(db,&user)?;
            if db.check_agent_access(&user,agent)?!=Some(AccessLevel::Owner) { return Err(REFUSED.into()); }
            let (email,sub)=match expected_account(home,db,&user,agent) {
                Ok(account)=>account,
                Err(_)=>return Ok(Some(DASHBOARD_HINT.into())),
            };
            ("oauth",email,sub)
        }
        Effective::Delegate{email} => ("delegate",email,None),
        _ => return Ok(None),
    };
    let row=Request { id:uuid::Uuid::new_v4().to_string(),home:home.canonicalize().map_err(|_|REFUSED)?,
        user,agent:agent.into(),tool:tool.into(),purpose:purpose.into(),mode:mode.into(),email,google_sub:sub,
        channel:channel.clone(),sender,conversation,session_id:session_id.into(),session_generation,
        source,
        text:text.into(),acl_version:db.acl_generation()?,config_version:config_version(home,agent)?,expires:chrono::Utc::now().timestamp()+store::TTL };
    validate(home,db,&row)?;
    if !needs_connection(home,db,&row)? { return Ok(None); }
    let Some(base)=url::landing_base(home,&channel)? else { return Ok(Some(DASHBOARD_HINT.into())); };
    let ticket=Store::open(home)?.issue(row)?;
    // Fragment is never sent in HTTP URLs, referrers, access logs or preview requests.
    Ok(Some(render::notice(&format!("{base}#ticket={ticket}"))))
}

/// Host capability hints also work when tools/list hides missing capabilities.
/// Conservative exact-word matching only selects a service to inspect; it
/// cannot grant authority or widen the closed purpose table.
pub(crate) fn hinted_tool(text:&str) -> Option<&'static str> {
    [("drive_search", &["Google Drive","雲端硬碟","ドライブ"][..]),
     ("google_chat_messages", &["Google Chat"][..]),
     ("gmail_search", &["Gmail"][..]),
     ("calendar_list_events", &["Google Calendar","Google 日曆","Google カレンダー"][..]),
     ("docs_read", &["Google Docs"][..]),("sheets_read", &["Google Sheets"][..])]
        .into_iter().find(|(_,words)|words.iter().any(|word|duduclaw_core::word_contains_ci(text,word))).map(|(tool,_)|tool)
}
