//! Live host turns only. Signed MCP refusals may request a hint for their own
//! turn, but never supply its member, original task, destination or scopes.
use std::{collections::HashMap,path::{Path,PathBuf},sync::{Arc,Mutex,OnceLock}};
use duduclaw_knowledge::requester::RequesterIdentity;
#[derive(Default)]
pub(crate) struct Notice { pub(crate) text: Option<String>, queued: Option<Queued> }
type NoticeCell=Arc<Mutex<Notice>>;
tokio::task_local! { pub(crate) static NOTICE: NoticeCell; }
struct Queued { home:PathBuf,agent:String,text:String,session:String,requester:RequesterIdentity,
    source:Option<serde_json::Value>,sessions:Arc<crate::session::SessionManager>,generation:Option<String>,tool:String }
#[derive(Clone)]
struct Turn { home:PathBuf, agent:String, text:String, session:String, requester:RequesterIdentity,
    source:Option<serde_json::Value>, notice:NoticeCell, sessions:Arc<crate::session::SessionManager>,generation:Option<String> }
type Key=(PathBuf,String,String);
fn turns()-> &'static Mutex<HashMap<Key,Turn>> {
    static TURNS:OnceLock<Mutex<HashMap<Key,Turn>>>=OnceLock::new(); TURNS.get_or_init(Default::default)
}
pub(crate) struct Guard(Key);
impl Drop for Guard { fn drop(&mut self) { if let Ok(mut map)=turns().lock() { map.remove(&self.0); } } }
pub(crate) fn register(home:&Path,agent:&str,id:&str,text:&str,session:&str,sessions:Arc<crate::session::SessionManager>,generation:Option<String>) -> Option<Guard> {
    let notice=NOTICE.try_with(Clone::clone).ok()?;
    let key=(home.to_path_buf(),agent.to_owned(),id.to_owned());
    let turn=Turn {home:home.into(),agent:agent.into(),text:text.into(),session:session.into(),
        requester:crate::knowledge::turn_requesters::current_requester(),
        source:crate::approval::CURRENT_TRUSTED_REPLY_TARGET.try_with(Clone::clone).ok().flatten().map(|t|t.notice_descriptor()),notice,sessions,generation};
    let mut map=turns().lock().ok()?;
    if map.contains_key(&key) || map.len()>=1024 { return None; }
    map.insert(key.clone(),turn);Some(Guard(key))
}
pub(crate) async fn missing(home:&Path,agent:&str,turn_id:Option<&str>,tool:&str,reason:&str) {
    if !matches!(reason,"not_connected"|"scope_missing"|"awaiting_consent") { return; }
    let Some(id)=turn_id else {return};
    let turn=turns().lock().ok().and_then(|m|m.get(&(home.into(),agent.into(),id.into())).cloned());
    let Some(turn)=turn else {return};
    if turn.notice.lock().map_or(true,|n|n.text.is_some()) {return;}
    let Ok(generation)=turn.sessions.turn_generation(&turn.session).await else {return};
    if generation!=turn.generation {return;}
    let Ok(mut notice)=turn.notice.lock() else {return};
    if notice.text.is_some() {return;}
    let Ok(Some(db))=crate::decision_notify::open_user_db(home) else {return};
    notice.text=super::issue_for_turn(&turn.home,&db,&turn.agent,tool,&turn.text,&turn.session,&turn.requester,turn.source,generation).ok().flatten();
}

/// Native gateway callers do not all carry a transport turn id. They may
/// only attach to one unambiguous, host-registered live request. Concurrent
/// turns for the same sender fail closed rather than choosing a conversation.
pub(crate) async fn probe_for_requester(home:&Path,agent:&str,requester:&RequesterIdentity,tool:&str) -> bool {
    let candidates=turns().lock().map(|map|map.iter().filter(|((h,a,_),turn)|
        h==home && a==agent && &turn.requester==requester).map(|((_,_,id),turn)|(id.clone(),turn.notice.clone())).collect::<Vec<_>>()).unwrap_or_default();
    let [(id,notice)]=candidates.as_slice() else {return false;};
    missing(home,agent,Some(id),tool,"not_connected").await;
    notice.lock().is_ok_and(|n|n.text.is_some())
}

/// Metadata gates may run on a blocking thread. Queue only trusted ingress
/// facts here; the outer gateway turn issues a ticket after leaving all
/// isolated effect scopes. No ticket/database/network work enters the gate.
pub(crate) fn queue_missing(home:&Path,agent:&str,requester:&RequesterIdentity,tool:&str,reason:&str) -> bool {
    if !matches!(reason,"not_connected"|"scope_missing"|"awaiting_consent") || super::purpose(tool).is_none() {return false;}
    let candidates=turns().lock().map(|map|map.values().filter(|turn|
        turn.home==home && turn.agent==agent && &turn.requester==requester).cloned().collect::<Vec<_>>()).unwrap_or_default();
    let [turn]=candidates.as_slice() else {return false;};
    let Ok(mut notice)=turn.notice.lock() else {return false;};
    if notice.text.is_none() && notice.queued.is_none() {
        notice.queued=Some(Queued {home:turn.home.clone(),agent:turn.agent.clone(),text:turn.text.clone(),
            session:turn.session.clone(),requester:turn.requester.clone(),source:turn.source.clone(),
            sessions:turn.sessions.clone(),generation:turn.generation.clone(),tool:tool.into()});
    }
    true
}
pub(crate) async fn flush(notice:&NoticeCell) {
    let queued=notice.lock().ok().and_then(|mut n|n.queued.take());
    let Some(turn)=queued else {return;};
    let Ok(generation)=turn.sessions.turn_generation(&turn.session).await else {return;};
    let Ok(Some(db))=crate::decision_notify::open_user_db(&turn.home) else {return;};
    let Ok(mut notice)=notice.lock() else {return;};
    if notice.text.is_none() {
        notice.text=super::issue_for_turn(&turn.home,&db,&turn.agent,&turn.tool,&turn.text,
            &turn.session,&turn.requester,turn.source,generation).ok().flatten();
    }
}

/// Called with the generation returned by ensure_turn_session's transaction.
pub(crate) fn bind_generation(home:&Path,agent:&str,id:&str,generation:Option<String>) {
    if let Ok(mut map)=turns().lock() {
        let key=(home.to_path_buf(),agent.to_owned(),id.to_owned());
        if let Some(turn)=map.get_mut(&key) {
            if generation.as_ref().is_some_and(|value| !value.is_empty()) { turn.generation=generation; }
            else { map.remove(&key); }
        }
    }
}

/// Only host-registered ingress facts may create a write-consent request.
/// A signed tool caller supplies neither the task text nor the destination.
pub(crate) fn drive_write_notice(home:&Path,agent:&str,turn_id:Option<&str>,
    requester:&RequesterIdentity,user:&str,tool:&str) -> Option<String> {
    if !duduclaw_core::google_identity::is_drive_write_tool(tool) { return None; }
    let turn=turns().lock().ok()?.get(&(home.into(),agent.into(),turn_id?.into()))?.clone();
    if &turn.requester!=requester { return None; }
    let member=match &turn.requester {
        RequesterIdentity::DashboardUser {user_id} => user_id,
        RequesterIdentity::ChannelUser {user_id:Some(id),..} => id,
        _ => return None,
    };
    if member!=user { return None; }
    let db=crate::decision_notify::open_user_db(home).ok()??;
    let notice=super::issue_for_turn(home,&db,agent,tool,&turn.text,&turn.session,
        &turn.requester,turn.source,turn.generation).ok()??;
    if notice==super::DASHBOARD_HINT { return None; }
    let link=super::render::ticket_url(&notice)?;
    Some(format!("請本人開啟這個連結，連結雲端硬碟寫入：{link}"))
}
