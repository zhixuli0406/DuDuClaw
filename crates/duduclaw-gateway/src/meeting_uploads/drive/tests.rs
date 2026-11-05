use super::*;
use axum::{Router,body::Body,extract::{State,OriginalUri},http::{HeaderMap,StatusCode},response::{Response,IntoResponse},routing::get};
use crate::{google_personal::meeting_access,person_credentials::{self as vault,Metadata,Secret,Status}};
use std::{collections::VecDeque,sync::Mutex};
use tower::ServiceExt;

const FILE_ID:&str="recording_123456";
fn file(size:u64)->Value{json!({"id":FILE_ID,"name":"會議.wav","mimeType":"audio/wav","size":size.to_string(),
    "trashed":false,"isAppAuthorized":true,"capabilities":{"canDownload":true},"version":"7"})}
#[test]
fn drive_ids_reject_urls_paths_and_unicode(){
    for id in [FILE_ID,"aB-0123456789_"]{assert!(valid_file_id(id));}
    for id in ["","short","../recording_123","https://drive.google.com/file/d/123","recording%2F123456","recording/123456","錄影0123456789","recording_123\n"]{assert!(!valid_file_id(id),"{id}");}
    assert!(!valid_file_id(&"a".repeat(201)));
}
#[test]
fn drive_metadata_enforces_size_type_and_app_authorization(){
    let parse=|v:Value|serde_json::from_value::<DriveFile>(v).unwrap();
    assert_eq!(parse(file(64)).media(64).unwrap().size,64);
    assert!(parse(file(65)).media(64).is_err());
    assert!(parse(file(0)).media(64).is_err());
    for (key,value) in [("size",json!(null)),("size",json!("18446744073709551616")),("mimeType",json!("text/audio/wav")),("isAppAuthorized",json!(false)),("trashed",json!(true)),("capabilities",json!({"canDownload":false}))]{
        let mut v=file(12);v[key]=value;assert!(parse(v).media(64).is_err(),"{key}");
    }
    assert!(!media_mime("audio/"));assert!(!media_mime("audio/wav\n"));
    assert!(page_token(&"a".repeat(2049)).is_err());
}

struct Fixture{_home:tempfile::TempDir,service:Arc<Service>,db:Arc<UserDb>,user:String,access:MeetingAccess}
async fn fixture()->Fixture{
    let home=tempfile::tempdir().unwrap();let path=home.path().canonicalize().unwrap();
    crate::team_edition::record(&path,duduclaw_core::EditionProfile::Enterprise);
    duduclaw_core::gateway_instance::acquire(&path).unwrap();
    crate::handlers::credential_protection_rpc::protection_for(&path).enable(false).unwrap();
    std::fs::create_dir_all(path.join("agents/alice/attachments")).unwrap();
    std::fs::write(path.join("agents/alice/agent.toml"),"[agent]\nname='alice'\nstatus='active'\n[capabilities]\nallowed_tools=['mcp__duduclaw__*']\n").unwrap();
    std::fs::write(path.join("config.toml"),"[integrations]\ngoogle_workspace=true\n[security]\negress_guard='off'\n").unwrap();
    std::fs::write(path.join(duduclaw_core::autonomy_gate::INIT_FILE),b"").unwrap();
    std::fs::write(path.join(duduclaw_core::autonomy_gate::STATE_FILE),r#"{"version":1,"global":null,"agents":{}}"#).unwrap();
    let db=Arc::new(UserDb::new(&path.join("users.db")).unwrap());
    let user=db.create_user("person@test.invalid","Person","fixture-password-123",duduclaw_auth::UserRole::Employee).unwrap();
    db.bind_agent(&user.id,"alice",duduclaw_auth::AccessLevel::Operator).unwrap();
    crate::mcp_oauth::upsert_client_config(&path,crate::mcp_oauth::McpOAuthClientConfig{
        provider_id:"google".into(),client_id:"meeting-test-client".into(),client_secret:"fixture-secret".into(),
        auth_url:crate::google_personal::oauth::AUTHORIZE.into(),token_url:crate::google_personal::oauth::TOKEN.into(),scopes:vec![],redirect_uri:"http://127.0.0.1/callback".into(),
    }).unwrap();
    let metadata=Metadata{owner_kind:"user".into(),owner_id:user.id.clone(),provider:"google".into(),credential_id:uuid::Uuid::new_v4().to_string(),revision:1,
        account_email:"person-google@test.invalid".into(),google_sub:"google-meeting-test".into(),hosted_domain:None,
        granted_scopes:vec![meeting_access::DRIVE_SCOPE.into(),meeting_access::MEET_SCOPE.into()],local_purposes:vec!["meeting_recordings".into()],status:Status::Connected,status_before_suspension:None,
        connected_at:chrono::Utc::now().to_rfc3339(),last_refresh_at:None,oauth_client_id:"meeting-test-client".into(),
        google_data_consent:Some(crate::google_data_consent::Evidence::issue(&user.id).unwrap())};
    let epoch=vault::begin_connection(&path,&user.id).unwrap();let key=crate::google_identity::protection_gate(&path).unwrap();
    vault::connect(&path,&key,metadata,&Secret{access_token:"meeting-fixture-token".into(),refresh_token:Some("fixture-refresh".into()),expires_at:Some(4_000_000_000)},epoch).unwrap();
    let access=meeting_access::resolve(&path,&db,&user.id).await.unwrap();
    let service=Service::open(crate::meeting_jobs::Manager::open(path).unwrap()).unwrap();
    Fixture{_home:home,service,db,user:user.id,access}
}
struct Script{responses:Mutex<VecDeque<Response>>,requests:Mutex<Vec<String>>,media_received:tokio::sync::Notify}
struct Mock{script:Arc<Script>,_guard:meeting_access::TestEndpointGuard,task:tokio::task::JoinHandle<()>}
impl Drop for Mock{fn drop(&mut self){self.task.abort();}}
impl Mock{
    async fn new(f:&Fixture,responses:Vec<Response>)->Self{
        let script=Arc::new(Script{responses:Mutex::new(responses.into()),requests:Mutex::new(Vec::new()),media_received:tokio::sync::Notify::new()});
        async fn serve(State(s):State<Arc<Script>>,OriginalUri(uri):OriginalUri,headers:HeaderMap)->Response{
            assert_eq!(headers.get("authorization").unwrap(),"Bearer meeting-fixture-token");
            s.requests.lock().unwrap().push(uri.to_string());
            if uri.query().is_some_and(|q|q.split('&').any(|p|p=="alt=media")){s.media_received.notify_one();}
            s.responses.lock().unwrap().pop_front().unwrap_or_else(||StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base=format!("http://{}",listener.local_addr().unwrap());
        let router=Router::new().route("/{*path}",get(serve)).with_state(script.clone());
        let task=tokio::spawn(async move{axum::serve(listener,router).await.unwrap();});
        let guard=meeting_access::install_test_endpoint(&f.service.manager.home,&base);
        Self{script,_guard:guard,task}
    }
    fn requests(&self)->Vec<String>{self.script.requests.lock().unwrap().clone()}
}
fn json(value:Value)->Response{axum::Json(value).into_response()}
fn media()->Response{Body::from("RIFF0000WAVE").into_response()}
fn selection(f:&Fixture)->String{super::super::selection::issue(&f.service.manager.home,&f.user,"alice",FILE_ID).unwrap()}
fn reservations(f:&Fixture)->u64{
    rusqlite::Connection::open(f.service.manager.home.join("meeting_jobs.db")).unwrap().query_row("SELECT COALESCE(SUM(reserved),0) FROM meeting_uploads",[],|r|r.get(0)).unwrap()
}
fn dashboard_app(f:&Fixture)->Router{
    crate::trigger_route::tests::write_agent(&f.service.manager.home.join("agents"),"alice","Alice","","specialist","\n[capabilities]\nallowed_tools=['mcp__duduclaw__*']\n");
    let ctx=f.db.live_user_context(&f.user).unwrap().unwrap();
    super::super::router_with_google(f.service.clone(),Arc::new(move|headers|{
        if headers.get("authorization").and_then(|v|v.to_str().ok())!=Some("Bearer dashboard-fixture-session"){
            return Err(StatusCode::UNAUTHORIZED.into_response());
        }
        Ok(ctx.clone())
    }),f.db.clone())
}
fn dashboard_request(method:&str,path:&str,body:Body)->axum::http::Request<Body>{
    axum::http::Request::builder().method(method).uri(path)
        .header("authorization","Bearer dashboard-fixture-session")
        .header("content-type","application/json").body(body).unwrap()
}
async fn response_json(response:Response)->Value{
    serde_json::from_slice(&axum::body::to_bytes(response.into_body(),64*1024).await.unwrap()).unwrap()
}
#[tokio::test]
async fn dashboard_drive_and_meet_lists_audit_success_without_private_data(){
    let f=fixture().await;let app=dashboard_app(&f);
    let mock=Mock::new(&f,vec![json(json!({"files":[file(12)]})),
        json(json!({"conferenceRecords":[{"name":"conferenceRecords/private-conference"}]})),
        json(json!({"recordings":[{"state":"FILE_GENERATED","startTime":"2026-11-01T00:00:00Z","driveDestination":{"file":FILE_ID}}]}))]).await;
    let response=app.clone().oneshot(dashboard_request("GET","/api/meeting-drive/files?agent=alice&search=private-search-value",Body::empty())).await.unwrap();
    assert_eq!(response.status(),StatusCode::OK);
    assert_eq!(response.headers().get("cache-control").unwrap(),"no-store");
    assert_eq!(response_json(response).await["files"][0]["id"],FILE_ID);
    let response=app.oneshot(dashboard_request("GET","/api/meeting-drive/recordings?agent=alice",Body::empty())).await.unwrap();
    assert_eq!(response.status(),StatusCode::OK);
    assert_eq!(response_json(response).await["recordings"][0]["file_id"],FILE_ID);
    assert_eq!(mock.requests().len(),3);
    let audit=std::fs::read_to_string(f.service.manager.home.join("security_audit.jsonl")).unwrap();
    let events:Vec<Value>=audit.lines().map(|line|serde_json::from_str(line).unwrap()).collect();
    for event in ["meeting_drive_list","meeting_meet_list"]{
        let rows:Vec<_>=events.iter().filter(|row|row["event_type"]==event).collect();
        assert_eq!(rows.len(),1,"one successful audit per list request");
        assert_eq!(rows[0]["details"]["outcome"],"allowed");
        assert_eq!(rows[0]["details"]["employee"],"alice");
    }
    for private in ["private-search-value","會議.wav",FILE_ID,"private-conference","meeting-fixture-token","fixture-refresh","dashboard-fixture-session"]{
        assert!(!audit.contains(private),"audit leaked {private}");
    }
}
#[tokio::test]
async fn dashboard_confirmed_file_receipt_allows_one_http_import_then_refuses_replay(){
    let f=fixture().await;let app=dashboard_app(&f);
    let mock=Mock::new(&f,vec![json(file(12)),media(),json(file(12))]).await;
    let selection=json!({"agent_id":"alice","file_id":FILE_ID,"confirm":true});
    let response=app.clone().oneshot(dashboard_request("POST","/api/meeting-drive/selection",Body::from(selection.to_string()))).await.unwrap();
    assert_eq!(response.status(),StatusCode::OK);
    assert_eq!(response.headers().get("cache-control").unwrap(),"no-store");
    let selected=response_json(response).await;
    assert_eq!(selected["expires_in"],300);
    let receipt=selected["selection_receipt"].as_str().unwrap();
    assert_eq!(receipt.len(),64);
    assert!(mock.requests().is_empty(),"registering a confirmed file ID does not fetch Google; import checks app authorization");
    let import=json!({"agent_id":"alice","file_id":FILE_ID,"selection_receipt":receipt,"confirm":true});
    let response=app.clone().oneshot(dashboard_request("POST","/api/meeting-drive/import",Body::from(import.to_string()))).await.unwrap();
    assert_eq!(response.status(),StatusCode::OK);
    assert_eq!(response_json(response).await["state"],"queued");
    let response=app.oneshot(dashboard_request("POST","/api/meeting-drive/import",Body::from(import.to_string()))).await.unwrap();
    assert_eq!(response.status(),StatusCode::BAD_REQUEST);
    assert_eq!(mock.requests().len(),3,"replay cannot issue another Google request");
    assert_eq!(f.service.manager.list("alice").unwrap().len(),1);
    let audit=std::fs::read_to_string(f.service.manager.home.join("security_audit.jsonl")).unwrap();
    assert!(!audit.contains(receipt) && !audit.contains(FILE_ID) && !audit.contains("meeting-fixture-token"));
}
#[tokio::test]
async fn import_receipt_rejections_never_reach_google_and_are_audited(){
    let f=fixture().await;let mock=Mock::new(&f,vec![]).await;
    let home=&f.service.manager.home;
    assert!(f.service.import_drive(&f.db,&f.access,"alice",FILE_ID,None,||Ok(())).await.is_err());
    for (owner,agent,file) in [("someone-else","alice",FILE_ID),(f.user.as_str(),"bob",FILE_ID),(f.user.as_str(),"alice","other_file_123456")]{
        let receipt=super::super::selection::issue(home,owner,agent,file).unwrap();
        assert!(f.service.import_drive(&f.db,&f.access,"alice",FILE_ID,Some(&receipt),||Ok(())).await.is_err());
    }
    let expired=selection(&f);
    rusqlite::Connection::open(home.join("meeting_jobs.db")).unwrap()
        .execute("UPDATE meeting_drive_selections SET expires=0",[]).unwrap();
    assert!(f.service.import_drive(&f.db,&f.access,"alice",FILE_ID,Some(&expired),||Ok(())).await.is_err());
    let replayed=selection(&f);
    super::super::selection::consume(home,&f.user,"alice",FILE_ID,Some(&replayed)).unwrap();
    assert!(f.service.import_drive(&f.db,&f.access,"alice",FILE_ID,Some(&replayed),||Ok(())).await.is_err());
    assert!(mock.requests().is_empty());assert_eq!(reservations(&f),0);
    let audit=std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap();
    assert!(audit.contains("meeting_drive_import") && audit.contains("import_refused"));
    assert!(!audit.contains(FILE_ID) && !audit.contains(&replayed) && !audit.contains("meeting-fixture-token"));
}
#[tokio::test]
async fn mocked_drive_import_reuses_upload_snapshot_and_records_owner_provenance(){
    let f=fixture().await;let mock=Mock::new(&f,vec![json(file(12)),media(),json(file(12))]).await;
    let job=f.service.import_drive(&f.db,&f.access,"alice",FILE_ID,Some(&selection(&f)),||Ok(())).await.unwrap();
    assert_eq!(job["state"],"queued");
    let job=f.service.manager.get("alice",job["job_id"].as_str().unwrap()).unwrap();
    let upload=job.source_upload_id.unwrap();
    let db=rusqlite::Connection::open(f.service.manager.home.join("meeting_jobs.db")).unwrap();
    let source:(String,String,String,String)=db.query_row("SELECT owner,google_account,drive_file_id,drive_version FROM meeting_drive_sources WHERE upload_id=?1",[upload],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
    assert_eq!(source,(f.user.clone(),"person-google@test.invalid".into(),FILE_ID.into(),"7".into()));
    assert_eq!(std::fs::read(&job.snapshot_path).unwrap(),b"RIFF0000WAVE");
    assert_eq!(reservations(&f),0);
    assert_eq!(mock.requests().len(),3);
    let audit=std::fs::read_to_string(f.service.manager.home.join("security_audit.jsonl")).unwrap();
    assert!(audit.contains("meeting_drive_import"));assert!(!audit.contains(FILE_ID));assert!(!audit.contains("meeting-fixture-token"));
}
#[tokio::test]
async fn mocked_drive_size_and_version_changes_refuse_without_queued_job(){
    for changed in [false,true]{
        let f=fixture().await;
        let responses=if changed{let mut after=file(12);after["version"]=json!("8");vec![json(file(12)),media(),json(after)]}
            else{vec![json(file(12)),Body::from("RIFF0000WAVE-too-large").into_response()]};
        let _mock=Mock::new(&f,responses).await;
        assert!(f.service.import_drive(&f.db,&f.access,"alice",FILE_ID,Some(&selection(&f)),||Ok(())).await.is_err());
        assert!(f.service.manager.list("alice").unwrap().is_empty());assert_eq!(reservations(&f),0);
    }
}
#[tokio::test]
async fn mocked_drive_and_meet_lists_are_bounded_and_never_follow_export_urls(){
    let f=fixture().await;
    let mut unauthorized=file(12);unauthorized["id"]=json!("not_app_authorized_123456");unauthorized["isAppAuthorized"]=json!(false);
    let _mock=Mock::new(&f,vec![json(json!({"files":[file(12),unauthorized],"nextPageToken":"next"})),
        json(json!({"conferenceRecords":[{"name":"conferenceRecords/abc-123"}],"nextPageToken":"meet-next"})),
        json(json!({"recordings":[{"state":"FILE_GENERATED","startTime":"2026-11-01T00:00:00Z","driveDestination":{"file":FILE_ID,"exportUri":"http://127.0.0.1/private"}}],"nextPageToken":"more-recordings"}))]).await;
    let files=files(&f.service.manager.home,&f.db,&f.access,"Bob's \\ recording","",64).await.unwrap();
    assert_eq!(files["files"].as_array().unwrap().len(),1);assert_eq!(files["next_page_token"],"next");
    let rows=recordings(&f.service.manager.home,&f.db,&f.access,"").await.unwrap();
    assert_eq!(rows["recordings"][0]["file_id"],FILE_ID);assert_eq!(rows["truncated"],true);
    assert!(!rows.to_string().contains("exportUri"));
    let requests=_mock.requests();assert_eq!(requests.len(),3);
    assert!(requests[2].starts_with("/meet.googleapis.com/v2/conferenceRecords/abc-123/recordings?"));
    let url=reqwest::Url::parse(&format!("http://localhost{}",requests[0])).unwrap();
    let q=url.query_pairs().find(|(k,_)|k=="q").unwrap().1.into_owned();
    assert!(q.ends_with("name contains 'Bob\\'s \\\\ recording'"));
}
#[tokio::test]
async fn mocked_google_redirect_is_refused_without_followup(){
    let f=fixture().await;
    let redirect=Response::builder().status(StatusCode::FOUND).header("Location","https://evil.example/private").body(Body::empty()).unwrap();
    let mock=Mock::new(&f,vec![redirect]).await;
    assert!(files(&f.service.manager.home,&f.db,&f.access,"","",64).await.is_err());
    assert_eq!(mock.requests().len(),1);
}
#[tokio::test]
async fn revoked_connection_during_media_read_cannot_commit_a_chunk(){
    let f=fixture().await;let _mock=Mock::new(&f,vec![media()]).await;
    let receipt=f.service.create_for(&f.user,"alice",12).await.unwrap();let id=receipt["upload_id"].as_str().unwrap();
    let response=f.access.get(&f.service.manager.home,&f.db,format!("{DRIVE_FILES}/{FILE_ID}?alt=media").parse().unwrap()).await.unwrap();
    let calls=std::cell::Cell::new(0);
    let result=receive_media(&f.service,&f.user,id,12,response,||{
        calls.set(calls.get()+1);
        if calls.get()==2{vault::disconnect(&f.service.manager.home,&f.user).unwrap();}
        f.access.validate(&f.service.manager.home,&f.db)
    }).await;
    assert!(result.is_err());assert_eq!(f.service.status_for(&f.user,id).unwrap()["received"],json!([]));
}
#[tokio::test]
async fn dropping_an_import_releases_partial_upload_reservation(){
    let f=fixture().await;
    let stalled=Body::from_stream(futures_util::stream::pending::<Result<Bytes,std::io::Error>>()).into_response();
    let mock=Mock::new(&f,vec![json(file(12)),stalled]).await;
    let receipt=selection(&f);
    let mut import=Box::pin(f.service.import_drive(&f.db,&f.access,"alice",FILE_ID,Some(&receipt),||Ok(())));
    tokio::select!{
        result=&mut import=>panic!("stalled import unexpectedly completed: {result:?}"),
        reached=tokio::time::timeout(Duration::from_secs(5),mock.script.media_received.notified())=>reached.unwrap(),
    }
    assert!(reservations(&f)>0,"cancel only after the source reservation exists");
    drop(import);
    tokio::time::timeout(Duration::from_secs(2),async{while reservations(&f)!=0{tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
    assert!(f.service.manager.list("alice").unwrap().is_empty());
}

#[tokio::test]
async fn chunked_media_overrun_and_truncation_release_reservations(){
    for content in [b"RIFF0000WAVE-overrun".as_slice(),b"RIFF0000".as_slice()]{
        let f=fixture().await;
        let data=Bytes::copy_from_slice(content);
        let response=Body::from_stream(futures_util::stream::once(async move{Ok::<_,std::io::Error>(data)})).into_response();
        let _mock=Mock::new(&f,vec![json(file(12)),response]).await;
        assert!(f.service.import_drive(&f.db,&f.access,"alice",FILE_ID,Some(&selection(&f)),||Ok(())).await.is_err());
        assert_eq!(reservations(&f),0);assert!(f.service.manager.list("alice").unwrap().is_empty());
    }
}
#[tokio::test]
async fn approval_gate_stops_drive_fetch_before_any_google_request(){
    let f=fixture().await;let mock=Mock::new(&f,vec![]).await;
    std::fs::write(f.service.manager.home.join("agents/alice/agent.toml"),"[agent]\nname='alice'\nstatus='active'\n[capabilities]\nallowed_tools=['mcp__duduclaw__*']\napproval_required_tools=['meeting_transcribe']\n").unwrap();
    assert!(f.service.import_drive(&f.db,&f.access,"alice",FILE_ID,Some(&selection(&f)),||Ok(())).await.is_err());
    assert!(mock.requests().is_empty());assert_eq!(reservations(&f),0);
}

#[tokio::test]
async fn dashboard_confirmation_requires_explicit_true_and_rate_limits_attempts_before_google() {
    let f=fixture().await;let app=dashboard_app(&f);
    let mock=Mock::new(&f,vec![]).await;
    for confirm in [None,Some(json!(false)),Some(json!("true"))] {
        let mut body=json!({"agent_id":"alice","file_id":FILE_ID});
        if let Some(confirm)=confirm {body["confirm"]=confirm;}
        let response=app.clone().oneshot(dashboard_request("POST","/api/meeting-drive/selection",Body::from(body.to_string()))).await.unwrap();
        assert_eq!(response.status(),StatusCode::BAD_REQUEST);
    }
    // Even malformed authenticated attempts consume the per-user budget.
    for _ in 3..20 {
        let response=app.clone().oneshot(dashboard_request("POST","/api/meeting-drive/selection",Body::from("{}"))).await.unwrap();
        assert_eq!(response.status(),StatusCode::BAD_REQUEST);
    }
    let body=json!({"agent_id":"alice","file_id":FILE_ID,"confirm":true});
    let response=app.oneshot(dashboard_request("POST","/api/meeting-drive/selection",Body::from(body.to_string()))).await.unwrap();
    assert_eq!(response.status(),StatusCode::TOO_MANY_REQUESTS);
    assert!(mock.requests().is_empty());
}

#[tokio::test]
async fn selection_route_rejects_employee_tokens_and_caller_supplied_owners() {
    let f=fixture().await;let app=dashboard_app(&f);
    let mock=Mock::new(&f,vec![]).await;
    let body=json!({"agent_id":"alice","file_id":FILE_ID,"confirm":true});
    let mut request=dashboard_request("POST","/api/meeting-drive/selection",Body::from(body.to_string()));
    request.headers_mut().insert("authorization","Bearer employee-mcp-token".parse().unwrap());
    let response=app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(),StatusCode::UNAUTHORIZED);
    let mut body=body;body["owner_id"]=json!(f.user);
    let response=app.oneshot(dashboard_request("POST","/api/meeting-drive/selection",Body::from(body.to_string()))).await.unwrap();
    assert_eq!(response.status(),StatusCode::BAD_REQUEST);
    assert!(mock.requests().is_empty());
}
