//! Closed signed loopback protocol; no dashboard or public upload surface.
use super::*;
use axum::{body::Bytes, extract::{ConnectInfo,DefaultBodyLimit,State}, http::{HeaderMap,StatusCode}, response::{IntoResponse,Response}, routing::post, Json,Router};
use std::{net::SocketAddr,time::Instant};
use crate::computer_use_sessions::{auth,gates};
use serde::Deserialize;
use serde_json::{Value,json};
pub const ROUTE:&str="/api/internal/meeting-jobs";
struct RouteState {manager:Arc<Manager>, replay:auth::ReplayGuard, rate:auth::RateLimiter}
#[derive(Deserialize)]
#[serde(tag="op",rename_all="snake_case",deny_unknown_fields)]
enum Request {
    UploadLink {protocol:String,turn_id:Option<String>},
    Create {protocol:String,source_path:String,engine:Option<String>,language_hint:String,turn_id:Option<String>},
    Status {protocol:String,job_id:String},
    Cancel {protocol:String,job_id:String},
}
pub fn router(manager:Arc<Manager>)->Router {
    Router::new().route(ROUTE,post(handle)).layer(DefaultBodyLimit::max(16*1024))
        .with_state(Arc::new(RouteState{manager,replay:Default::default(),rate:Default::default()}))
}
fn fail(status:StatusCode,message:&str)->Response {(status,Json(json!({"message":message}))).into_response()}
async fn handle(State(st):State<Arc<RouteState>>,ConnectInfo(peer):ConnectInfo<SocketAddr>,headers:HeaderMap,body:Bytes)->Response {
    let m=&st.manager;
    if !duduclaw_core::gateway_instance::held(&m.home){return fail(StatusCode::SERVICE_UNAVAILABLE,"會議服務暫時無法使用。");}
    let caller=match auth::authenticate(&m.home,peer,&headers,&body,chrono::Utc::now().timestamp().max(0) as u64){Ok(c)=>c,Err(_)=>return fail(StatusCode::UNAUTHORIZED,"無法驗證會議請求。")};
    if !st.rate.allow(&caller.agent_id,Instant::now()) || !st.replay.record(&caller.nonce,Instant::now()){return fail(StatusCode::TOO_MANY_REQUESTS,"請稍後再試。");}
    let request:Request=match serde_json::from_slice(&body){Ok(r)=>r,Err(_)=>return fail(StatusCode::BAD_REQUEST,"會議請求格式錯誤。")};
    let protocol=match &request {Request::UploadLink{protocol,..}|Request::Create{protocol,..}|Request::Status{protocol,..}|Request::Cancel{protocol,..}=>protocol};
    if protocol!="meeting_jobs_v1" {return fail(StatusCode::BAD_REQUEST,"會議請求格式錯誤。");}
    let tool=match request {Request::UploadLink{..}=>"meeting_upload_link",Request::Create{..}=>"meeting_transcribe",Request::Status{..}=>"meeting_transcribe_status",Request::Cancel{..}=>"meeting_transcribe_cancel"};
    let agent=&caller.agent_id;
    if let Err(e)=m.employee(agent){return fail(StatusCode::FORBIDDEN,&e);}
    let policy=match m.policy_digest(agent){Ok(p)=>p,Err(e)=>return fail(StatusCode::FORBIDDEN,&e)};
    if let Err(e)=gates::tool_gates(&m.home,agent,tool).await{return fail(StatusCode::FORBIDDEN,&e.message);}
    // Resolve an owned record before approval or any distinguishable mutation.
    if let Request::Status{job_id,..}|Request::Cancel{job_id,..}=&request {
        if m.store.get(agent,job_id).is_err(){return fail(StatusCode::NOT_FOUND,store::NOT_FOUND);}
    }
    // Snapshot and reserve before any review: approvals identify bytes, never a mutable path.
    let admission=if let Request::Create{source_path,engine,language_hint,turn_id,..}=&request {
        match m.admit_job(agent,source_path,engine.as_deref(),language_hint,turn_id.as_deref()) {
            Ok(admission)=>Some(admission),Err(e)=>return fail(StatusCode::BAD_REQUEST,&e),
        }
    }else{None};
    let call_id=admission.as_ref().map(|a|format!("{}:{}:{}",caller.nonce,a.job.source_sha256,a.job.source_bytes)).unwrap_or_else(||caller.nonce.clone());
    let mut args:Value=serde_json::from_slice(&body).unwrap_or(Value::Null);
    if let Some(o)=args.as_object_mut(){o.remove("op");o.remove("protocol");o.remove("turn_id");}
    if let Some(admission)=&admission {
        args["source_sha256"]=admission.job.source_sha256.clone().into();
        args["source_bytes"]=admission.job.source_bytes.into();
    }
    let policies=duduclaw_core::agent_toml::load(&m.agent_dir(agent)).capabilities.policy;
    let policy_ask=match duduclaw_security::policy_kernel::evaluate(&duduclaw_security::policy_kernel::ToolCallEvent{tool_name:tool,arguments:&args,agent_id:agent},&policies) {
        duduclaw_security::policy_kernel::Decision::Allow=>false,
        duduclaw_security::policy_kernel::Decision::Ask{..}=>true,
        _=>return fail(StatusCode::FORBIDDEN,"員工權限不允許這項會議操作。"),
    };
    let needs_review=match gates::action_review(&m.home,agent,tool,&args).await {Ok(v)=>v,Err(e)=>return fail(StatusCode::FORBIDDEN,&e.message)};
    let mut egress=if let Request::Create{turn_id,..}|Request::UploadLink{turn_id,..}=&request {
        if let Err(e)=gates::tool_gates(&m.home,agent,"meeting_transcribe").await{return fail(StatusCode::FORBIDDEN,&e.message);}
        if let Err(e)=m.background_allowed(agent){return fail(StatusCode::FORBIDDEN,&e);}
        match gates::egress_check_with_call(&m.home,agent,"meeting_transcribe",turn_id.as_deref(),&call_id){Ok(value)=>value,Err(e)=>return fail(StatusCode::FORBIDDEN,&e.message)}
    }else{None};
    if let (Some(binding),Some(egress))=(admission.as_ref(),egress.as_mut()) {
        egress["source_binding"]=binding.binding();
    }
    if policy_ask || egress.is_some() || needs_review || gates::approval_required(&m.home,agent,tool) || (matches!(request,Request::UploadLink{..}) && gates::approval_required(&m.home,agent,"meeting_transcribe")) {
        if let Err(e)=gates::obtain_approval(&m.home,agent,tool,"（會議逐字稿）",&args,egress.as_ref(),300,Duration::from_secs(2)).await{audit(&m.home,"meeting_job_approval",agent,agent,&call_id,"approval_refused",None,None);return fail(StatusCode::FORBIDDEN,&e.message);}
        audit(&m.home,"meeting_job_approval",agent,agent,&call_id,"approved",None,None);
    }
    // Re-read gates after a potentially long approval wait.
    if let Err(e)=gates::tool_gates(&m.home,agent,tool).await{return fail(StatusCode::FORBIDDEN,&e.message);}
    if m.policy_digest(agent).ok().as_ref()!=Some(&policy){return fail(StatusCode::FORBIDDEN,"員工權限已變更，請重新提交會議。");}
    if let Request::Create{turn_id,..}|Request::UploadLink{turn_id,..}=&request {
        if let Err(e)=crate::egress_gate::confirm_dispatch_with_call(&m.home,agent,tool,true,crate::egress_guard::query_turn(agent,turn_id.as_deref()),egress.as_ref(),&call_id) {
            audit(&m.home,"meeting_job_egress",agent,agent,&call_id,"egress_refused",None,None);
            return fail(StatusCode::FORBIDDEN,&e);
        }
        audit(&m.home,"meeting_job_egress",agent,agent,&call_id,"allowed",None,None);
    }
    if let Request::UploadLink{turn_id,..}=&request {
        return match crate::meeting_links::issue(m,agent,turn_id.as_deref()){Ok(value)=>Json(value).into_response(),Err(e)=>fail(StatusCode::BAD_REQUEST,&e)};
    }
    let result=match request {
        Request::UploadLink{..}=>unreachable!(),
        Request::Create{..}=>admission.expect("create admission").commit_checked(|| {
            if m.policy_digest(agent)?!=policy {return Err("員工權限已變更，請重新提交會議。".into());}
            m.background_allowed(agent)
        }),
        Request::Status{job_id,..}=>m.store.get(agent,&job_id),
        Request::Cancel{job_id,..}=>m.cancel(agent,&job_id),
    };
    match result {Ok(j)=>Json(j.ai_public()).into_response(),Err(e)=>fail(if e==store::NOT_FOUND {StatusCode::NOT_FOUND}else{StatusCode::BAD_REQUEST},&e)}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn meeting_protocol_has_closed_operations_and_no_model_supplied_owner() {
        assert!(serde_json::from_value::<Request>(json!({"protocol":"meeting_jobs_v1","op":"status","job_id":"x"})).is_ok());
        for value in [json!({"protocol":"meeting_jobs_v1","op":"status","job_id":"x","agent":"other"}),json!({"protocol":"meeting_jobs_v1","op":"download","job_id":"x"}),json!({"op":"status","job_id":"x"})] {
            assert!(serde_json::from_value::<Request>(value).is_err());
        }
    }
}
