//! Dashboard JWT handlers call these functions after authentication. Tickets
//! only locate a request; they cannot authenticate a browser or select a user.
use std::path::Path;
use duduclaw_auth::UserDb;
use serde_json::{json,Value};
use super::{store::{Store,Request,REFUSED},validate};

pub(crate) fn preview(home:&Path,db:&UserDb,user:&str,ticket:&str) -> Result<Value,String> {
    let row=Store::open(home)?.preview(ticket,user)?;
    validate(home,db,&row)?;
    let mut services=if row.purpose=="drive_write" {
        vec![crate::google_personal::oauth::DRIVE_WRITE_SCOPE.to_string(),
            "https://www.googleapis.com/auth/documents".into(),
            "https://www.googleapis.com/auth/spreadsheets".into(), "openid".into(), "email".into()]
    } else if row.mode=="delegate" {
        crate::google_identity::delegated_scopes::for_tool(home,&row.tool).map_err(|_|REFUSED)?.iter().map(|s|s.to_string()).collect::<Vec<_>>()
    } else { crate::google_personal::oauth::requested_scopes(false,row.purpose=="drive_tools").into_iter().map(str::to_string).collect() };
    if row.mode=="oauth" && row.purpose=="workspace" {
        services.extend(crate::google_personal::oauth::requested_scopes(false,true).into_iter().map(str::to_string));
        services.sort(); services.dedup();
    }
    if row.mode=="oauth" && !matches!(row.purpose.as_str(),"drive_tools"|"workspace"|"drive_write") && crate::google_workspace::chat_read_enabled(home) {
        services.extend(crate::google_workspace::CHAT_READ_SCOPES.iter().map(|s|s.to_string()));
    }
    Ok(json!({"agent":row.agent,"account":row.email,"purpose":row.purpose,"mode":row.mode,
        "services":services,"expires":row.expires}))
}

pub(crate) async fn confirm(home:&Path,db:&UserDb,user:&str,session:&str,ticket:&str,origin:&str,
    acknowledged:bool,shared_access:bool,acknowledge_write:bool) -> Result<Value,String> {
    if !acknowledged || !shared_access || uuid::Uuid::parse_str(session).is_err() { return Err(REFUSED.into()); }
    let store=Store::open(home)?;
    let row=store.preview(ticket,user)?;
    if row.purpose=="drive_write" && !acknowledge_write { return Err(REFUSED.into()); }
    validate(home,db,&row)?;
    // Validate the final callback before consuming the request. No paste-back
    // fallback is permitted for a chat authorization request.
    if row.mode=="oauth" {
        let allowed=crate::server::allowed_origins_snapshot();
        let (plan,_)=crate::remote_mcp::connect::dashboard_redirect_plan(home,origin,&allowed,crate::google_personal::flow::CALLBACK_PATH)?;
        if matches!(plan,crate::remote_mcp::connect::RedirectPlan::Paste{..}) { return Err(super::DASHBOARD_HINT.into()); }
        if row.channel!="webchat" && !super::url::external_base(&::url::Url::parse(plan.redirect_uri()).map_err(|_|REFUSED)?) { return Err(REFUSED.into()); }
    }
    let row=store.claim_ticket(ticket,user,session)?;
    let id=row.id.clone();
    let result=confirm_claimed(home,db,user,session,origin,row,acknowledge_write).await;
    if result.is_err() { let _=store.cas(&id,"confirming","stopped"); }
    result
}

async fn confirm_claimed(home:&Path,db:&UserDb,user:&str,session:&str,origin:&str,row:Request,acknowledge_write:bool) -> Result<Value,String> {
    if row.mode=="delegate" {
        // Prove the administrator's DWD grant for this service without reading
        // Workspace data. A member confirmation cannot create that grant.
        let scopes=crate::google_identity::delegated_scopes::for_tool(home,&row.tool).map_err(|_|REFUSED)?;
        crate::google_identity::resolve::mint_delegate(home,&row.email,scopes,
            crate::google_service_account::CacheMode::Bypass).await.map_err(|_|REFUSED)?;
        validate(home,db,&row)?;
        complete(home,db,row,None)?;
        return Ok(json!({"completed":true}));
    }
    // Consent-only requests do not need another provider round trip.
    if row.purpose!="drive_write" {
        if let Some(record)=crate::person_credentials::get(home,user)? {
            if record.metadata.status==crate::person_credentials::Status::Connected
                && crate::google_personal::scopes::permitted_for(&row.tool,&record.metadata)
                && crate::google_data_consent::require_current(record.metadata.google_data_consent.as_ref()).is_ok() {
                complete(home,db,row,Some(&record.metadata))?;
                return Ok(json!({"completed":true}));
            }
        }
    }
    let allowed=crate::server::allowed_origins_snapshot();
    let result=crate::google_personal::flow::start_authorization_request(home,db,user,session,origin,&allowed,&row,acknowledge_write).await?;
    let state=result.get("state").and_then(Value::as_str).ok_or(REFUSED)?;
    Store::open(home)?.bind_oauth(&row.id,state)?;
    Ok(result)
}

/// Executed after account/scope checks and credential publication. The
/// outbox carries the exact credential identity, never a token or send grant.
pub(crate) fn complete(home:&Path,db:&UserDb,mut row:Request,metadata:Option<&crate::person_credentials::Metadata>) -> Result<(),String> {
    validate(home,db,&row)?;
    let receipt=if row.purpose=="drive_write" {
        let metadata=super::write_metadata(home,&row)?;
        json!({"mode":"oauth","purpose":"drive_write","credential_id":metadata.credential_id,
            "email":metadata.account_email,"sub":metadata.google_sub})
    } else if row.mode=="delegate" {
        json!({"mode":"delegate","email":row.email})
    } else {
        let metadata=metadata.ok_or(REFUSED)?;
        if !super::account_matches(&row,&metadata.google_sub,&metadata.account_email)
            || metadata.status!=crate::person_credentials::Status::Connected
            || !crate::google_personal::scopes::permitted_for(&row.tool,metadata) { return Err(REFUSED.into()); }
        json!({"mode":"oauth","credential_id":metadata.credential_id,"email":metadata.account_email,"sub":metadata.google_sub})
    };
    // The existing API and SQL constraint both retain decided_by == user_id.
    // This write bumps ACL generation exactly once. Any intervening unrelated
    // change refuses continuation instead of laundering a newer snapshot.
    let generation=row.acl_version.checked_add(1).ok_or(REFUSED)?;
    db.set_google_consent_versioned(&row.user,&row.agent,true,&row.user,Some(row.acl_version))?;
    row.acl_version=generation;
    validate(home,db,&row)?;
    Store::open(home)?.ready(&row,&receipt)
}

pub(crate) fn private_response(mut response:axum::response::Response) -> axum::response::Response {
    let headers=response.headers_mut();
    headers.insert("cache-control",axum::http::HeaderValue::from_static("no-store"));
    headers.insert("referrer-policy",axum::http::HeaderValue::from_static("no-referrer"));
    response
}

/// Same origin policy as the existing OAuth flow (including local WebChat).
/// The shared validator uses origin_host_matches for external authorities.
pub(crate) fn valid_origin(origin:&str,allowed:&[String]) -> bool {
    !origin.is_empty() && crate::remote_mcp::connect::validate_redirect_origin(origin,allowed).is_ok()
}
