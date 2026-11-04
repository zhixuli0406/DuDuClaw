//! A public capability grants only its own upload protocol, never dashboard APIs.
mod store;
mod traffic;
use std::{sync::Arc,net::SocketAddr,time::Instant};
use axum::{body::Body,extract::{State,ConnectInfo,Path,Request,DefaultBodyLimit},http::{HeaderMap,StatusCode},response::{IntoResponse,Response,Html},routing::{get,post,put},Router,Json};
use serde_json::{Value,json};
use crate::{meeting_jobs::Manager,meeting_uploads::Service};
use store::{Store,Link,INVALID};
struct PublicState {service:Arc<Service>,store:Store,rate:traffic::Admission}
const TOKEN_HEADER:&str="x-meeting-upload-token";
pub fn issue(manager:&Manager,agent:&str,turn:Option<&str>)->Result<Value,String>{
    manager.employee(agent)?;manager.background_allowed(agent)?;
    let binding=crate::meeting_jobs::notice::capture(agent,turn);
    if binding.requester.is_none() || binding.reply_channel.is_none(){return Err("請從使用者的原始對話建立會議上傳連結。".into());}
    let raw=std::fs::read_to_string(manager.home.join("config.toml")).map_err(|_|"請管理員設定控制台公開網址，或使用控制台的「會議逐字稿」頁面。")?;
    let config:toml::Table=raw.parse().map_err(|_|"無法讀取控制台公開網址設定。")?;
    let public=config.get("dashboard").and_then(|v|v.get("public_url")).and_then(|v|v.as_str())
        .and_then(crate::deep_link::valid_oauth_public_base).ok_or("請管理員設定控制台公開網址，或使用控制台的「會議逐字稿」頁面。")?;
    let url=url::Url::parse(&public.1).map_err(|_|"控制台公開網址格式錯誤。")?;
    if url.scheme()!="https" && !matches!(url.host_str(),Some("localhost"|"127.0.0.1"|"[::1]")) {return Err("請管理員將控制台公開網址設為 HTTPS，或使用控制台的「會議逐字稿」頁面。".into());}
    let base=crate::deep_link::oauth_dashboard_base(&manager.home,&public.0);
    let cfg=duduclaw_inference::transcription::config::TranscriptionConfig::load(&manager.home)?;
    let store=Store::open(&manager.home)?;
    let token=store.issue(agent,binding,cfg.max_bytes,manager.policy_digest(agent)?,chrono::Utc::now().timestamp())?;
    let link=store.get(&token,chrono::Utc::now().timestamp())?;
    crate::meeting_jobs::audit(&manager.home,"link_issue",link.binding.requester.as_deref().unwrap_or(agent),agent,&link.id,"allowed",None,None);
    // A fragment keeps the secret out of server access logs and Referer headers.
    Ok(json!({"upload_url":format!("{base}/meeting-upload#{token}"),"expires_in_seconds":1800,"max_bytes":cfg.max_bytes}))
}
pub fn router(service:Arc<Service>)->Result<Router,String>{
    let state=Arc::new(PublicState{store:Store::open(&service.manager.home)?,service,rate:Default::default()});
    Ok(Router::new().route("/meeting-upload",get(page))
        .route("/api/meeting-link",get(metadata).post(create))
        .route("/api/meeting-link/chunks/{index}",put(chunk))
        .route("/api/meeting-link/complete",post(complete))
        .layer(DefaultBodyLimit::max(1024))
        .layer(axum::middleware::map_response(|mut response:Response| async move {
            response.headers_mut().insert("cache-control","no-store".parse().unwrap());
            response.headers_mut().insert("referrer-policy","no-referrer".parse().unwrap());response
        })).with_state(state))
}
fn fail(message:&str)->Response {(if message=="嘗試次數過多，請稍後再試。"{StatusCode::TOO_MANY_REQUESTS}else{StatusCode::BAD_REQUEST},Json(json!({"message":message}))).into_response()}
fn auth(st:&PublicState,headers:&HeaderMap,peer:SocketAddr)->Result<Link,String>{
    let now=Instant::now();
    if !st.rate.pre_auth(peer.ip(),now){return Err("嘗試次數過多，請稍後再試。".into());}
    let _slot=traffic::AUTH_SLOTS.try_acquire().map_err(|_|"嘗試次數過多，請稍後再試。")?;
    let link=match check_link(st,headers){Ok(link)=>link,Err(e)=>{
        crate::meeting_jobs::audit(&st.service.manager.home,"link_access","public","unknown","unknown","token_or_policy_refused",None,None);
        return Err(e);
    }};
    if !st.rate.link(&link.id,now){return Err("嘗試次數過多，請稍後再試。".into());}
    Ok(link)
}

fn check_link(st:&PublicState,headers:&HeaderMap)->Result<Link,String>{
    if !duduclaw_core::gateway_instance::held(&st.service.manager.home){return Err("會議服務暫時無法使用。".into());}
    let token=headers.get(TOKEN_HEADER).and_then(|v|v.to_str().ok()).ok_or(INVALID)?;
    let link=st.store.get(token,chrono::Utc::now().timestamp())?;
    let manager=&st.service.manager;
    manager.employee(&link.agent)?;manager.background_allowed(&link.agent)?;
    if manager.policy_digest(&link.agent)?!=link.policy_digest {return Err("員工權限已變更，請索取新上傳連結。".into());}
    Ok(link)
}

fn owner(link:&Link)->String{
    use sha2::{Digest,Sha256};
    // All links issued to the same channel requester share a fairness budget.
    let channel=link.binding.reply_channel.as_deref().unwrap_or("").split(':').next().unwrap_or("");
    let identity=serde_json::json!([channel,link.binding.requester]);
    format!("meeting-link-owner:{}",hex::encode(Sha256::digest(identity.to_string().as_bytes())))
}
async fn metadata(State(st):State<Arc<PublicState>>,ConnectInfo(peer):ConnectInfo<SocketAddr>,headers:HeaderMap)->Response {
    let link=match auth(&st,&headers,peer){Ok(l)=>l,Err(e)=>return fail(&e)};
    let name=std::fs::read_to_string(st.service.manager.agent_dir(&link.agent).join("agent.toml")).ok()
        .and_then(|s|s.parse::<toml::Table>().ok()).and_then(|t|t.get("agent")?.get("name")?.as_str().map(str::to_owned)).unwrap_or_else(||"AI 員工".into());
    let progress=link.upload_id.as_deref().and_then(|id|st.service.status_for(&owner(&link),id).ok());
    Json(json!({"employee_name":name,"max_bytes":link.max_bytes,"upload":progress})).into_response()
}
async fn create(State(st):State<Arc<PublicState>>,ConnectInfo(peer):ConnectInfo<SocketAddr>,headers:HeaderMap,body:Body)->Response {
    #[derive(serde::Deserialize)] #[serde(deny_unknown_fields)] struct Input{size:u64}
    let link=match auth(&st,&headers,peer){Ok(l)=>l,Err(e)=>return fail(&e)};
    let Ok(_flight)=crate::meeting_uploads::traffic::Flight::acquire(&owner(&link))else{return fail("嘗試次數過多，請稍後再試。");};
    let Ok(_slot)=crate::meeting_uploads::traffic::UPLOAD_SLOTS.try_acquire()else{return fail("嘗試次數過多，請稍後再試。");};
    let body=match crate::meeting_uploads::traffic::read_body(body,1024).await{Ok(b)=>b,Err(e)=>return fail(&e)};
    let input:Input=match serde_json::from_slice(&body){Ok(i)=>i,Err(_)=>return fail("上傳資料格式不正確。")};
    if input.size==0||input.size>link.max_bytes{return fail("檔案超過這個連結的大小上限。");}
    if let Some(id)=&link.upload_id {return match st.service.status_for(&owner(&link),id){Ok(v)=>Json(v).into_response(),Err(e)=>fail(&e)};}
    if st.store.change(&link.id,"fresh","creating",None).is_err(){return fail(INVALID);}
    match st.service.create_checked_for(&owner(&link),&link.agent,input.size,||check_link(&st,&headers).map(|_|())).await {
        Ok(value)=>{
            let Some(id)=value["upload_id"].as_str()else{return fail("無法建立上傳。");};
            match st.store.change(&link.id,"creating","uploading",Some(id)){Ok(_)=>{crate::meeting_jobs::audit(&st.service.manager.home,"link_redeem",&owner(&link),&link.agent,&link.id,"allowed",None,None);Json(value).into_response()},Err(e)=>fail(&e)}
        },
        Err(e)=>{let _=st.store.change(&link.id,"creating","revoked",None);crate::meeting_jobs::audit(&st.service.manager.home,"link_redeem",&owner(&link),&link.agent,&link.id,"admission_refused",None,None);fail(&e)}
    }
}
async fn chunk(State(st):State<Arc<PublicState>>,ConnectInfo(peer):ConnectInfo<SocketAddr>,Path(index):Path<u64>,request:Request)->Response {
    let link=match auth(&st,request.headers(),peer){Ok(l)=>l,Err(e)=>return fail(&e)};
    let Some(id)=link.upload_id.as_deref()else{return fail(INVALID);};
    let expected=match st.service.preflight_for(&owner(&link),id,index){Ok(n)=>n,Err(e)=>return fail(&e)};
    if request.headers().get("content-length").and_then(|h|h.to_str().ok()).and_then(|v|v.parse::<usize>().ok())!=Some(expected){return fail("上傳分段大小不正確。");}
    let hash=match request.headers().get("x-chunk-sha256").and_then(|h|h.to_str().ok()){Some(h)=>h.to_string(),None=>return fail("缺少分段驗證資料。")};
    let Ok(_slot)=crate::meeting_uploads::traffic::UPLOAD_SLOTS.try_acquire() else{return fail("目前有其他分段正在上傳，請稍後重試。");};
    let Ok(_flight)=crate::meeting_uploads::traffic::Flight::acquire(&owner(&link))else{return fail("嘗試次數過多，請稍後再試。");};
    let headers=request.headers().clone();
    let bytes=match crate::meeting_uploads::traffic::read_body(request.into_body(),expected).await{Ok(b)=>b,Err(e)=>return fail(&e)};
    if let Err(e)=check_link(&st,&headers){return fail(&e);}
    match st.service.put_checked_for(&owner(&link),id,index,&hash,bytes,||check_link(&st,&headers).map(|_|())).await {Ok(v)=>Json(v).into_response(),Err(e)=>fail(&e)}
}
async fn complete(State(st):State<Arc<PublicState>>,ConnectInfo(peer):ConnectInfo<SocketAddr>,headers:HeaderMap)->Response {
    let link=match auth(&st,&headers,peer){Ok(l)=>l,Err(e)=>return fail(&e)};
    let Some(id)=&link.upload_id else{return fail(INVALID);};
    match st.service.complete_checked_for(&owner(&link),id,Some(link.binding.clone()),||check_link(&st,&headers).map(|_|())).await {
        Ok(v)=>{let _=st.store.change(&link.id,"uploading","done",None);Json(json!({"job_id":v["job_id"],"state":v["state"]})).into_response()},
        Err(e)=>fail(&e)
    }
}
async fn page()->Response {
    let mut response=Html(include_str!("upload.html")).into_response();
    response.headers_mut().insert("Cache-Control","no-store".parse().unwrap());
    response.headers_mut().insert("Referrer-Policy","no-referrer".parse().unwrap());
    response.headers_mut().insert("X-Content-Type-Options","nosniff".parse().unwrap());
    response.headers_mut().insert("Content-Security-Policy","default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'".parse().unwrap());
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;
    #[tokio::test]
    async fn public_router_accepts_only_its_own_token_protocol() {
        let home=tempfile::tempdir().unwrap();
        duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
        let manager=Manager::open(home.path().to_path_buf()).unwrap();
        let service=Service::open(manager).unwrap();
        let router=router(service).unwrap();
        let request=axum::http::Request::builder().uri("/api/meeting-link")
            .header("authorization","Bearer dashboard-session")
            .extension(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()))
            .body(axum::body::Body::empty()).unwrap();
        let response=router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(),StatusCode::BAD_REQUEST);
        let request=axum::http::Request::builder().uri("/api/meeting-jobs")
            .header(TOKEN_HEADER,"a".repeat(64)).body(axum::body::Body::empty()).unwrap();
        assert_eq!(router.oneshot(request).await.unwrap().status(),StatusCode::NOT_FOUND);
    }
    #[test]
    fn requester_budget_is_shared_across_conversations_and_links(){
        let mut link=Link{id:"one".into(),agent:"alice".into(),binding:crate::meeting_jobs::NotificationBinding{reply_channel:Some("webchat:person:one".into()),requester:Some("person".into()),..Default::default()},max_bytes:1,policy_digest:String::new(),expires:1,upload_id:None,state:"fresh".into()};
        let first=owner(&link);link.id="two".into();link.agent="bob".into();link.binding.reply_channel=Some("webchat:person:two".into());
        assert_eq!(first,owner(&link));link.binding.requester=Some("another".into());assert_ne!(first,owner(&link));
    }

}
