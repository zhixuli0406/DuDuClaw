//! Public model-registry adapter. Hardware is a gateway boot snapshot; model
//! discovery never executes a hardware probe or reads a run's environment.
use std::{future::Future,path::PathBuf,pin::Pin,sync::Arc};
use duduclaw_isolation::ids::EmployeeId;
use duduclaw_mcp_boundary::{RequestContext,VerificationError,tool_policy::AuthorizedToolCall};
use serde_json::{Value,json};
use crate::isolation_connectors::{BoundConnectorExecutor,LiveConnectorSelectionPolicy,ProductConnectorAccount,SelectedConnector};

pub const MODEL_TOOLS: &[&str] = &["model_search","model_recommend"];
pub struct ModelConnectorProvider {
    home: PathBuf,agent: String,employee: EmployeeId,account: ProductConnectorAccount,
    agent_config: String,hardware: duduclaw_inference::types::HardwareInfo,
}
impl ModelConnectorProvider {
    pub fn new(home: PathBuf,agent: String,employee: EmployeeId,account: ProductConnectorAccount,
        hardware: duduclaw_inference::types::HardwareInfo) -> Result<Self,VerificationError> {
        if !home.is_absolute() || !duduclaw_core::is_valid_agent_id(&agent) { return Err(VerificationError::Invalid); }
        let agent_config = effective_agent(&home,&agent)?;
        Ok(Self { home,agent,employee,account,agent_config,hardware })
    }
    fn live(&self,tool: &str) -> Result<(),VerificationError> {
        let raw = effective_agent(&self.home,&self.agent)?;
        if raw != self.agent_config { return Err(VerificationError::BindingMismatch); }
        let config: toml::Value = toml::from_str(&raw).map_err(|_| VerificationError::Invalid)?;
        let caps = config.get("capabilities");
        if caps.and_then(|c|c.get("denied_tools")).and_then(toml::Value::as_array)
            .is_some_and(|v|v.iter().any(|v|v.as_str()==Some(tool))) { return Err(VerificationError::Unavailable); }
        if caps.and_then(|c|c.get("allowed_tools")).and_then(toml::Value::as_array)
            .is_some_and(|v|!v.is_empty() && !v.iter().any(|v|v.as_str()==Some(tool))) { return Err(VerificationError::Unavailable); }
        if config.get("agent").and_then(|a|a.get("status")).and_then(toml::Value::as_str)
            .is_some_and(|s|s!="active") { return Err(VerificationError::Unavailable); }
        Ok(())
    }
    async fn run(&self,selected: SelectedConnector,call: AuthorizedToolCall) -> Result<Value,VerificationError> {
        selected.verify_call(&call)?;
        let selected = Arc::new(selected);
        self.live(call.name())?;
        let hw = &self.hardware;
        let curated = duduclaw_inference::model_registry::curated::builtin_registry();
        let mut entries = duduclaw_inference::model_registry::curated::filter_by_hardware(&curated,hw.ram_available_mb);
        let mut text = match call.name() {
            "model_search" => {
                let query = call.arguments().get("query").and_then(Value::as_str).unwrap_or("chat gguf");
                let check_selected = selected.clone();
                let check = Arc::new(move ||check_selected.recheck().is_ok());
                let results = duduclaw_inference::model_registry::hf_api::search_models_checked(query,hw.ram_available_mb,check)
                    .await.map_err(|_|VerificationError::Unavailable)?;
                for entry in results { if !entries.iter().any(|e|e.repo==entry.repo && e.filename==entry.filename) { entries.push(entry); } }
                format!("Models for '{}' (RAM: {} MB):\n",query.chars().take(200).collect::<String>(),hw.ram_available_mb)
            },
            "model_recommend" => format!("Hardware: {} ({:?})\nRAM: {} MB available / {} MB total\nRecommended max model: {:.1} GB\n\nRecommended models:\n",
                hw.gpu_name,hw.gpu_type,hw.ram_available_mb,hw.ram_total_mb,hw.recommended_max_model_gb),
            _ => return Err(VerificationError::Unavailable),
        };
        if entries.is_empty() { text.push_str("\n  No models fit in available RAM."); }
        for (i,e) in entries.iter().take(if call.name()=="model_search" {15} else {usize::MAX}).enumerate() {
            let tier = match e.tier { duduclaw_inference::model_registry::ModelTier::Recommended=>"[推薦]",_=>"[社群]" };
            text.push_str(&format!("\n  {}. {} {} ({}, {}) — {}\n     repo: {} file: {}",i+1,tier,e.name,e.params,e.size_display(),e.description,e.repo,e.filename));
        }
        selected.recheck()?;
        Ok(json!({"content":[{"type":"text","text":text}]}))
    }
}
fn effective_agent(home: &std::path::Path,agent: &str) -> Result<String,VerificationError> {
    let dir=crate::ephemeral::resolve_agent_dir(home,agent).or_else(||(!crate::ephemeral::is_ephemeral_id(agent))
        .then(||home.join("agents").join(agent))).ok_or(VerificationError::Unavailable)?;
    duduclaw_core::agent_toml::read_effective_text(&dir).map_err(|_|VerificationError::Unavailable)
}
impl LiveConnectorSelectionPolicy for ModelConnectorProvider {
    fn supports(&self,tool:&str)->bool {MODEL_TOOLS.contains(&tool)}
    fn candidates(&self,context:&RequestContext,tool:&str,provider:&str)->Result<Vec<ProductConnectorAccount>,VerificationError> {
        context.verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)?;
        if context.record().employee!=self.employee || provider!="model" || !MODEL_TOOLS.contains(&tool) {return Err(VerificationError::Unavailable);}
        self.live(tool)?;
        Ok(vec![self.account.clone()])
    }
}
impl BoundConnectorExecutor for ModelConnectorProvider {
    fn supports(&self,tool:&str)->bool {MODEL_TOOLS.contains(&tool)}
    fn execute<'a>(&'a self,selected:SelectedConnector,call:AuthorizedToolCall)->Pin<Box<dyn Future<Output=Result<Value,VerificationError>>+Send+'a>> {
        Box::pin(self.run(selected,call))
    }
}
