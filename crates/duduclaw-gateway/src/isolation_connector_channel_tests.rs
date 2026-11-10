use super::*;
use std::sync::{Mutex,atomic::{AtomicBool,Ordering}};
use duduclaw_isolation::{bounded::{List,Text},capability::{CapabilityId,CapabilityRecord,EmployeeBinding,Lane,LiveRequestVerifier,Principal,ResourceGrant},ids::{RunId,Sha256Digest}};
use duduclaw_mcp_boundary::{bindings::BindingStore,tool_policy::{authorize,ResourceResolver,ToolPolicy}};
use crate::isolation_connectors::{ConnectorCatalog, ConnectorKind};
struct Live(Arc<AtomicBool>);
impl LiveRequestVerifier for Live {fn verify_live(&self,_:&CapabilityRecord,_:u64)->Result<(),VerificationError>{
    if self.0.load(Ordering::SeqCst){Ok(())}else{Err(VerificationError::Revoked)}}}
fn text<const N:usize>(s:&str)->Text<N>{Text::new(s.into()).unwrap()}
struct Resolver(Vec<ResourceGrant>);
impl ResourceResolver for Resolver {fn resolve(&self,_:&RequestContext,_:&ToolPolicy,_:&Value)->Result<Vec<ResourceGrant>,VerificationError>{Ok(self.0.clone())}}
struct Fixture {home:tempfile::TempDir,context:RequestContext,bindings:Arc<Mutex<BindingStore>>,catalog:Arc<ConnectorCatalog>,live:Arc<AtomicBool>}
impl Fixture {
    fn new()->Self {
        let home=tempfile::tempdir().unwrap();std::fs::create_dir_all(home.path().join("agents/alice")).unwrap();
        std::fs::write(home.path().join("config.toml"),"").unwrap();
        std::fs::write(home.path().join("agents/alice/agent.toml"),"[agent]\nstatus='active'\n[capabilities]\ncomputer_use=true\n").unwrap();
        let binding=EmployeeBinding::Personal(text("owner"));let principal=Principal::Human{user_id:text("owner"),generation:1};
        let employee=EmployeeId([2;16]);let live=Arc::new(AtomicBool::new(true));
        let context=RequestContext::with_live_verifier(CapabilityRecord {
            id:CapabilityId(Sha256Digest([1;32])),employee,run:RunId([3;16]),task:None,principal:principal.clone(),employee_binding:binding.clone(),lane:Lane::Standard,
            tools:List::new(CHANNEL_TOOLS.iter().chain(["model_search","model_recommend","computer_login"].iter()).map(|s|text(s)).collect()).unwrap(),
            resources:List::new(vec![ResourceGrant::EmployeeScope{id:employee},ResourceGrant::Workspace{handle:text("session-workspace")}]).unwrap(),
            policy_revision:1,gateway_epoch:[0;16],issued_at:1,not_before:1,expires_at:u64::MAX,nonce:[0;32],jti:[0;16],
        },Arc::new(Live(live.clone())));
        let mut bindings=BindingStore::new(rusqlite::Connection::open_in_memory().unwrap()).unwrap();bindings.register_principal(&principal).unwrap();bindings.create_employee(employee,&binding).unwrap();
        let catalog=Arc::new(ConnectorCatalog::new(rusqlite::Connection::open_in_memory().unwrap()).unwrap());
        Self{home,context,bindings:Arc::new(Mutex::new(bindings)),catalog,live}
    }
    fn account(&self,label:&str,provider:&str)->ProductConnectorAccount {
        let binding=&self.context.record().employee_binding;
        let id=self.catalog.register(label,provider,ConnectorKind::Personal,binding).unwrap();
        self.bindings.lock().unwrap().register_connector(id.as_str(),binding).unwrap();
        ProductConnectorAccount::Personal{account:id.as_str().into(),owner:"owner".into()}
    }
    fn call(&self,provider:Arc<dyn LiveConnectorSelectionPolicy>,tool:&str,args:Value)->(SelectedConnector,AuthorizedToolCall){
        let selected=SelectedConnector::select_with_policy(&self.context,tool,&args,self.bindings.clone(),self.catalog.clone(),Some(provider)).unwrap();
        let resources=vec![ResourceGrant::Employee{id:self.context.record().employee},ResourceGrant::Connector{account:text(selected.account()),binding:selected.binding().clone()},
            ResourceGrant::Workspace{handle:text("session-workspace")},ResourceGrant::Mailbox{id:text(&format!("account:{}",selected.account()))},ResourceGrant::Recipient{id:text("chat") }];
        let call=authorize(&self.context,tool,&args,&Resolver(resources)).unwrap();(selected,call)
    }
    fn channels(&self,scope:&str)->Arc<ChannelConnectorProvider>{
        let account=self.account("channel","channel");
        let route=ChannelAccount::new(account,"discord".into(),scope.into(),"discord:alice".into(),Some("TOKEN-CANARY".into()),None).unwrap();
        Arc::new(ChannelConnectorProvider::new(self.home.path().into(),"alice".into(),self.context.record().employee,vec![route]).unwrap())
    }
}
#[tokio::test]
async fn channel_config_exact_scope_and_pairing_list_do_not_disclose_other_subjects(){
    let f=Fixture::new();let provider=f.channels("chat");
    let mgr=crate::channel_settings::ChannelSettingsManager::new(&f.home.path().join("sessions.db")).unwrap();
    mgr.set("discord","other","guild_name","PRIVATE-OTHER").await.unwrap();
    for key in ["admin_users", "shared_bot_binding", "discord_agent_choices", "group_context", "future_private_key"] {
        mgr.set("discord","chat",key,"PRIVATE-SETTING").await.unwrap();
    }
    mgr.set("discord","chat","require_pairing","true").await.unwrap();
    let (selected,call)=f.call(provider.clone(),"channel_config",json!({"channel":"discord","scope_id":"chat","key":"mention_only","value":"true"}));
    assert!(provider.execute(selected,call).await.unwrap()["isError"].is_null());
    let (selected,call)=f.call(provider.clone(),"channel_config_list",json!({"channel":"discord","scope_id":"chat"}));
    let result=provider.execute(selected,call).await.unwrap().to_string();assert!(result.contains("mention_only"));assert!(!result.contains("PRIVATE-OTHER"));assert!(!result.contains("TOKEN-CANARY"));assert!(!result.contains("PRIVATE-SETTING"));assert!(result.contains("require_pairing"));
    let ctrl=crate::access_control::AccessController::with_persistence(f.home.path().join("access_control.json"));ctrl.approve_user("PRIVATE-OTHER").await;
    let (selected,call)=f.call(provider.clone(),"pairing_manage",json!({"action":"approve","subject":"chat"}));provider.execute(selected,call).await.unwrap();
    let (selected,call)=f.call(provider.clone(),"pairing_manage",json!({"action":"list"}));let result=provider.execute(selected,call).await.unwrap().to_string();assert!(result.contains("chat"));assert!(!result.contains("PRIVATE-OTHER"));
}
#[tokio::test]
async fn channel_rejects_original_invalid_settings_scope_and_account_replays_before_io(){
    let f=Fixture::new();let provider=f.channels("bad/scope");
    let (selected,call)=f.call(provider.clone(),"channel_config_list",json!({"channel":"discord","scope_id":"bad/scope"}));assert!(provider.execute(selected,call).await.is_err());
    assert!(SelectedConnector::select_with_policy(&f.context,"send_message",&json!({"channel":"discord","chat_id":"other","text":"x"}),f.bindings.clone(),f.catalog.clone(),Some(provider.clone())).is_err());
    let f=Fixture::new();let provider=f.channels("chat");let(selected,call)=f.call(provider.clone(),"send_message",json!({"channel":"discord","chat_id":"chat","text":"never send"}));
    f.live.store(false,Ordering::SeqCst);assert!(provider.execute(selected,call).await.is_err());
}
#[tokio::test]
async fn channel_status_is_bounded_to_selected_label_and_session_scope(){
    let f=Fixture::new();let provider=f.channels("chat");
    std::fs::write(f.home.path().join("channel_status.json"),json!({"channels":{"discord:alice":{"connected":true},"discord:bob":{"note":"PRIVATE-OTHER"}},"updated_at":"now"}).to_string()).unwrap();
    let db=rusqlite::Connection::open(f.home.path().join("sessions.db")).unwrap();db.execute_batch("CREATE TABLE sessions(id TEXT,last_active TEXT);INSERT INTO sessions VALUES('discord:chat','2026-01-01T00:00:00Z'),('discord:other','2026-01-01T00:00:00Z');").unwrap();drop(db);
    let mgr=crate::channel_settings::ChannelSettingsManager::new(&f.home.path().join("sessions.db")).unwrap();
    mgr.set("discord","chat","guild_name","Selected Guild").await.unwrap();
    mgr.set("discord","chat","mention_only","true").await.unwrap();
    for key in ["admin_users", "allowed_users", "blocked_users", "agent_override", "allowed_channels", "shared_bot_binding", "future_private_key"] {
        mgr.set("discord","chat",key,"PRIVATE-SETTING").await.unwrap();
    }
    let(selected,call)=f.call(provider.clone(),"channel_status",json!({"channel":"discord"}));let result=provider.execute(selected,call).await.unwrap();let text=result["content"][0]["text"].as_str().unwrap();
    assert!(!text.contains("PRIVATE-OTHER"));assert!(!text.contains("PRIVATE-SETTING"));assert!(text.contains("Selected Guild"));assert!(text.contains("mention_only"));let payload:Value=serde_json::from_str(text).unwrap();assert_eq!(payload["sessions"]["discord"]["total_sessions"],1);
}
#[test]
fn self_configuring_senders_cannot_substitute_home_credentials_for_personal_accounts(){
    for channel in ["googlechat","teams","wecom","dingtalk"] {
        assert!(ChannelAccount::new(ProductConnectorAccount::Personal{account:"personal".into(),owner:"owner".into()},channel.into(),"chat".into(),channel.into(),Some("TOKEN".into()),None).is_err());
        assert!(ChannelAccount::new(ProductConnectorAccount::SharedHome{account:"company".into(),tenant:"tenant".into()},channel.into(),"chat".into(),channel.into(),None,None).is_ok());
    }
}
#[tokio::test]
async fn bound_io_context_rechecks_product_and_binding_after_wait(){
    let f=Fixture::new();let provider=f.channels("chat");let(selected,_)=f.call(provider.clone(),"channel_config_list",json!({"channel":"discord","scope_id":"chat"}));
    let selected=Arc::new(selected);let context=selected.io_context();f.bindings.lock().unwrap().revoke_connector(selected.account()).unwrap();
    assert!(duduclaw_mcp_boundary::io_authority::scope(context,async{duduclaw_mcp_boundary::io_authority::verify_before_io()}).await.is_err());
}
#[tokio::test]
async fn model_recommend_uses_provided_gateway_hardware_and_revoked_search_never_sends(){
    use crate::isolation_connector_model::ModelConnectorProvider;
    let f=Fixture::new();let account=f.account("model","model");
    let hardware=duduclaw_inference::types::HardwareInfo{gpu_type:duduclaw_inference::types::GpuType::None,gpu_name:"gateway snapshot".into(),vram_total_mb:0,vram_available_mb:0,ram_total_mb:16000,ram_available_mb:8000,cpu_cores:4,recommended_backend:duduclaw_inference::types::BackendType::OpenAiCompat,recommended_max_model_gb:5.0};
    let provider=Arc::new(ModelConnectorProvider::new(f.home.path().into(),"alice".into(),f.context.record().employee,account,hardware).unwrap());
    let(selected,call)=f.call(provider.clone(),"model_recommend",json!({}));let out=provider.execute(selected,call).await.unwrap();assert!(out.to_string().contains("gateway snapshot"));
    let(selected,call)=f.call(provider.clone(),"model_search",json!({"query":"gguf"}));f.live.store(false,Ordering::SeqCst);assert!(provider.execute(selected,call).await.is_err());
}
#[tokio::test]
async fn computer_login_binds_exact_credential_and_live_grants_without_secret_in_call(){
    use crate::isolation_connector_computer::ComputerConnectorProvider;
    let f=Fixture::new();let account=f.account("computer","computer");
    let metadata=crate::website_credentials::upsert(f.home.path(),crate::website_credentials::Upsert{id:None,label:"work".into(),login_url:"https://example.com/login".into(),allowed_paths:vec![],username:"private-user".into(),password:"PASSWORD-CANARY".into(),allowed_agents:vec!["alice".into()]},"owner").unwrap();
    let sessions=crate::computer_use_sessions::ComputerUseSessions::new(f.home.path().into());
    let provider=Arc::new(ComputerConnectorProvider::new(f.home.path().into(),"alice".into(),f.context.record().employee,vec![(account,metadata.id.clone())],sessions).unwrap());
    assert!(SelectedConnector::select_with_policy(&f.context,"computer_login",&json!({"credential":"unbound"}),f.bindings.clone(),f.catalog.clone(),Some(provider.clone())).is_err());
    let(selected,call)=f.call(provider.clone(),"computer_login",json!({"credential":"work"}));assert!(!call.arguments().to_string().contains("PASSWORD-CANARY"));
    crate::website_credentials::delete(f.home.path(),&metadata.id).unwrap();assert!(provider.execute(selected,call).await.is_err());
}

#[test]
fn channel_tokens_preserve_line_base64_and_keep_telegram_url_path_closed(){
    let account=ProductConnectorAccount::Personal{account:"bot".into(),owner:"owner".into()};
    assert!(ChannelAccount::new(account.clone(),"line".into(),"chat".into(),"line:alice".into(),Some("AbC+/123==".into()),None).is_ok());
    assert!(ChannelAccount::new(account.clone(),"telegram".into(),"chat".into(),"telegram:alice".into(),Some("123456:AbC_-".into()),None).is_ok());
    for bad in ["123/token","123?query","123\r\nheader"] {
        assert!(ChannelAccount::new(account.clone(),"telegram".into(),"chat".into(),"telegram:alice".into(),Some(bad.into()),None).is_err());
    }
}

#[test]
fn home_sender_factory_never_substitutes_an_ambient_home(){
    // Source boundary regression: the generic factory must honor its explicit
    // registered home for every subtype (Google Chat/Teams previously did not).
    // include_str! keeps the checkout's line endings (CRLF on Windows autocrlf).
    let source=include_str!("channel_sender.rs").replace("\r\n","\n");
    let factory=source.split("pub fn create_sender_for_home(").nth(1).unwrap().split("///").next().unwrap();
    assert!(!factory.contains("platform::duduclaw_home()"));
    assert!(factory.contains("create_googlechat_sender(\n            home_dir.to_path_buf()"));
    assert!(factory.contains("create_teams_sender(\n            home_dir.to_path_buf()"));
}

#[tokio::test]
async fn channel_audience_refuses_unbound_recipient() {
    let f = Fixture::new();
    let provider = f.channels("123456789012345678");
    for tool in ["send_message", "send_photo", "send_sticker"] {
        for recipient in ["987654321098765432", "123456789012345678.evil", "*", ""] {
            let args = json!({"channel":"discord","chat_id":recipient,"text":"never send",
                "url":"https://example.invalid/photo","sticker_id":"sticker"});
            assert!(SelectedConnector::select_with_policy(&f.context, tool, &args,
                f.bindings.clone(), f.catalog.clone(), Some(provider.clone())).is_err());
            // Bypass selection filtering with a fixture policy to exercise the
            // executor's independent audience check, with live authority intact.
            struct Unfiltered(Arc<ChannelConnectorProvider>);
            impl LiveConnectorSelectionPolicy for Unfiltered {
                fn supports(&self, tool: &str) -> bool { CHANNEL_TOOLS.contains(&tool) }
                fn candidates(&self, context: &RequestContext, tool: &str, provider: &str)
                    -> Result<Vec<ProductConnectorAccount>, VerificationError> {
                    self.0.candidates(context, tool, provider)
                }
            }
            let (selected, call) = f.call(Arc::new(Unfiltered(provider.clone())), tool, args);
            assert!(matches!(provider.execute(selected, call).await, Err(VerificationError::BindingMismatch)));
        }
    }
}

#[tokio::test]
async fn computer_login_scoped_to_employee_credentials() {
    use crate::isolation_connector_computer::ComputerConnectorProvider;
    let f = Fixture::new();
    let account = f.account("computer", "computer");
    let store = |label: &str, agent: &str| crate::website_credentials::upsert(f.home.path(),
        crate::website_credentials::Upsert { id:None, label:label.into(),
            login_url:"https://example.invalid/login".into(), allowed_paths:vec![],
            username:format!("{agent}-private-user"), password:format!("{agent}-PASSWORD-CANARY"),
            allowed_agents:vec![agent.into()] }, "owner").unwrap();
    let own = store("alice-login", "alice");
    let other = store("bob-login", "bob");
    let sessions = crate::computer_use_sessions::ComputerUseSessions::new(f.home.path().into());
    let account_id = account.account().to_string();
    // Even trusted boot input cannot attach Bob's credential to Alice.
    assert!(matches!(ComputerConnectorProvider::new(f.home.path().into(), "alice".into(),
        f.context.record().employee, vec![(account.clone(), other.id.clone())], sessions.clone()),
        Err(VerificationError::BindingMismatch)));
    let provider = Arc::new(ComputerConnectorProvider::new(f.home.path().into(), "alice".into(),
        f.context.record().employee, vec![(account, own.id.clone())], sessions).unwrap());
    let (selected, call) = f.call(provider.clone(), "computer_login", json!({"credential":"alice-login"}));
    assert_eq!(selected.account(), account_id);
    assert!(!call.arguments().to_string().contains("PASSWORD-CANARY"));
    for credential in ["bob-login", other.id.as_str(), "alice-login/bob-login"] {
        assert!(SelectedConnector::select_with_policy(&f.context, "computer_login",
            &json!({"credential":credential,"connector_account":account_id.as_str()}),
            f.bindings.clone(), f.catalog.clone(), Some(provider.clone())).is_err());
    }
    // Removing the employee grant after selection fences the use-time path
    // before a browser session or decrypted credential is requested.
    crate::website_credentials::upsert(f.home.path(), crate::website_credentials::Upsert {
        id:Some(own.id), label:"alice-login".into(), login_url:"https://example.invalid/login".into(),
        allowed_paths:vec![], username:"alice-private-user".into(), password:"alice-PASSWORD-CANARY".into(),
        allowed_agents:vec!["bob".into()] }, "owner").unwrap();
    assert!(provider.execute(selected, call).await.is_err());
}
