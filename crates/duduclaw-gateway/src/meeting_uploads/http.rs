//! Dashboard Bearer-session surface. No cookie or public-token authentication.
use super::*;
use axum::{Router,Json,body::Body,extract::{Path as RoutePath,Query,State},http::{HeaderMap,StatusCode,header,HeaderValue},response::{Response,IntoResponse},routing::{get,post,put}};
use duduclaw_auth::{UserContext,AccessLevel};
use serde::Deserialize;
use std::path::Path;

pub type Authenticate=Arc<dyn Fn(&HeaderMap)->Result<UserContext,Response>+Send+Sync>;
#[path = "drive_http.rs"]
mod drive_http;
struct RouteState{service:Arc<Service>,authenticate:Authenticate,user_db:Option<Arc<duduclaw_auth::UserDb>>,selection_rate:super::selection::RegistrationRate}
use super::traffic::{UPLOAD_SLOTS,DOWNLOAD_SLOTS,Flight,read_body};
pub fn router(service:Arc<Service>,authenticate:Authenticate)->Router{
    routes(service,authenticate,None)
}
pub fn router_with_google(service:Arc<Service>,authenticate:Authenticate,user_db:Arc<duduclaw_auth::UserDb>)->Router{
    routes(service,authenticate,Some(user_db))
}
fn routes(service:Arc<Service>,authenticate:Authenticate,user_db:Option<Arc<duduclaw_auth::UserDb>>)->Router{
    Router::new()
        .route("/api/meeting-uploads",post(create))
        .route("/api/meeting-uploads/{id}",get(status))
        .route("/api/meeting-uploads/{id}/{index}",put(chunk))
        .route("/api/meeting-uploads/{id}/complete",post(complete))
        .route("/api/meeting-uploads/{id}/cancel",post(cancel_upload))
        .route("/api/meeting-jobs",get(list))
        .route("/api/meeting-jobs/{id}/cancel",post(cancel))
        .route("/api/meeting-jobs/{id}/download",get(download))
        .merge(drive_http::routes())
        .with_state(Arc::new(RouteState{service,authenticate,user_db,selection_rate:Default::default()}))
}
fn error(code:StatusCode,message:&str)->Response{(code,Json(json!({"error":message}))).into_response()}
fn result(value:Result<Value,String>)->Response{match value{Ok(value)=>Json(value).into_response(),Err(message)=>error(if message==store::NOT_FOUND{StatusCode::NOT_FOUND}else{StatusCode::BAD_REQUEST},&message)}}
fn identity(st:&RouteState,headers:&HeaderMap)->Result<UserContext,Response>{
    if !duduclaw_core::gateway_instance::held(&st.service.manager.home){return Err(error(StatusCode::SERVICE_UNAVAILABLE,"會議服務暫時無法使用。"));}
    (st.authenticate)(headers)
}
async fn authorize(st:&RouteState,ctx:&UserContext,agent:&str,level:AccessLevel)->Result<(),Response>{
    let refused=||{
        crate::meeting_jobs::audit(&st.service.manager.home,"meeting_dashboard_access",&ctx.user_id,agent,"dashboard","authorization_refused",None,None);
        error(StatusCode::NOT_FOUND,"找不到這位員工或無法使用。")
    };
    st.service.manager.employee(agent).map_err(|_|refused())?;
    let live=crate::review_evidence::audience::fresh_dashboard_context(&st.service.manager.home,ctx).map_err(|_|refused())?;
    if !live.has_agent_access(agent,level){return Err(refused());}
    let employee=duduclaw_agent::registry::AgentRegistry::load_agent(&st.service.manager.agent_dir(agent)).await.map_err(|_|refused())?;
    if !employee.config.agent.status.is_operational(){return Err(refused());}
    if !live.is_admin(){
        if crate::machine_employee::requires_admin(&employee){return Err(refused());}
        let default=crate::webchat::access::gateway_default_id(&st.service.manager.home).await.map_err(|_|refused())?;
        if employee.config.agent.role==duduclaw_core::types::AgentRole::Main || agent==default{return Err(refused());}
    }
    authorize_commit(st,ctx,agent,level).map_err(|_|refused())
}
/// Last read immediately before a mutation, after every lock/review await.
fn authorize_commit(st:&RouteState,ctx:&UserContext,agent:&str,level:AccessLevel)->Result<(),String>{
    let denied="找不到這位員工或無法使用。";
    let home=&st.service.manager.home;
    let live=crate::review_evidence::audience::fresh_dashboard_context(home,ctx).map_err(|_|denied)?;
    if !live.has_agent_access(agent,level){return Err(denied.into());}
    let raw=duduclaw_core::agent_toml::read_effective_text(&st.service.manager.agent_dir(agent)).map_err(|_|denied)?;
    let config:duduclaw_core::types::AgentConfig=toml::from_str(&raw).map_err(|_|denied)?;
    if config.agent.name!=agent || !config.agent.status.is_operational(){return Err(denied.into());}
    if !live.is_admin(){
        if crate::machine_employee::requires_admin_for_id(home,agent) || config.agent.role==duduclaw_core::types::AgentRole::Main{return Err(denied.into());}
        let raw=match std::fs::read_to_string(home.join("config.toml")){Ok(v)=>v,Err(e)if e.kind()==std::io::ErrorKind::NotFound=>String::new(),Err(_)=>return Err(denied.into())};
        let global:toml::Value=raw.parse().map_err(|_|denied)?;
        if let Some(general)=global.get("general"){
            let general=general.as_table().ok_or(denied)?;
            if let Some(default)=general.get("default_agent"){
                if default.as_str().ok_or(denied)?==agent{return Err(denied.into());}
            }
        }
    }
    Ok(())
}
#[derive(Deserialize)]#[serde(deny_unknown_fields)]struct NewUpload{agent_id:String,file_name:String,size:u64}
async fn create(State(st):State<Arc<RouteState>>,headers:HeaderMap,body:Body)->Response{
    let ctx=match identity(&st,&headers){Ok(c)=>c,Err(r)=>return r};
    let Ok(_flight)=Flight::acquire(&ctx.user_id)else{return error(StatusCode::TOO_MANY_REQUESTS,"請稍後重試。");};
    let Ok(_slot)=UPLOAD_SLOTS.try_acquire()else{return error(StatusCode::TOO_MANY_REQUESTS,"請稍後重試。");};
    let body=match read_body(body,4096).await{Ok(b)=>b,Err(e)=>return error(StatusCode::REQUEST_TIMEOUT,&e)};
    let input:NewUpload=match serde_json::from_slice(&body){Ok(v)=>v,Err(_)=>return error(StatusCode::BAD_REQUEST,"上傳資訊格式錯誤。")};
    if input.file_name.is_empty() || input.file_name.len()>512 || input.file_name.chars().any(char::is_control){return error(StatusCode::BAD_REQUEST,"檔案名稱無效。");}
    if let Err(r)=authorize(&st,&ctx,&input.agent_id,AccessLevel::Operator).await{return r;}
    result(st.service.create_checked_for(&ctx.user_id,&input.agent_id,input.size,||authorize_commit(&st,&ctx,&input.agent_id,AccessLevel::Operator)).await)
}
async fn status(State(st):State<Arc<RouteState>>,RoutePath(id):RoutePath<String>,headers:HeaderMap)->Response{
    let ctx=match identity(&st,&headers){Ok(c)=>c,Err(r)=>return r};
    let upload=match st.service.get_for(&ctx.user_id,&id){Ok(u)=>u,Err(e)=>return error(StatusCode::NOT_FOUND,&e)};
    if let Err(r)=authorize(&st,&ctx,&upload.agent,AccessLevel::Viewer).await{return r;}
    result(st.service.status_for(&ctx.user_id,&id))
}
async fn chunk(State(st):State<Arc<RouteState>>,RoutePath((id,index)):RoutePath<(String,u64)>,headers:HeaderMap,body:Body)->Response{
    let ctx=match identity(&st,&headers){Ok(c)=>c,Err(r)=>return r};
    let upload=match st.service.get_for(&ctx.user_id,&id){Ok(u)=>u,Err(e)=>return error(StatusCode::NOT_FOUND,&e)};
    if let Err(r)=authorize(&st,&ctx,&upload.agent,AccessLevel::Operator).await{return r;}
    let expected=match st.service.preflight_for(&ctx.user_id,&id,index){Ok(n)=>n,Err(e)=>return error(StatusCode::BAD_REQUEST,&e)};
    if headers.get(header::CONTENT_LENGTH).is_some_and(|h|h.to_str().ok().and_then(|s|s.parse::<usize>().ok())!=Some(expected)){return error(StatusCode::PAYLOAD_TOO_LARGE,"上傳分段大小不符。");}
    let Some(hash)=headers.get("x-chunk-sha256").and_then(|h|h.to_str().ok())else{return error(StatusCode::BAD_REQUEST,"缺少上傳驗證碼。");};
    if hash.len()!=64{return error(StatusCode::BAD_REQUEST,"上傳驗證碼格式錯誤。");}
    let Ok(_slot)=UPLOAD_SLOTS.try_acquire()else{return error(StatusCode::TOO_MANY_REQUESTS,"目前有其他分段正在上傳，請稍後重試。");};
    let Ok(_flight)=Flight::acquire(&ctx.user_id)else{return error(StatusCode::TOO_MANY_REQUESTS,"同一上傳者已有請求正在處理。");};
    let bytes=match read_body(body,expected).await{Ok(b)=>b,Err(e)=>return error(StatusCode::REQUEST_TIMEOUT,&e)};
    // A revoked binding during a slow upload cannot commit buffered bytes.
    if let Err(r)=authorize(&st,&ctx,&upload.agent,AccessLevel::Operator).await{return r;}
    result(st.service.put_checked_for(&ctx.user_id,&id,index,hash,bytes,||authorize_commit(&st,&ctx,&upload.agent,AccessLevel::Operator)).await)
}
async fn complete(State(st):State<Arc<RouteState>>,RoutePath(id):RoutePath<String>,headers:HeaderMap)->Response{
    let ctx=match identity(&st,&headers){Ok(c)=>c,Err(r)=>return r};
    let upload=match st.service.get_for(&ctx.user_id,&id){Ok(u)=>u,Err(e)=>return error(StatusCode::NOT_FOUND,&e)};
    if let Err(r)=authorize(&st,&ctx,&upload.agent,AccessLevel::Operator).await{return r;}
    result(st.service.complete_checked_for(&ctx.user_id,&id,None,||authorize_commit(&st,&ctx,&upload.agent,AccessLevel::Operator)).await)
}
async fn cancel_upload(State(st):State<Arc<RouteState>>,RoutePath(id):RoutePath<String>,headers:HeaderMap)->Response{
    let ctx=match identity(&st,&headers){Ok(c)=>c,Err(r)=>return r};
    let upload=match st.service.get_for(&ctx.user_id,&id){Ok(u)=>u,Err(e)=>return error(StatusCode::NOT_FOUND,&e)};
    if let Err(r)=authorize(&st,&ctx,&upload.agent,AccessLevel::Operator).await{return r;}
    result(st.service.cancel_checked_for(&ctx.user_id,&id,||authorize_commit(&st,&ctx,&upload.agent,AccessLevel::Operator)).await)
}
#[derive(Deserialize)]#[serde(deny_unknown_fields)]struct AgentQuery{agent:String}
async fn list(State(st):State<Arc<RouteState>>,Query(query):Query<AgentQuery>,headers:HeaderMap)->Response{
    let ctx=match identity(&st,&headers){Ok(c)=>c,Err(r)=>return r};
    if let Err(r)=authorize(&st,&ctx,&query.agent,AccessLevel::Viewer).await{return r;}
    result((||{let jobs=st.service.manager.list(&query.agent)?.iter().map(|j|j.public()).collect::<Vec<_>>();
        Ok(json!({"jobs":jobs,"max_bytes":TranscriptionConfig::load(&st.service.manager.home)?.max_bytes}))})())
}
async fn cancel(State(st):State<Arc<RouteState>>,RoutePath(id):RoutePath<String>,Query(query):Query<AgentQuery>,headers:HeaderMap)->Response{
    let ctx=match identity(&st,&headers){Ok(c)=>c,Err(r)=>return r};
    if let Err(r)=authorize(&st,&ctx,&query.agent,AccessLevel::Operator).await{return r;}
    if let Err(e)=authorize_commit(&st,&ctx,&query.agent,AccessLevel::Operator){return error(StatusCode::NOT_FOUND,&e);}
    let cancelled=st.service.manager.cancel(&query.agent,&id);
    crate::meeting_jobs::audit(&st.service.manager.home,"meeting_dashboard_cancel",&ctx.user_id,&query.agent,&id,if cancelled.is_ok(){"allowed"}else{"job_unavailable"},None,None);
    result(cancelled.map(|j|j.public()))
}
#[derive(Deserialize)]#[serde(deny_unknown_fields)]struct DownloadQuery{agent:String,format:String}
async fn download(State(st):State<Arc<RouteState>>,RoutePath(id):RoutePath<String>,Query(query):Query<DownloadQuery>,headers:HeaderMap)->Response{
    let ctx=match identity(&st,&headers){Ok(c)=>c,Err(r)=>return r};
    if let Err(r)=authorize(&st,&ctx,&query.agent,AccessLevel::Viewer).await{return r;}
    let key=match query.format.as_str(){"md"=>"markdown_path","json"=>"json_path",_=>return error(StatusCode::BAD_REQUEST,"不支援這個下載格式。")};
    let job=match st.service.manager.get(&query.agent,&id){Ok(j)if j.state=="done"=>j,_=>return error(StatusCode::NOT_FOUND,"找不到這份逐字稿。")};
    let Some(path)=job.result.as_ref().and_then(|v|v[key].as_str())else{return error(StatusCode::NOT_FOUND,"找不到這份逐字稿。")};
    let Ok(_slot)=DOWNLOAD_SLOTS.try_acquire()else{return error(StatusCode::TOO_MANY_REQUESTS,"請稍後重試下載。");};
    let Ok(flight)=Flight::acquire(&ctx.user_id)else{return error(StatusCode::TOO_MANY_REQUESTS,"同一使用者已有傳輸正在處理。");};
    let bytes=match pipeline::read_attachment(&st.service.manager.agent_dir(&query.agent),Path::new(path),pipeline::MAX_OUTPUT_BYTES as u64){Ok(b)=>b,Err(_)=>return error(StatusCode::NOT_FOUND,"找不到這份逐字稿。")};
    // Keep both permits alive until the response is consumed or disconnected.
    let stream=futures_util::stream::unfold((Bytes::from(bytes),_slot,flight),|(mut bytes,slot,flight)|async move{
        if bytes.is_empty(){return None;}
        let part=bytes.split_to(bytes.len().min(64*1024));
        Some((Ok::<_,std::io::Error>(part),(bytes,slot,flight)))
    });
    let mut response=Response::new(Body::from_stream(stream));
    response.headers_mut().insert(header::CONTENT_TYPE,HeaderValue::from_static("application/octet-stream"));
    response.headers_mut().insert(header::CACHE_CONTROL,HeaderValue::from_static("no-store"));
    response.headers_mut().insert("x-content-type-options",HeaderValue::from_static("nosniff"));
    let filename=format!("meeting-{}.{}",job.id,query.format);
    if let Ok(value)=HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")){response.headers_mut().insert(header::CONTENT_DISPOSITION,value);}
    response
}

#[cfg(test)]mod tests{
    use super::*;
    use tower::ServiceExt;
    use axum::http::Request;
    use std::sync::atomic::{AtomicBool,Ordering};
    struct Fixture{_home:tempfile::TempDir,service:Arc<Service>,db:Arc<duduclaw_auth::UserDb>,ctx:UserContext}
    async fn fixture()->Fixture{
        let home=tempfile::tempdir().unwrap();let path=home.path().canonicalize().unwrap();
        crate::team_edition::record(&path,duduclaw_core::EditionProfile::Enterprise);
        for (id,role) in [("alice","specialist"),("main","main"),("other","specialist")]{
            crate::trigger_route::tests::write_agent(&path.join("agents"),id,id,"",role,"\n[capabilities]\nallowed_tools=['mcp__duduclaw__*']\n");
        }
        let db=Arc::new(duduclaw_auth::UserDb::new(&path.join("users.db")).unwrap());
        let user=db.create_user("meeting@test.invalid","Meeting","test-password-12345",duduclaw_auth::UserRole::Employee).unwrap();
        db.bind_agent(&user.id,"alice",AccessLevel::Operator).unwrap();
        db.bind_agent(&user.id,"main",AccessLevel::Viewer).unwrap();
        let ctx=db.live_user_context(&user.id).unwrap().unwrap();
        duduclaw_core::gateway_instance::acquire(&path).unwrap();
        let service=Service::open(crate::meeting_jobs::Manager::open(path).unwrap()).unwrap();
        Fixture{_home:home,service,db,ctx}
    }
    fn app(f:&Fixture)->Router{
        let ctx=f.ctx.clone();
        router(f.service.clone(),Arc::new(move|headers|{
            if headers.get(header::AUTHORIZATION).and_then(|v|v.to_str().ok())!=Some("Bearer test-session"){return Err(error(StatusCode::UNAUTHORIZED,"請登入。"));}
            Ok(ctx.clone())
        }))
    }
    fn req(method:&str,uri:&str,body:Body)->Request<Body>{Request::builder().method(method).uri(uri).header(header::AUTHORIZATION,"Bearer test-session").body(body).unwrap()}
    #[tokio::test]
    async fn unauthenticated_upload_never_reads_the_request_body(){
        let f=fixture().await;let read=Arc::new(AtomicBool::new(false));let marker=read.clone();
        let body=Body::from_stream(futures_util::stream::once(async move{marker.store(true,Ordering::SeqCst);Ok::<_,std::io::Error>(Bytes::from_static(b"untrusted"))}));
        let response=app(&f).oneshot(Request::builder().method("POST").uri("/api/meeting-uploads").body(body).unwrap()).await.unwrap();
        assert_eq!(response.status(),StatusCode::UNAUTHORIZED);assert!(!read.load(Ordering::SeqCst));
    }
    #[tokio::test]
    async fn routes_enforce_live_bound_employee_main_and_machine_restrictions(){
        let f=fixture().await;
        for (agent,status) in [("alice",StatusCode::OK),("other",StatusCode::NOT_FOUND),("main",StatusCode::NOT_FOUND)]{
            let response=app(&f).oneshot(req("GET",&format!("/api/meeting-jobs?agent={agent}"),Body::empty())).await.unwrap();
            assert_eq!(response.status(),status,"{agent}");
        }
        std::fs::write(f.service.manager.home.join("preset_bindings.toml"),"[agents.alice]\npreset_id='system-operator'\nversion='1'\n").unwrap();
        assert_eq!(app(&f).oneshot(req("GET","/api/meeting-jobs?agent=alice",Body::empty())).await.unwrap().status(),StatusCode::NOT_FOUND);
        std::fs::remove_file(f.service.manager.home.join("preset_bindings.toml")).unwrap();
        f.db.unbind_agent(&f.ctx.user_id,"alice").unwrap();
        assert_eq!(app(&f).oneshot(req("GET","/api/meeting-jobs?agent=alice",Body::empty())).await.unwrap().status(),StatusCode::NOT_FOUND);
    }
    #[tokio::test]
    async fn chunk_route_refuses_oversized_length_before_body_and_wrong_hash(){
        let f=fixture().await;
        let created=f.service.create_for(&f.ctx.user_id,"alice",12).await.unwrap();let id=created["upload_id"].as_str().unwrap();
        let uri=format!("/api/meeting-uploads/{id}/0");
        let read=Arc::new(AtomicBool::new(false));let marker=read.clone();
        let body=Body::from_stream(futures_util::stream::once(async move{marker.store(true,Ordering::SeqCst);Ok::<_,std::io::Error>(Bytes::from_static(b"too-large-data"))}));
        let mut request=req("PUT",&uri,body);
        request.headers_mut().insert(header::CONTENT_LENGTH,"999999999".parse().unwrap());
        assert_eq!(app(&f).oneshot(request).await.unwrap().status(),StatusCode::PAYLOAD_TOO_LARGE);
        assert!(!read.load(Ordering::SeqCst));
        let mut request=req("PUT",&uri,Body::from("RIFF0000WAVE"));
        request.headers_mut().insert("x-chunk-sha256","0".repeat(64).parse().unwrap());
        assert_eq!(app(&f).oneshot(request).await.unwrap().status(),StatusCode::BAD_REQUEST);
        assert_eq!(f.service.status_for(&f.ctx.user_id,id).unwrap()["received"],json!([]));
    }
    #[tokio::test]
    async fn revoked_binding_during_chunk_body_read_cannot_commit(){
        let f=fixture().await;
        let created=f.service.create_for(&f.ctx.user_id,"alice",12).await.unwrap();let id=created["upload_id"].as_str().unwrap();
        let db=f.db.clone();let user=f.ctx.user_id.clone();
        let body=Body::from_stream(futures_util::stream::once(async move{
            db.unbind_agent(&user,"alice").unwrap();Ok::<_,std::io::Error>(Bytes::from_static(b"RIFF0000WAVE"))
        }));
        let mut request=req("PUT",&format!("/api/meeting-uploads/{id}/0"),body);
        request.headers_mut().insert("x-chunk-sha256",hex::encode(Sha256::digest(b"RIFF0000WAVE")).parse().unwrap());
        assert_eq!(app(&f).oneshot(request).await.unwrap().status(),StatusCode::NOT_FOUND);
        assert_eq!(f.service.status_for(&f.ctx.user_id,id).unwrap()["received"],json!([]));
    }
    #[tokio::test]
    async fn viewer_can_read_but_cannot_create_chunk_complete_or_cancel(){
        let f=fixture().await;
        let created=f.service.create_for(&f.ctx.user_id,"alice",12).await.unwrap();let id=created["upload_id"].as_str().unwrap();
        f.db.bind_agent(&f.ctx.user_id,"alice",AccessLevel::Viewer).unwrap();
        for uri in ["/api/meeting-jobs?agent=alice".to_string(),format!("/api/meeting-uploads/{id}")]{
            assert_eq!(app(&f).oneshot(req("GET",&uri,Body::empty())).await.unwrap().status(),StatusCode::OK);
        }
        let body=Body::from(json!({"agent_id":"alice","file_name":"a.wav","size":12}).to_string());
        assert_eq!(app(&f).oneshot(req("POST","/api/meeting-uploads",body)).await.unwrap().status(),StatusCode::NOT_FOUND);
        for (method,uri) in [("PUT",format!("/api/meeting-uploads/{id}/0")),("POST",format!("/api/meeting-uploads/{id}/complete")),("POST",format!("/api/meeting-uploads/{id}/cancel")),("POST","/api/meeting-jobs/no-id/cancel?agent=alice".into())]{
            assert_eq!(app(&f).oneshot(req(method,&uri,Body::empty())).await.unwrap().status(),StatusCode::NOT_FOUND);
        }
    }
    #[tokio::test]
    async fn downgrade_after_last_chunk_refuses_complete_without_creating_job(){
        let f=fixture().await;
        let created=f.service.create_for(&f.ctx.user_id,"alice",12).await.unwrap();let id=created["upload_id"].as_str().unwrap();
        f.service.put_for(&f.ctx.user_id,id,0,&hex::encode(Sha256::digest(b"RIFF0000WAVE")),Bytes::from_static(b"RIFF0000WAVE")).await.unwrap();
        f.db.bind_agent(&f.ctx.user_id,"alice",AccessLevel::Viewer).unwrap();
        assert_eq!(app(&f).oneshot(req("POST",&format!("/api/meeting-uploads/{id}/complete"),Body::empty())).await.unwrap().status(),StatusCode::NOT_FOUND);
        assert!(f.service.manager.list("alice").unwrap().is_empty());
    }


    #[tokio::test]
    async fn drive_import_refuses_employee_tokens_before_reading_body(){
        let f=fixture().await;
        for path in ["/api/meeting-drive/import","/api/meeting-drive/selection"]{
        for token in [None,Some("Bearer employee-mcp-token")]{
            let read=Arc::new(AtomicBool::new(false));let marker=read.clone();
            let body=Body::from_stream(futures_util::stream::once(async move{marker.store(true,Ordering::SeqCst);Ok::<_,std::io::Error>(Bytes::from_static(b"untrusted"))}));
            let mut request=Request::builder().method("POST").uri(path);
            if let Some(token)=token{request=request.header(header::AUTHORIZATION,token);}
            let response=app(&f).oneshot(request.body(body).unwrap()).await.unwrap();
            assert_eq!(response.status(),StatusCode::UNAUTHORIZED);assert!(!read.load(Ordering::SeqCst));
        }
        }
        let audit=std::fs::read_to_string(f.service.manager.home.join("security_audit.jsonl")).unwrap();
        assert!(audit.contains("meeting_drive_import_request") && audit.contains("meeting_drive_selection"));
        assert!(audit.contains("authentication_refused") && !audit.contains("employee-mcp-token"));
    }
    #[tokio::test]
    async fn drive_import_requires_selection_confirmation_and_rejects_owner_override(){
        let f=fixture().await;
        for body in [
            json!({"agent_id":"alice","file_id":"recording_123456","confirm":false}),
            json!({"agent_id":"alice","file_id":"https://drive.google.com/file/d/123","confirm":true}),
            json!({"agent_id":"alice","file_id":"recording_123456","confirm":true,"owner":"other"}),
        ]{
            assert_eq!(app(&f).oneshot(req("POST","/api/meeting-drive/import",Body::from(body.to_string()))).await.unwrap().status(),StatusCode::BAD_REQUEST);
        }
        assert!(f.service.manager.list("alice").unwrap().is_empty());
        let audit=std::fs::read_to_string(f.service.manager.home.join("security_audit.jsonl")).unwrap();
        assert!(audit.contains("meeting_drive_import_request") && audit.contains("request_refused"));
    }
    #[tokio::test]
    async fn drive_routes_require_live_operator_binding_before_google_access(){
        let f=fixture().await;
        for agent in ["other","main"]{
            let body=json!({"agent_id":agent,"file_id":"recording_123456","confirm":true});
            assert_eq!(app(&f).oneshot(req("POST","/api/meeting-drive/import",Body::from(body.to_string()))).await.unwrap().status(),StatusCode::NOT_FOUND);
            let selection=json!({"agent_id":agent,"file_id":"recording_123456","confirm":true});
            assert_eq!(app(&f).oneshot(req("POST","/api/meeting-drive/selection",Body::from(selection.to_string()))).await.unwrap().status(),StatusCode::NOT_FOUND);
        }
        f.db.unbind_agent(&f.ctx.user_id,"alice").unwrap();
        assert_eq!(app(&f).oneshot(req("GET","/api/meeting-drive/files?agent=alice",Body::empty())).await.unwrap().status(),StatusCode::NOT_FOUND);
    }
    #[tokio::test]
    async fn drive_list_query_rejections_are_audited_without_query_content(){
        let f=fixture().await;
        for path in ["files","recordings"]{
            let uri=format!("/api/meeting-drive/{path}?agent=alice&unexpected=private-search-value");
            assert_eq!(app(&f).oneshot(req("GET",&uri,Body::empty())).await.unwrap().status(),StatusCode::BAD_REQUEST);
        }
        let audit=std::fs::read_to_string(f.service.manager.home.join("security_audit.jsonl")).unwrap();
        assert!(audit.contains("meeting_drive_list") && audit.contains("meeting_meet_list") && audit.contains("query_refused"));
        assert!(!audit.contains("private-search-value"));
    }
}
