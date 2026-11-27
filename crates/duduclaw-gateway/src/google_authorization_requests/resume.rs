//! Durable outbox with at-most-once admission. A crash after claim is an
//! uncertain outcome, never permission to repeat a task or an outbound send.
use std::{path::Path,sync::Arc};
use serde_json::Value;
use super::{store::{Store,Request,REFUSED},validate};

fn ready(home:&Path,db:&duduclaw_auth::UserDb,row:&Request,receipt:&Value) -> Result<(),String> {
    validate(home,db,row)?;
    if db.google_consent(&row.user,&row.agent)?!=Some(true) { return Err(REFUSED.into()); }
    if row.purpose=="drive_write" {
        let metadata=super::write_metadata(home,row)?;
        if receipt["purpose"]!="drive_write"
            || receipt["credential_id"].as_str()!=Some(metadata.credential_id.as_str())
            || receipt["sub"].as_str()!=Some(metadata.google_sub.as_str())
            || receipt["email"].as_str()!=Some(metadata.account_email.as_str()) { return Err(REFUSED.into()); }
    } else if row.mode=="oauth" {
        let record=crate::person_credentials::get(home,&row.user)?.ok_or(REFUSED)?;
        if receipt["credential_id"].as_str()!=Some(record.metadata.credential_id.as_str())
            || receipt["sub"].as_str()!=Some(record.metadata.google_sub.as_str())
            || receipt["email"].as_str()!=Some(record.metadata.account_email.as_str())
            || record.metadata.status!=crate::person_credentials::Status::Connected
            || !crate::google_personal::scopes::permitted_for(&row.tool,&record.metadata)
            || crate::google_data_consent::require_current(record.metadata.google_data_consent.as_ref()).is_err() { return Err(REFUSED.into()); }
    } else if receipt["mode"]!="delegate" || receipt["email"].as_str()!=Some(row.email.as_str()) { return Err(REFUSED.into()); }
    Ok(())
}

pub(crate) fn start(ctx:Arc<crate::channel_reply::ReplyContext>) {
    tokio::spawn(async move {
        let mut interval=tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            interval.tick().await;
            if !duduclaw_core::gateway_instance::held(&ctx.home_dir) { continue; }
            // No SQLite connection/lock is retained across model awaits.
            let rows=Store::open(&ctx.home_dir).and_then(|s|s.ready_rows());
            let Ok(rows)=rows else { continue; };
            for (row,receipt) in rows {
                if run(&ctx,&row,&receipt).await.is_err() {
                    if let Ok(store)=Store::open(&ctx.home_dir) {
                        let _=store.cas(&row.id,"ready","stopped");
                    }
                    // Never log the original text, ticket, state or account.
                    tracing::warn!("Google authorization continuation stopped; see My Connections");
                }
            }
        }
    });
}

async fn run(ctx:&Arc<crate::channel_reply::ReplyContext>,row:&Request,receipt:&Value) -> Result<(),String> {
    // Pre-claim checks must remain local: no secret manager or provider calls.
    if let Some(source)=&row.source {
        let target=crate::approval::TrustedReplyTarget::validate_notice(source)?;
        if target.channel!=row.channel || target.principal_id!=row.sender
            || target.conversation_id!=row.conversation { return Err(REFUSED.into()); }
    }
    if row.conversation.is_empty() || row.sender.is_empty() { return Err(REFUSED.into()); }
    if ctx.session_manager.turn_generation(&row.session_id).await.map_err(|_|REFUSED)? != row.session_generation
        || ctx.session_manager.archived_at(&row.session_id).await.map_err(|_|REFUSED)?.is_some() { return Err(REFUSED.into()); }
    claim_and_run(&ctx.home_dir,&row.id,|| async {
        let db=crate::decision_notify::open_user_db(&ctx.home_dir)?.ok_or(REFUSED)?;
        ready(&ctx.home_dir,&db,row,receipt)?;
        let target=match &row.source {
            Some(source)=>Some(crate::approval::TrustedReplyTarget::restore_notice(&ctx.home_dir,source).await?),
            None=>None,
        };
        let sender=if row.channel=="webchat" {
            crate::channel_sender::create_webchat_sender(row.session_id.clone(),ctx.event_tx.clone())
        } else if let Some(target)=&target { target.sender(ctx.http.clone()) }
        else {
            // Credential resolution (including Feishu's token request) is claimed work.
            let target=crate::channel_sender::resolve_channel_target(&ctx.home_dir,&row.channel,&row.conversation).await?;
            crate::channel_sender::create_sender_for_home(&ctx.home_dir,&target,ctx.http.clone())
        };
        ready(&ctx.home_dir,&db,row,receipt)?;
        if ctx.session_manager.turn_generation(&row.session_id).await.map_err(|_|REFUSED)? != row.session_generation
            || ctx.session_manager.archived_at(&row.session_id).await.map_err(|_|REFUSED)?.is_some() { return Err(REFUSED.into()); }
        let work=async {
            // Re-enter the ordinary pipeline, including requester mapping,
            // channel access, tool gates and independent outbound approvals.
            let reply=crate::channel_reply::build_guarded_reply_for_agent(
                &row.text,ctx,&row.agent,&row.session_id,&row.sender,None).await;
            ready(&ctx.home_dir,&db,row,receipt)?;
            if !reply.still_valid().await { return Err(REFUSED.into()); }
            sender.send_text(&reply.text).await.map_err(|_|REFUSED.to_owned())?;
            Ok(())
        };
        if row.channel=="webchat" {
            crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(row.user.clone(),Box::pin(work)).await
        } else if let Some(target)=target {
            crate::approval::scope_trusted_reply(Ok(target),Box::pin(work)).await
        } else { Box::pin(work).await }
    }).await
}

/// The work future is only constructed after the single-use CAS succeeds.
/// Every fallible operation after claim, including sender restoration, settles here.
pub(super) async fn claim_and_run<F, Fut>(home:&Path,id:&str,work:F) -> Result<(),String>
where F:FnOnce()->Fut, Fut:std::future::Future<Output=Result<(),String>> {
    Store::open(home)?.cas(id,"ready","claimed")?;
    let outcome=work().await.and_then(|()| Store::open(home)?.cas(id,"claimed","done"));
    if outcome.is_err() {
        // Failure settlement must also work if the ticket expired during I/O.
        Store::open(home)?.stop_claimed(id)?;
    }
    outcome
}
