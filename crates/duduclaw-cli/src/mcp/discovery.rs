//! Signed internal MCP callers use the same create/query service as RPC.
use super::*;
use duduclaw_gateway::discovery::service::{self, TrustedCaller};
fn caller(home: &Path, agent: &str) -> std::result::Result<TrustedCaller,String> {
    let claim=std::env::var(duduclaw_core::ENV_AGENT_ID).unwrap_or_default();
    if claim != agent { return Err("discovery requires an explicit signed caller identity".into()); }
    let token=std::env::var(duduclaw_core::ENV_AGENT_TOKEN).ok();
    TrustedCaller::from_signed_agent(home,&claim,token.as_deref())
}
pub(crate) async fn handle_discovery_create(args: &Value, home: &Path, agent: &str) -> Value {
    let caller=match caller(home,agent) {Ok(caller)=>caller,Err(error)=>return tool_error(&error)};
    let store=match duduclaw_gateway::task_store::TaskStore::open(home) {Ok(store)=>store,Err(error)=>return tool_error(&error)};
    let broker=match duduclaw_gateway::approval::ApprovalBroker::open(home) {Ok(broker)=>broker,Err(error)=>return tool_error(&error)};
    let mut request=args.clone();
    if request.get("assigned_to").is_none() { request["assigned_to"]=serde_json::json!(agent); }
    match service::create_from_value(home,&store,&broker,&caller,request).await {
        Ok(created)=>tool_text(&serde_json::to_string(&created).unwrap()),Err(error)=>tool_error(&error)
    }
}
pub(crate) async fn handle_discovery_query(name: &str,args: &Value,home: &Path,agent: &str) -> Value {
    let caller=match caller(home,agent) {Ok(caller)=>caller,Err(error)=>return tool_error(&error)};
    let run=args.get("run_id").and_then(Value::as_str).unwrap_or("");
    let result=match name {
        "discovery_catalog"=>service::catalog(home,&caller,args.get("agent_id").and_then(Value::as_str).unwrap_or(agent)),
        "discovery_list"=>service::list(home,&caller,args.get("agent_id").and_then(Value::as_str),
            args.get("limit").and_then(Value::as_u64).and_then(|limit|usize::try_from(limit).ok()).unwrap_or(20)).await,
        "discovery_tree"=>service::tree(home,&caller,run).await,
        "discovery_artifact"=>service::artifact(home,&caller,run,args.get("file_id").and_then(Value::as_str)).await,
        "discovery_cancel"=>service::cancel(home,&caller,run).await,
        _=>Err("unknown discovery operation".into()),
    };
    match result {Ok(value)=>tool_text(&value.to_string()),Err(error)=>tool_error(&error)}
}
