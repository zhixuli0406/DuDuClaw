//! These routes accept the same human dashboard JWT as uploads, never an
//! employee MCP token, capability link, cookie or caller-supplied owner ID.
use super::*;
use crate::{google_personal::{self,meeting_access},person_credentials};
use crate::meeting_uploads::drive;
use duduclaw_auth::UserDb;

pub(super) fn routes()->Router<Arc<RouteState>>{
    Router::new()
        .route("/api/meeting-drive/status",get(status))
        .route("/api/meeting-drive/files",get(files))
        .route("/api/meeting-drive/recordings",get(recordings))
        .route("/api/meeting-drive/selection",post(selection))
        .route("/api/meeting-drive/import",post(import))
}
fn audited_identity(st:&RouteState,headers:&HeaderMap,event:&'static str)->Result<UserContext,Response>{
    identity(st,headers).inspect_err(|_|crate::meeting_jobs::audit(&st.service.manager.home,event,"unauthenticated","","dashboard","authentication_refused",None,None))
}
fn private_result(value:Result<Value,String>)->Response{
    let mut response=result(value);
    response.headers_mut().insert(header::CACHE_CONTROL,HeaderValue::from_static("no-store"));
    response
}
fn db<'a>(st:&'a RouteState,ctx:&UserContext)->Result<&'a UserDb,String>{
    if !crate::team_edition::team_grants_enabled(&st.service.manager.home){return Err("請使用團隊版的個人 Google 連線。".into());}
    let db=st.user_db.as_deref().ok_or("個人連線服務尚未就緒。")?;
    google_personal::flow::active_user(db,&ctx.user_id)?;
    Ok(db)
}
#[derive(Deserialize)]#[serde(deny_unknown_fields)]
struct PickerConfig{developer_key:String,app_id:String}
fn picker_config(home:&Path)->Result<Option<PickerConfig>,String>{
    let raw=std::fs::read_to_string(home.join("config.toml")).map_err(|_|"無法確認 Google 選檔設定，請聯絡管理員。")?;
    let config:toml::Value=raw.parse().map_err(|_|"無法確認 Google 選檔設定，請聯絡管理員。")?;
    let Some(value)=config.get("integrations").and_then(|v|v.get("google_meeting_picker"))else{return Ok(None)};
    let cfg:PickerConfig=value.clone().try_into().map_err(|_|"Google 選檔設定不完整，請聯絡管理員。")?;
    if !(20..=200).contains(&cfg.developer_key.len()) || !cfg.developer_key.bytes().all(|b|b.is_ascii_alphanumeric()||matches!(b,b'_'|b'-'))
        || cfg.app_id.is_empty() || cfg.app_id.len()>32 || !cfg.app_id.bytes().all(|b|b.is_ascii_digit()){
        return Err("Google 選檔設定不完整，請聯絡管理員。".into());
    }
    Ok(Some(cfg))
}
async fn status(State(st):State<Arc<RouteState>>,headers:HeaderMap)->Response{
    let ctx=match identity(&st,&headers){Ok(c)=>c,Err(r)=>return r};
    private_result((||{
        let db=db(&st,&ctx)?;
        let home=&st.service.manager.home;
        let record=person_credentials::get(home,&ctx.user_id)?;
        let picker=picker_config(home)?;
        let client=crate::mcp_oauth::get_client_config(home,"google");
        let authorized=record.as_ref().is_some_and(|r|meeting_access::check_metadata(home,db,&ctx.user_id,&r.metadata).is_ok()
            && client.as_ref().is_some_and(|c|!c.client_id.is_empty() && c.client_id==r.metadata.oauth_client_id));
        Ok(json!({"connected":record.is_some(),"authorized":authorized,
            "picker_configured":picker.is_some() && client.as_ref().is_some_and(|c|!c.client_id.is_empty()),
            "client_id":client.as_ref().map(|c|&c.client_id),
            "developer_key":picker.as_ref().map(|p|&p.developer_key),"app_id":picker.as_ref().map(|p|&p.app_id),
            "account_email":record.as_ref().map(|r|&r.metadata.account_email)}))
    })())
}
#[derive(Deserialize)]#[serde(deny_unknown_fields)]
struct ListQuery{agent:String,#[serde(default)]search:String,#[serde(default)]page_token:String}
async fn files(State(st):State<Arc<RouteState>>,query:Result<Query<ListQuery>,axum::extract::rejection::QueryRejection>,headers:HeaderMap)->Response{
    list(st,headers,query.map(|Query(query)|query),false).await
}
async fn recordings(State(st):State<Arc<RouteState>>,query:Result<Query<ListQuery>,axum::extract::rejection::QueryRejection>,headers:HeaderMap)->Response{
    list(st,headers,query.map(|Query(query)|query),true).await
}
async fn list(st:Arc<RouteState>,headers:HeaderMap,query:Result<ListQuery,axum::extract::rejection::QueryRejection>,meet:bool)->Response{
    let event=if meet{"meeting_meet_list"}else{"meeting_drive_list"};
    let ctx=match audited_identity(&st,&headers,event){Ok(c)=>c,Err(r)=>return r};
    let query=match query{Ok(query)=>query,Err(_)=>{
        crate::meeting_jobs::audit(&st.service.manager.home,event,&ctx.user_id,"","list","query_refused",None,None);
        return error(StatusCode::BAD_REQUEST,"Google 清單查詢格式錯誤。");
    }};
    let mut audit=MutationAudit{home:&st.service.manager.home,event,owner:&ctx.user_id,agent:&query.agent,id:"list".into(),outcome:"request_refused"};
    let Ok(_flight)=Flight::acquire(&ctx.user_id)else{return error(StatusCode::TOO_MANY_REQUESTS,"請稍後重試。");};
    let Ok(_slot)=DOWNLOAD_SLOTS.try_acquire()else{return error(StatusCode::TOO_MANY_REQUESTS,"請稍後重試。");};
    if let Err(r)=authorize(&st,&ctx,&query.agent,AccessLevel::Operator).await{return r;}
    let result=async{
        let db=db(&st,&ctx)?;
        let home=&st.service.manager.home;
        let access=meeting_access::resolve(home,db,&ctx.user_id).await?;
        authorize_commit(&st,&ctx,&query.agent,AccessLevel::Operator)?;
        let result=if meet{drive::recordings(home,db,&access,&query.page_token).await}
            else{drive::files(home,db,&access,&query.search,&query.page_token,TranscriptionConfig::load(home)?.max_bytes).await};
        authorize_commit(&st,&ctx,&query.agent,AccessLevel::Operator)?;
        result
    }.await;
    if result.is_ok(){audit.outcome="allowed";}
    private_result(result)
}
#[derive(Deserialize)]#[serde(deny_unknown_fields)]
struct Selection{agent_id:String,file_id:String,confirm:bool}
async fn selection(State(st):State<Arc<RouteState>>,headers:HeaderMap,body:Body)->Response{
    let ctx=match audited_identity(&st,&headers,"meeting_drive_selection"){Ok(c)=>c,Err(r)=>return r};
    let mut audit=MutationAudit{home:&st.service.manager.home,event:"meeting_drive_selection",owner:&ctx.user_id,agent:"",id:"selection".into(),outcome:"request_refused"};
    let body=match read_body(body,4096).await{Ok(b)=>b,Err(e)=>return error(StatusCode::REQUEST_TIMEOUT,&e)};
    // Probe only the target identity first. Authorization precedes all semantic
    // body validation and rate limiting, so stale or unbound callers cannot use
    // malformed requests to bypass the live operator-binding check.
    let target=serde_json::from_slice::<serde_json::Value>(&body).ok()
        .and_then(|value|value.get("agent_id").and_then(serde_json::Value::as_str).map(str::to_owned));
    if let Some(agent)=target.as_deref(){
        if let Err(r)=authorize(&st,&ctx,agent,AccessLevel::Operator).await{return r;}
    }
    if !st.selection_rate.allow(&ctx.user_id,std::time::Instant::now()) {
        audit.outcome="rate_limited";
        return error(StatusCode::TOO_MANY_REQUESTS,"檔案確認太頻繁（每人每分鐘最多 20 次），請稍後重試。");
    }
    let Ok(_flight)=Flight::acquire(&ctx.user_id)else{return error(StatusCode::TOO_MANY_REQUESTS,"請稍後重試。");};
    let input:Selection=match serde_json::from_slice(&body){Ok(v)=>v,Err(_)=>return error(StatusCode::BAD_REQUEST,"選檔資訊格式錯誤。")};
    let mut target_audit=MutationAudit{home:&st.service.manager.home,event:"meeting_drive_selection_target",owner:&ctx.user_id,agent:&input.agent_id,id:input.file_id.clone(),outcome:"request_refused"};
    if !input.confirm || !drive::valid_file_id(&input.file_id){return error(StatusCode::BAD_REQUEST,"請確認要交給這位員工轉錄的 Google 檔案。");}
    let result=async{
        let db=db(&st,&ctx)?;
        let home=&st.service.manager.home;
        let access=meeting_access::resolve(home,db,&ctx.user_id).await?;
        authorize_commit(&st,&ctx,&input.agent_id,AccessLevel::Operator)?;
        access.validate(home,db)?;
        let receipt=crate::meeting_uploads::selection::issue(home,&ctx.user_id,&input.agent_id,&input.file_id)?;
        Ok(json!({"selection_receipt":receipt,"expires_in":crate::meeting_uploads::selection::TTL_SECONDS}))
    }.await;
    if result.is_ok(){audit.outcome="allowed";target_audit.outcome="allowed";}
    private_result(result)
}
#[derive(Deserialize)]#[serde(deny_unknown_fields)]
struct Import{agent_id:String,file_id:String,confirm:bool,#[serde(default)]selection_receipt:Option<String>}
async fn import(State(st):State<Arc<RouteState>>,headers:HeaderMap,body:Body)->Response{
    let ctx=match audited_identity(&st,&headers,"meeting_drive_import_request"){Ok(c)=>c,Err(r)=>return r};
    let mut audit=MutationAudit{home:&st.service.manager.home,event:"meeting_drive_import_request",owner:&ctx.user_id,agent:"",id:"import".into(),outcome:"request_refused"};
    let body=match read_body(body,4096).await{Ok(b)=>b,Err(e)=>return error(StatusCode::REQUEST_TIMEOUT,&e)};
    let target=serde_json::from_slice::<serde_json::Value>(&body).ok()
        .and_then(|value|value.get("agent_id").and_then(serde_json::Value::as_str).map(str::to_owned));
    if let Some(agent)=target.as_deref(){
        if let Err(r)=authorize(&st,&ctx,agent,AccessLevel::Operator).await{return r;}
    }
    let Ok(_flight)=Flight::acquire(&ctx.user_id)else{return error(StatusCode::TOO_MANY_REQUESTS,"請稍後重試。");};
    let Ok(_slot)=UPLOAD_SLOTS.try_acquire()else{return error(StatusCode::TOO_MANY_REQUESTS,"請稍後重試。");};
    let input:Import=match serde_json::from_slice(&body){Ok(v)=>v,Err(_)=>return error(StatusCode::BAD_REQUEST,"匯入資訊格式錯誤。")};
    if !input.confirm || !drive::valid_file_id(&input.file_id){return error(StatusCode::BAD_REQUEST,"請選擇 Google 錄影並確認要交給這位員工轉錄。");}
    let result=async{
        let db=db(&st,&ctx)?;
        let access=meeting_access::resolve(&st.service.manager.home,db,&ctx.user_id).await?;
        st.service.import_drive(db,&access,&input.agent_id,&input.file_id,input.selection_receipt.as_deref(),
            ||authorize_commit(&st,&ctx,&input.agent_id,AccessLevel::Operator)).await
    }.await;
    if result.is_ok(){audit.outcome="allowed";}
    private_result(result)
}
