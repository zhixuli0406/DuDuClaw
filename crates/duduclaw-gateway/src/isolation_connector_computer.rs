//! Label-only website-login connector. The gateway's credential store and
//! ComputerUseSessions retain all secret acquisition and origin/session gates.
use std::{collections::BTreeMap,future::Future,path::PathBuf,pin::Pin,sync::Arc};
use duduclaw_isolation::ids::EmployeeId;
use duduclaw_mcp_boundary::{RequestContext,VerificationError,tool_policy::AuthorizedToolCall};
use serde_json::{Value,json};
use crate::isolation_connectors::{BoundConnectorExecutor,LiveConnectorSelectionPolicy,ProductConnectorAccount,SelectedConnector};

pub const COMPUTER_CONNECTOR_TOOLS: &[&str] = &["computer_login"];
pub struct ComputerConnectorProvider {
    home: PathBuf,agent: String,employee: EmployeeId,
    accounts: BTreeMap<String,(ProductConnectorAccount,crate::website_credentials::Metadata)>,
    sessions: Arc<crate::computer_use_sessions::ComputerUseSessions>,
}
impl ComputerConnectorProvider {
    /// Account-to-credential-id bindings are gateway boot input, never tool
    /// arguments. The browser receives only a label; secrets stay out of the run.
    pub fn new(home: PathBuf,agent: String,employee: EmployeeId,
        accounts: Vec<(ProductConnectorAccount,String)>,sessions: Arc<crate::computer_use_sessions::ComputerUseSessions>)
        -> Result<Self,VerificationError> {
        if !home.is_absolute() || !duduclaw_core::is_valid_agent_id(&agent) || accounts.is_empty() || accounts.len()>128 {
            return Err(VerificationError::Invalid);
        }
        let metadata = crate::website_credentials::list(&home).map_err(|_|VerificationError::Unavailable)?;
        let mut registry = BTreeMap::new();
        let mut ids = std::collections::BTreeSet::new();
        for (account,id) in accounts {
            let mut matches = metadata.iter().filter(|m|m.id==id && m.allowed_agents.iter().any(|a|a==&agent));
            let record = matches.next().ok_or(VerificationError::BindingMismatch)?.clone();
            if matches.next().is_some() || !ids.insert(id) || registry.insert(account.account().to_owned(),(account,record)).is_some() {
                return Err(VerificationError::Invalid);
            }
        }
        Ok(Self {home,agent,employee,accounts:registry,sessions})
    }
    fn live(&self,metadata:&crate::website_credentials::Metadata)->Result<(),VerificationError> {
        let matches:Vec<_>=crate::website_credentials::list(&self.home).map_err(|_|VerificationError::Unavailable)?.into_iter()
            .filter(|m|m.id==metadata.id || m.label==metadata.label).collect();
        if matches.as_slice()!=[metadata.clone()] || !metadata.allowed_agents.iter().any(|a|a==&self.agent) {
            return Err(VerificationError::BindingMismatch);
        }
        let dir=crate::ephemeral::resolve_agent_dir(&self.home,&self.agent).or_else(||(!crate::ephemeral::is_ephemeral_id(&self.agent))
            .then(||self.home.join("agents").join(&self.agent))).ok_or(VerificationError::Unavailable)?;
        let raw=duduclaw_core::agent_toml::read_effective_text(&dir).map_err(|_|VerificationError::Unavailable)?;
        let config:toml::Value=toml::from_str(&raw).map_err(|_|VerificationError::Invalid)?;
        if config.get("capabilities").and_then(|c|c.get("computer_use")).and_then(toml::Value::as_bool)!=Some(true)
            || config.get("capabilities").and_then(|c|c.get("denied_tools")).and_then(toml::Value::as_array)
                .is_some_and(|v|v.iter().any(|v|v.as_str()==Some("computer_login")))
            || config.get("agent").and_then(|a|a.get("status")).and_then(toml::Value::as_str)
                .is_some_and(|s|s!="active") {return Err(VerificationError::Unavailable);}
        Ok(())
    }
    async fn run(&self,selected:SelectedConnector,call:AuthorizedToolCall)->Result<Value,VerificationError> {
        selected.verify_call(&call)?;
        let (_,metadata)=self.accounts.get(selected.account()).ok_or(VerificationError::BindingMismatch)?;
        if call.arguments().get("credential").and_then(Value::as_str)!=Some(metadata.label.as_str()) {return Err(VerificationError::BindingMismatch);}
        // No run-provided password, username, turn identity or alternate secret.
        if call.arguments().as_object().is_none_or(|a|a.keys().any(|k|!matches!(k.as_str(),"credential"|"session_id"|"connector_account"))) {
            return Err(VerificationError::Invalid);
        }
        self.live(metadata)?;
        let selected=Arc::new(selected);
        let session=call.arguments().get("session_id").map(|v|v.as_str().ok_or(VerificationError::Invalid)).transpose()?;
        let request=crate::computer_use_sessions::actions::ActionRequest::Login {credential:metadata.label.clone()};
        selected.recheck()?;
        let result=duduclaw_mcp_boundary::io_authority::scope(selected.io_context(),
            self.sessions.action(&self.agent,session,None,&request)).await;
        let result=match result {
            Ok(result) if selected.recheck().is_ok()=>result,
            _=>return Ok(json!({"isError":true,"content":[{"type":"text","text":
                "無法確認網站登入結果，請先檢查工作階段，勿直接重試。"}]})),
        };
        let status=result.get("status").and_then(Value::as_str).unwrap_or("failed_uncleared");
        let message=result.get("message").and_then(Value::as_str).unwrap_or("無法確認登入結果，請檢查工作階段。");
        Ok(json!({"isError":!matches!(status,"logged_in"|"needs_takeover"),"content":[{"type":"text","text":format!("{status}：{message}")}]}))
    }
}
impl LiveConnectorSelectionPolicy for ComputerConnectorProvider {
    fn supports(&self,tool:&str)->bool {tool=="computer_login"}
    fn candidates(&self,context:&RequestContext,tool:&str,provider:&str)->Result<Vec<ProductConnectorAccount>,VerificationError> {
        context.verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)?;
        if context.record().employee!=self.employee || tool!="computer_login" || provider!="computer" {return Err(VerificationError::Unavailable);}
        Ok(self.accounts.values().filter(|(_,m)|self.live(m).is_ok()).map(|(a,_)|a.clone()).collect())
    }
    fn candidates_for_call(&self,context:&RequestContext,tool:&str,provider:&str,args:&Value)->Result<Vec<ProductConnectorAccount>,VerificationError> {
        let label=args.get("credential").and_then(Value::as_str).ok_or(VerificationError::Invalid)?;
        Ok(self.candidates(context,tool,provider)?.into_iter().filter(|a|self.accounts.get(a.account()).is_some_and(|(_,m)|m.label==label)).collect())
    }
}
impl BoundConnectorExecutor for ComputerConnectorProvider {
    fn supports(&self,tool:&str)->bool {tool=="computer_login"}
    fn execute<'a>(&'a self,selected:SelectedConnector,call:AuthorizedToolCall)->Pin<Box<dyn Future<Output=Result<Value,VerificationError>>+Send+'a>> {
        Box::pin(self.run(selected,call))
    }
}
