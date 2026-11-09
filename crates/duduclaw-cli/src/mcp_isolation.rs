//! Native Linux MCP adapter. Gateway-owned operations reuse original handlers
//! and employee gates; employee code and hostile parsing require A/E.
use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc};
use duduclaw_isolation::{capability::{Lane, ResourceGrant}, ids::EmployeeId};
use duduclaw_mcp_boundary::{RequestContext, VerificationError, resources::ResourceRoots};
use serde_json::{Value, json};
use crate::{mcp_auth::Principal, mcp_dispatch::McpDispatcher, mcp_namespace::NamespaceContext};

#[path = "mcp_isolation_collections.rs"]
pub(crate) mod collections;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BoundaryRoute {
    File,
    GatewayHandler(duduclaw_core::tool_effect::ToolEffect),
    Resource,
    Execution,
    Worker,
}

/// The effect table controls policy, not process safety: a read can spawn a
/// hardware probe or parse a hostile archive. Both inventories must agree.
pub(crate) fn boundary_route(name: &str) -> Option<BoundaryRoute> {
    use duduclaw_mcp_boundary::tool_policy::{Route, policy};
    let effect = duduclaw_core::tool_effect::effect_of_builtin(name)?;
    let policy = policy(name)?;
    // These are authenticated queue/store/control operations. No employee
    // subprocess is started by their original handlers; launch factories are
    // still mandatory for the separate run/parse operations.
    if crate::mcp_isolation_a_routes::is_target_dispatch(name)
        || crate::mcp_isolation_inference::supports(name)
        || (cfg!(target_os = "linux") && name == "discovery_artifact")
        || crate::mcp_fork::is_fork_tool(name)
        || matches!(name, "inspect_branches" | "diff_branches" | "fork_cost"
            | "discovery_catalog" | "discovery_list" | "discovery_tree" | "discovery_cancel") {
        return Some(BoundaryRoute::GatewayHandler(effect));
    }
    Some(match policy.route {
        Route::File => BoundaryRoute::File,
        Route::Worker => BoundaryRoute::Worker,
        // These operate gateway-owned containers through the existing session
        // and durable-workspace protocol, not the employee's host desktop.
        Route::Execution if matches!(name, "computer_screenshot" | "computer_click" |
            "computer_type" | "computer_login" | "computer_key" | "computer_scroll" |
            "computer_session_start" | "computer_navigate" | "computer_session_stop") =>
            BoundaryRoute::Resource,
        Route::Execution => BoundaryRoute::Execution,
        // HX5: same execution bridge; `ExecutionToolBridge::execute` sends these
        // to `execute_in_run` (the caller's own run), never reserve/spawn.
        Route::InRunHelper => BoundaryRoute::Execution,
        Route::Service if name == "hardware_info" => BoundaryRoute::Execution,
        Route::Service | Route::Connector => BoundaryRoute::Resource,
    })
}

fn host_e_control(name: &str) -> bool {
    matches!(name, "skill_from_recording" | "skill_hub_install" | "skill_curator_status" | "skill_synthesis_run" | "meeting_transcribe")
}

/// Target fields in the original tool schema are data, never caller identity.
/// Preserve them for the existing record/delegation/scope policy. Other routes
/// still prohibit host path and execution identity overrides.
pub(crate) fn check_tool_arguments(name: &str, arguments: &Value) -> Result<(), VerificationError> {
    // H12a: own-skill tools (incl. the curator's typed `run` flag) accept only
    // their exact protected schema; the subject is the capability-bound employee.
    if duduclaw_mcp_boundary::tool_policy::OWN_SKILL_TOOLS.contains(&name) {
        return duduclaw_mcp_boundary::tool_policy::check_own_skill_arguments(name, arguments);
    }
    // H12b/c/d: own-skill job tools, same rule (exact protected schema).
    if duduclaw_mcp_boundary::tool_policy::OWN_SKILL_JOB_TOOLS.contains(&name) {
        return duduclaw_mcp_boundary::tool_policy::check_own_skill_job_arguments(name, arguments);
    }
    if !matches!(boundary_route(name), Some(BoundaryRoute::Resource | BoundaryRoute::GatewayHandler(_))) {
        return duduclaw_mcp_boundary::check_arguments(arguments);
    }
    let fields = arguments.as_object().ok_or(VerificationError::Invalid)?;
    let tool = crate::mcp::tools().find(|tool| tool.name == name).ok_or(VerificationError::Invalid)?;
    for key in ["agent_id", "agent", "principal", "root", "cwd", "run", "run_id", "employee_id"] {
        if fields.contains_key(key) && !tool.params.iter().any(|param| param.name == key) {
            return Err(VerificationError::Invalid);
        }
    }
    Ok(())
}

/// Trusted service adapters for resource-scoped gateway operations and A/E/C.
/// No default implementation forwards to the old path-based handlers. A service
/// must retain each handler's record/audience/ownership gates as well as use only
/// the resolved grants. Execution/Worker routes must use A/E, never local spawn.
pub trait ResourceToolService: Send + Sync {
    fn supports(&self, tool: &str) -> bool;
    fn resolver(&self) -> &dyn duduclaw_mcp_boundary::tool_policy::ResourceResolver;
    fn check_handler_policy<'a>(&'a self, call: &'a duduclaw_mcp_boundary::tool_policy::AuthorizedToolCall)
        -> Pin<Box<dyn Future<Output = Result<(), VerificationError>> + Send + 'a>>;
    /// Return the MCP tool result, not a JSON-RPC envelope. Unknown or uncertain
    /// effects must not be described as a confirmed pre-dispatch rejection.
    fn execute<'a>(&'a self, call: duduclaw_mcp_boundary::tool_policy::AuthorizedToolCall)
        -> Pin<Box<dyn Future<Output = Result<Value, VerificationError>> + Send + 'a>>;
}

async fn authorize_resource_call(service: Arc<dyn ResourceToolService>, context: &RequestContext,
    name: &str, arguments: &Value) -> Result<duduclaw_mcp_boundary::tool_policy::AuthorizedToolCall, VerificationError> {
    let (context, name, arguments) = (context.clone(), name.to_owned(), arguments.clone());
    duduclaw_mcp_boundary::authority::verify_blocking(move ||
        duduclaw_mcp_boundary::tool_policy::authorize(&context, &name, &arguments, service.resolver())).await
}

/// Called after the legacy dispatcher gates and argument rewrites. Native file
/// handlers retain their fd-only path; every other tool uses an explicit service.
pub(crate) async fn dispatch_resource_tool(home: &std::path::Path, name: &str, arguments: &Value) -> Option<Value> {
    let current = current()?;
    if let Some(reason) = crate::mcp_isolation_disposition::intentional_refusal(name) {
        return Some(crate::mcp::tool_error(refusal_message(home, reason).await));
    }
    if home != current.home || check_tool_arguments(name, arguments).is_err() {
        return Some(crate::mcp::tool_error("無法確認這次操作的資源授權，已拒絕執行。"));
    }
    match boundary_route(name) {
        Some(BoundaryRoute::File) => return None,
        Some(BoundaryRoute::GatewayHandler(_)) => {
            if !current.context.record().tools.as_slice().iter().any(|tool| tool.as_str() == name) {
                return Some(crate::mcp::tool_error("無法確認這次操作的員工身分。"));
            }
            if crate::mcp_isolation_inference::supports(name) {
                return Some(crate::mcp_isolation_inference::handle(name, arguments, Some(home)).await);
            }
            return None;
        },
        Some(BoundaryRoute::Worker) => {
            return Some(match current.worker.as_ref() {
                Some(bridge) => bridge.execute(&current.context, name, arguments).await,
                None => crate::mcp::tool_error("隔離解析服務尚未就緒，已拒絕執行。"),
            });
        }
        Some(BoundaryRoute::Execution) => {
            return Some(match current.execution.as_ref() {
                Some(bridge) => bridge.execute(&current.context, name, arguments).await,
                None => crate::mcp::tool_error("隔離執行服務尚未就緒，已拒絕執行。"),
            });
        }
        _ => {}
    }
    let result = async {
        let service = current.services.as_ref().filter(|service| service.supports(name))
            .ok_or(VerificationError::Unavailable)?;
        let call = authorize_resource_call(service.clone(), &current.context, name, arguments).await?;
        service.check_handler_policy(&call).await?;
        // Re-resolve after an async handler-policy check. An approval or lookup
        // may have yielded while ownership/collection membership changed.
        let checked = authorize_resource_call(service.clone(), &current.context, name, arguments).await?;
        if checked.resources() != call.resources() { return Err(VerificationError::BindingMismatch); }
        let live = checked.context().clone();
        duduclaw_mcp_boundary::authority::verify_blocking(move ||
            live.verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)).await?;
        service.execute(checked).await
    }.await;
    Some(match result {
        Ok(result) => result,
        Err(_) => {
            duduclaw_security::audit::append_tool_call_with_extras(home, &current.agent, name,
                "isolation resource authorization refused", false,
                &[("error_class", json!("isolation_resource"))]);
            crate::mcp::tool_error("無法確認這次操作的資源授權，已拒絕執行。")
        }
    })
}

/// Builtin gateway services retain their existing handler, reached only with
/// an AuthorizedToolCall. Connectors never qualify for this adapter.
pub(crate) fn legacy_resource_handler(name: &str) -> bool {
    use duduclaw_mcp_boundary::tool_policy::{policy, Route};
    matches!(boundary_route(name), Some(BoundaryRoute::Resource))
        && policy(name).is_some_and(|p| p.route != Route::Connector)
}

pub(crate) async fn authorize_gateway_handler(home: &std::path::Path, name: &str, arguments: &Value)
    -> Result<duduclaw_mcp_boundary::tool_policy::AuthorizedToolCall, VerificationError> {
    let current = current().ok_or(VerificationError::Invalid)?;
    if home != current.home || !legacy_resource_handler(name) { return Err(VerificationError::Invalid); }
    check_tool_arguments(name, arguments)?;
    let service = current.services.as_ref().filter(|s| s.supports(name)).ok_or(VerificationError::Unavailable)?;
    let call = authorize_resource_call(service.clone(), &current.context, name, arguments).await?;
    service.check_handler_policy(&call).await?;
    let checked = authorize_resource_call(service.clone(), &current.context, name, arguments).await?;
    if checked.resources() != call.resources() { return Err(VerificationError::BindingMismatch); }
    let live = checked.context().clone();
    duduclaw_mcp_boundary::authority::verify_blocking(move ||
        live.verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)).await?;
    Ok(checked)
}

pub(crate) struct IsolatedCall {
    pub home: PathBuf,
    pub context: RequestContext,
    pub agent: String,
    pub roots: Arc<ResourceRoots>,
    pub parser: Option<Arc<dyn duduclaw_mcp_boundary::parsing::FileParser>>,
    pub services: Option<Arc<dyn ResourceToolService>>,
    pub worker: Option<Arc<duduclaw_mcp_boundary::worker_execution::WorkerToolBridge>>,
    pub execution: Option<Arc<crate::mcp_isolation_execution::ExecutionToolBridge>>,
    pub forks: Option<Arc<dyn duduclaw_mcp_boundary::forks::ForkToolService>>,
    pub record_resolver: Option<Arc<dyn duduclaw_mcp_boundary::tool_policy::ResourceResolver>>,
}

tokio::task_local! { pub(crate) static ISOLATED_CALL: Arc<IsolatedCall>; }

pub(crate) fn current() -> Option<Arc<IsolatedCall>> {
    ISOLATED_CALL.try_with(Arc::clone).ok()
}

pub(crate) async fn refusal_message(home: &std::path::Path,
    reason: crate::mcp_isolation_disposition::Refusal) -> &'static str {
    if reason != crate::mcp_isolation_disposition::Refusal::ForkNextRelease { return reason.message(); }
    let language = duduclaw_gateway::prompt_identity::read_default_language(home).await;
    reason.message_for_language(language.as_deref())
}

pub(crate) fn process_lane() -> duduclaw_core::ProcessLane {
    match current().map(|call| call.context.record().lane) {
        Some(Lane::Standard) => duduclaw_core::ProcessLane::Normal,
        Some(Lane::Explore) => duduclaw_core::ProcessLane::Explore,
        None => duduclaw_core::ProcessLane::current(),
    }
}

/// Shared list/call predicate. Capability grants only narrow existing scopes.
pub(crate) fn tool_allowed(name: &str, principal: &Principal) -> bool {
    let Some(call) = current() else { return true; };
    let record = call.context.record();
    if crate::mcp_isolation_disposition::intentional_refusal(name).is_some() { return false; }
    let supported = match boundary_route(name) {
        Some(BoundaryRoute::File) => name == "file_read" || call.parser.is_some(),
        Some(BoundaryRoute::GatewayHandler(_)) if crate::mcp_fork::is_fork_tool(name) =>
            call.forks.as_ref().is_some_and(|service| service.supports(name)),
        Some(BoundaryRoute::GatewayHandler(_)) => true,
        Some(BoundaryRoute::Worker) => call.worker.as_ref().is_some_and(|bridge| bridge.supports(name)),
        Some(BoundaryRoute::Execution) => call.execution.as_ref().is_some_and(|bridge| bridge.supports(name)),
        Some(BoundaryRoute::Resource) => call.services.as_ref().is_some_and(|service| service.supports(name))
            && (!host_e_control(name) || call.parser.is_some())
            && (name != "web_fetch_cached" || call.record_resolver.is_some()),
        None => false,
    };
    supported && duduclaw_mcp_boundary::tool_policy::eligible(&call.context, name)
        && record.tools.as_slice().iter().any(|tool| tool.as_str() == name)
        && crate::mcp_auth::tool_requires_scope(name).is_none_or(|required|
            principal.scopes.contains(&required) || principal.scopes.contains(&crate::mcp_auth::Scope::Admin))
}

/// Constructed by the trusted host after registry/key-policy lookup. The context
/// supplied by boundary::server has already been checked against live authority.
/// No bearer or environment variable is needed by this adapter.
pub struct IsolationDispatcher {
    employee: EmployeeId,
    agent: String,
    principal: Principal,
    namespace: NamespaceContext,
    dispatcher: McpDispatcher,
    roots: Arc<ResourceRoots>,
    parser: Option<Arc<dyn duduclaw_mcp_boundary::parsing::FileParser>>,
    services: Option<Arc<dyn ResourceToolService>>,
    worker: Option<Arc<duduclaw_mcp_boundary::worker_execution::WorkerToolBridge>>,
    execution: Option<Arc<crate::mcp_isolation_execution::ExecutionToolBridge>>,
    forks: Option<Arc<dyn duduclaw_mcp_boundary::forks::ForkToolService>>,
    record_resolver: Option<Arc<dyn duduclaw_mcp_boundary::tool_policy::ResourceResolver>>,
    turn_resolver: Option<Arc<dyn duduclaw_gateway::isolation_egress::RunTurnResolver>>,
    workflow: Option<(duduclaw_isolation::ids::RunId, Arc<crate::mcp::workflow_operation::WorkflowSession>)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Ready,
    IntentionalRefusal(crate::mcp_isolation_disposition::Refusal),
    MissingFactory,
}

impl IsolationDispatcher {
    /// Host-only readiness inventory. It describes installed implementations,
    /// not permission to call: capability and original policy gates still apply.
    /// A nonempty factory that supports no tools cannot look fully ready here.
    pub fn service_manifest(&self) -> Vec<(&'static str, ServiceState)> {
        crate::mcp::tools().map(|tool| {
            let name = tool.name;
            let state = if let Some(reason) = crate::mcp_isolation_disposition::intentional_refusal(name) {
                ServiceState::IntentionalRefusal(reason)
            } else {
                let ready = match boundary_route(name) {
                    Some(BoundaryRoute::GatewayHandler(_)) if crate::mcp_fork::is_fork_tool(name) =>
                        self.forks.as_ref().is_some_and(|service| service.supports(name)),
                    Some(BoundaryRoute::GatewayHandler(_)) => true,
                    Some(BoundaryRoute::File) => name == "file_read" || self.parser.is_some(),
                    Some(BoundaryRoute::Resource) => self.services.as_ref().is_some_and(|s| s.supports(name))
                        && (!host_e_control(name) || self.parser.is_some())
                        && (name != "web_fetch_cached" || self.record_resolver.is_some()),
                    Some(BoundaryRoute::Execution) => self.execution.as_ref().is_some_and(|s| s.supports(name)),
                    Some(BoundaryRoute::Worker) => self.worker.as_ref().is_some_and(|s| s.supports(name)),
                    None => false,
                };
                if ready { ServiceState::Ready } else { ServiceState::MissingFactory }
            };
            (name, state)
        }).collect()
    }

    pub fn require_complete_services(&self) -> Result<(), VerificationError> {
        if self.service_manifest().iter().any(|(_, state)| *state == ServiceState::MissingFactory) {
            return Err(VerificationError::Unavailable);
        }
        Ok(())
    }

    pub fn require_context_services(&self, context: &RequestContext) -> Result<(), VerificationError> {
        self.require_complete_services()?;
        if context.record().employee != self.employee { return Err(VerificationError::BindingMismatch); }
        context.record().validate_resource_mode().map_err(|_| VerificationError::Invalid)?;
        Ok(())
    }

    pub fn new(home: PathBuf, employee: EmployeeId, agent: String,
        principal: Principal, roots: Arc<ResourceRoots>) -> Result<Self, VerificationError> {
        if principal.is_external || !duduclaw_core::is_valid_agent_id(&agent)
            || duduclaw_core::is_reserved_agent_id(&agent)
            || agent == duduclaw_core::UNTRUSTED_AGENT_ID
            || (principal.client_id != duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID
                && principal.client_id != agent)
            || !crate::mcp_namespace::client_is_agent(&home, &agent) {
            return Err(VerificationError::Invalid);
        }
        let namespace = crate::mcp_namespace::resolve_for_caller(&principal,
            crate::mcp_namespace::CallerIdentity { verified_agent: Some(&agent), client_is_agent: true })
            .map_err(|_| VerificationError::Invalid)?;
        let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30))
            .build().map_err(|_| VerificationError::Unavailable)?;
        // F6fb item 2 (H4): this dispatcher writes the home's memory.db itself,
        // so it carries the label writer gate (every write of a tombstoned or
        // quarantined label refused, triggers included). F6fb2 item 5: it runs
        // inside the gateway process (the factory is installed by
        // `cmd_run_server`, served by `McpBoundaryService::attach`), i.e. as
        // the gateway uid that owns apply.db; the gateway's boot gate is
        // already installed and is marked always-active here.
        let _ = duduclaw_gateway::install_isolated_process_writer_gate();
        let memory = duduclaw_memory::SqliteMemoryEngine::new(&home.join("memory.db"))
            .map_err(|_| VerificationError::Unavailable)?;
        let redaction = crate::mcp_redaction::McpRedactionLayer::try_init_for_identity(
            &home, &agent, "isolated-mcp")
            .map_err(|_| VerificationError::Unavailable)?;
        let dispatcher = McpDispatcher::new(home, http, Arc::new(memory), agent.clone(),
            Arc::new(crate::odoo_pool::OdooConnectorPool::default()),
            crate::mcp_rate_limit::RateLimiter::new(), crate::mcp_memory_quota::DailyQuota::new())
            .with_redaction(redaction.map(Arc::new));
        Ok(Self { employee, agent, principal, namespace, dispatcher, roots,
            parser: None, services: None, execution: None, worker: None, forks: None, record_resolver: None, turn_resolver: None, workflow: None })
    }

    /// Boot wiring: A/E readiness and resource/account authority are injected
    /// together. Missing A/E services remain absent; no process fallback exists.
    pub fn with_gateway_services(mut self,
        resolver: Arc<duduclaw_gateway::isolation_resources::GatewayResourceResolver>,
        factories: duduclaw_gateway::isolation_execution::GatewayFactories,
        connectors: Option<Arc<dyn duduclaw_gateway::isolation_connectors::BoundConnectorExecutor>>) -> Self {
        self.services = Some(Arc::new(crate::mcp_isolation_resources::GatewayResourceToolService::new(
            self.dispatcher.home_dir.clone(), self.agent.clone(), self.roots.clone(), resolver.clone(), connectors)));
        self.record_resolver = Some(resolver);
        self.forks = factories.forks;
        self.execution = factories.execution;
        self.worker = factories.worker;
        self.parser = factories.parser;
        self
    }

    pub fn with_file_parser(mut self, parser: Arc<dyn duduclaw_mcp_boundary::parsing::FileParser>) -> Self {
        self.parser = Some(parser); self
    }

    pub fn with_resource_services(mut self, services: Arc<dyn ResourceToolService>) -> Self {
        self.services = Some(services); self
    }

    pub fn with_execution_bridge(mut self, bridge: Arc<crate::mcp_isolation_execution::ExecutionToolBridge>) -> Self {
        self.execution = Some(bridge); self
    }

    /// Resource resolver retained for A/E integration. Resource services enforce
    /// finite run grants or the explicitly signed live employee authority.
    pub fn with_record_resolver(mut self,
        resolver: Arc<dyn duduclaw_mcp_boundary::tool_policy::ResourceResolver>) -> Self {
        self.record_resolver = Some(resolver); self
    }

    pub fn with_turn_resolver(mut self, resolver: Arc<dyn duduclaw_gateway::isolation_egress::RunTurnResolver>) -> Self {
        self.turn_resolver = Some(resolver); self
    }

    /// Optional workflow service session, installed by the gateway for one run.
    /// The employee never receives its signing secret.
    pub fn with_workflow_session(mut self, run: duduclaw_isolation::ids::RunId,
        id: String, secret: String) -> Result<Self, VerificationError> {
        let session = crate::mcp::workflow_operation::WorkflowSession::from_verified_host(
            &self.dispatcher.home_dir, &self.agent, &self.principal, id, secret)
            .map_err(|_| VerificationError::Invalid)?;
        self.workflow = Some((run, session));
        Ok(self)
    }

    async fn request(&self, request: Value) -> Result<Option<Value>, VerificationError> {
        let object = request.as_object().ok_or(VerificationError::Invalid)?;
        if object.keys().any(|key| !matches!(key.as_str(), "jsonrpc" | "id" | "method" | "params"))
            || request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(VerificationError::Invalid);
        }
        let method = request.get("method").and_then(Value::as_str).ok_or(VerificationError::Invalid)?;
        let Some(id) = request.get("id") else {
            return if method == "notifications/initialized" { Ok(None) } else { Err(VerificationError::Invalid) };
        };
        if !(id.is_string() || id.is_i64() || id.is_u64()) { return Err(VerificationError::Invalid); }
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
        let fields = params.as_object().ok_or(VerificationError::Invalid)?;
        let modern = if method == "initialize" { false } else {
            match crate::mcp_protocol::request_era(&params) {
                crate::mcp_protocol::RequestEra::Legacy => false,
                crate::mcp_protocol::RequestEra::Modern(_) => true,
                crate::mcp_protocol::RequestEra::Unsupported(version) =>
                    return Ok(Some(crate::mcp_protocol::unsupported_version_error(id, &version))),
            }
        };
        if modern && let Some(problem) = crate::mcp_protocol::modern_meta_problem(&params) {
            return Ok(Some(crate::mcp_protocol::modernize_response(
                crate::mcp::jsonrpc_error(id, -32602, &problem))));
        }
        let capabilities = || json!({"tools": {"listChanged": false}});
        let response = match (method, modern) {
            ("initialize", _) => {
                if fields.keys().any(|key| !matches!(key.as_str(), "protocolVersion" | "capabilities" | "clientInfo" | "_meta")) {
                    return Err(VerificationError::Invalid);
                }
                crate::mcp::jsonrpc_response(id, crate::mcp_protocol::initialize_result(&params, capabilities()))
            }
            ("ping", false) | ("server/discover", true) => {
                if fields.keys().any(|key| key != "_meta") { return Err(VerificationError::Invalid); }
                crate::mcp::jsonrpc_response(id, if method == "ping" { json!({}) }
                    else { crate::mcp_protocol::discover_result(capabilities()) })
            }
            ("tools/list", _) => {
                if fields.keys().any(|key| key != "_meta") { return Err(VerificationError::Invalid); }
                let mut response = crate::mcp::handle_tools_list_for_agent(id, &self.principal,
                    &self.dispatcher.home_dir, &self.agent).await;
                if let Some(tools) = response["result"]["tools"].as_array_mut() {
                    for tool in tools {
                        project_tool_schema(tool);
                        if matches!(tool["name"].as_str(), Some("file_read" | "csv_read" | "xlsx_read")) {
                            tool["inputSchema"]["properties"]["workspace"] = json!({"type": "string",
                                "description": "這次執行獲授權的工作區識別碼"});
                            tool["inputSchema"]["properties"]["path"]["description"] = json!("工作區內的相對檔案路徑");
                            if let Some(required) = tool["inputSchema"]["required"].as_array_mut() {
                                required.push(json!("workspace"));
                            } else { tool["inputSchema"]["required"] = json!(["path", "workspace"]); }
                        }
                    }
                }
                response
            }
            ("tools/call", _) => {
                if fields.keys().any(|key| !matches!(key.as_str(), "name" | "arguments" | "_meta")) {
                    return Err(VerificationError::Invalid);
                }
                let name = params.get("name").and_then(Value::as_str).ok_or(VerificationError::Invalid)?;
                if name.is_empty() || name.len() > 128
                    || !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') {
                    return Err(VerificationError::Invalid);
                }
                check_tool_arguments(name, params.get("arguments").unwrap_or(&json!({})))?;
                // Reuse all gates, including approvals, egress and result redaction.
                let mut dispatcher = self.dispatcher.clone();
                dispatcher.workflow_session = self.workflow.as_ref().filter(|(run, _)|
                    current().is_some_and(|call| call.context.record().run == *run))
                    .map(|(_, session)| session.clone());
                if let (Some(call), Some(layer)) = (current(), dispatcher.redaction.as_ref()) {
                    let run: String = call.context.record().run.0.iter().map(|byte| format!("{byte:02x}")).collect();
                    dispatcher.redaction = Some(Arc::new(crate::mcp_redaction::McpRedactionLayer {
                        manager: layer.manager.clone(), agent_id: self.agent.clone(), session_id: format!("isolated:{run}"),
                    }));
                }
                // Bound the request future before it crosses the nested identity scopes.
                Box::pin(dispatcher.dispatch_tool_call(&self.principal, &self.namespace, &params, id)).await
            }
            _ => crate::mcp::jsonrpc_error(id, -32601, "不支援此隔離執行方法。"),
        };
        let response = if modern { crate::mcp_protocol::modernize_response(response) } else { response };
        let encoded = serde_json::to_string(&response).map_err(|_| VerificationError::Invalid)?;
        let redacted = crate::mcp_redact::redact(&encoded);
        Ok(Some(serde_json::from_str(&redacted).map_err(|_| VerificationError::Invalid)?))
    }
}

fn project_tool_schema(tool: &mut Value) {
    crate::mcp_isolation_inference::describe(tool);
    if tool["name"] == "fork_cost" {
        tool["description"] = json!("Read an owned protected fork's cost availability. Usage is not yet measured: measurement_status is unavailable, aggregate and per-branch spend are null, never zero estimates.");
    }
    if tool["name"] == "merge_or_select" {
        tool["description"] = json!("Select an exact completed branch result using the host parent revision and digest. Returns manifest_selected; does not overwrite the live parent filesystem.");
        tool["inputSchema"]["properties"] = json!({
            "fork_id":{"type":"string"}, "branch_id":{"type":"string"},
            "expected_revision":{"type":"integer","minimum":0},
            "expected_parent_digest":{"type":"string","pattern":"^[0-9a-f]{64}$"}});
        tool["inputSchema"]["required"] = json!(["fork_id","branch_id","expected_revision","expected_parent_digest"]);
        tool["inputSchema"]["additionalProperties"] = json!(false);
    }
    if tool["name"] == "diff_branches" {
        tool["description"] = json!("Compare authenticated recorded branch snapshots by their manifests; does not read live parent files.");
        tool["inputSchema"]["properties"] = json!({"fork_id":{"type":"string"},
            "left_branch_id":{"type":"string"},"right_branch_id":{"type":"string"}});
        tool["inputSchema"]["required"] = json!(["fork_id","left_branch_id","right_branch_id"]);
        tool["inputSchema"]["additionalProperties"] = json!(false);
    }

    if tool["name"] == "browser_record_start" {
        tool["description"] = json!("Start a protected headless browser recording inside this run (headless=true required, 1–300 seconds, default 30). Returns {status: recording, id}; it never outlives this run and is flushed at run end. Artifacts go to recordings/<id>/ in the authorized workspace. Reaches only this employee's public-web egress.");
        tool["inputSchema"]["properties"]["headless"] = json!({"type":"boolean","const":true,"description":"受保護錄製須明確指定 true；不開啟主機桌面視窗"});
        tool["inputSchema"]["properties"]["max_seconds"] = json!({"type":"integer","minimum":1,"maximum":300,"default":30});
        if let Some(required) = tool["inputSchema"]["required"].as_array_mut() { required.push(json!("headless")); }
        else { tool["inputSchema"]["required"] = json!(["headless"]); }
    }
    if tool["name"] == "browser_record_stop" {
        tool["description"] = json!("Stop your protected browser recording by its returned id and return its result with artifact sha256 digests. In the same run this flushes the recording; returns stop_requested if the flush is still running (repeat to read the result). In a later run it returns the stored result.");
        tool["inputSchema"]["properties"]["id"] = json!({"type":"string","pattern":"^[0-9a-f]{32}$"});
    }
    let name = tool["name"].as_str().unwrap_or("").to_owned();
    if matches!(boundary_route(&name), Some(BoundaryRoute::GatewayHandler(_))) { return; }
    // H12a: own-skill tools act only on the capability-bound employee, so the
    // protected schema offers no subject selector. A legacy `agent_id` is still
    // accepted as a self-reference and checked by the producer; it is never
    // rewritten into another field (no `target_employee`).
    // H12b/c/d: the same for own-skill job tools, which also lose the install
    // target selectors (`scope`, `owner`): they publish only into own SKILLS.
    if duduclaw_mcp_boundary::tool_policy::OWN_SKILL_TOOLS.contains(&name.as_str())
        || duduclaw_mcp_boundary::tool_policy::OWN_SKILL_JOB_TOOLS.contains(&name.as_str()) {
        let removed: &[&str] = if duduclaw_mcp_boundary::tool_policy::OWN_SKILL_JOB_TOOLS.contains(&name.as_str()) {
            &["agent_id", "target_agent", "scope", "owner"]
        } else { &["agent_id", "target_agent"] };
        if let Some(required) = tool["inputSchema"]["required"].as_array_mut() {
            required.retain(|value| !value.as_str().is_some_and(|key| removed.contains(&key)));
        }
        if let Some(properties) = tool["inputSchema"]["properties"].as_object_mut() {
            for key in removed { properties.remove(*key); }
        }
        if name == "skill_from_recording" {
            tool["inputSchema"]["properties"]["id"] = json!({"type":"string","pattern":"^[0-9a-f]{32}$",
                "description":"受保護錄製回傳的 id（必須是你自己已完成的錄製）"});
        }
        return;
    }
    if matches!(boundary_route(&name), Some(BoundaryRoute::Resource)) {
        if duduclaw_gateway::isolation_connectors::provider_for_tool(&name).is_some() {
            if let Some(properties) = tool["inputSchema"]["properties"].as_object_mut() {
                properties.insert("connector_account".into(), json!({"type":"string",
                    "description":"這次執行獲授權的連線帳號識別碼；省略時必須只有一個有效帳號"}));
            }
        }
        return;
    }
    let schema = &mut tool["inputSchema"];
    let mut target_required = false;
    if let Some(required) = schema["required"].as_array_mut() {
        target_required = required.iter().any(|value| value == "agent_id");
        required.retain(|value| !matches!(value.as_str(), Some("agent_id" | "agent" | "principal" | "root" | "cwd" | "run" | "run_id" | "employee_id")));
    }
    if let Some(properties) = schema["properties"].as_object_mut() {
        if properties.remove("agent_id").is_some() {
            properties.insert("target_employee".into(), json!({"type":"string", "pattern":"^[0-9a-f]{32}$",
                "description":"這次執行獲授權的目標員工識別碼；不會改變呼叫者身分"}));
        }
        for key in ["agent", "principal", "root", "cwd", "run", "run_id", "employee_id"] { properties.remove(key); }
        if name == "code_map" {
            properties.insert("workspace".into(), json!({"type":"string", "description":"這次執行獲授權的工作區識別碼"}));
            properties.insert("path".into(), json!({"type":"string", "description":"工作區內的相對目錄路徑；省略表示整個工作區"}));
        }
    }
    // HX5/HX6: execute_program / office_script / code_map and the browser
    // recording run inside the caller's run; none has a pending child-run job
    // to query or cancel (`browser_record_stop {id}` reads the recording).
    if target_required {
        schema["required"].as_array_mut().expect("required array checked above").push(json!("target_employee"));
    }
    if name == "code_map" {
        if let Some(required) = schema["required"].as_array_mut() { required.push(json!("workspace")); }
        else { schema["required"] = json!(["workspace"]); }
    }
}

impl duduclaw_mcp_boundary::server::Dispatcher for IsolationDispatcher {
    fn require_complete_services(&self) -> Result<(), VerificationError> {
        IsolationDispatcher::require_complete_services(self)
    }
    fn require_context_services(&self, context: &RequestContext) -> Result<(), VerificationError> {
        IsolationDispatcher::require_context_services(self, context)
    }

    fn dispatch<'a>(&'a self, context: RequestContext, agent_slug: &'a str, request: Value)
        -> Pin<Box<dyn Future<Output = Result<Option<Value>, VerificationError>> + Send + 'a>> {
        Box::pin(async move {
            if context.record().employee != self.employee || agent_slug != self.agent {
                return Err(VerificationError::BindingMismatch);
            }
            let call = Arc::new(IsolatedCall { home: self.dispatcher.home_dir.clone(),
                context: context.clone(), agent: self.agent.clone(), roots: self.roots.clone(),
                parser: self.parser.clone(), services: self.services.clone(), execution: self.execution.clone(), worker: self.worker.clone(),
                record_resolver: self.record_resolver.clone(), forks: self.forks.clone() });
            duduclaw_gateway::isolation_egress::scope(&self.dispatcher.home_dir, &self.agent, &context,
                self.turn_resolver.clone(), ISOLATED_CALL.scope(call, crate::mcp_auth::with_authenticated_call(
                crate::mcp_auth::AuthenticatedCall::without_reference(self.principal.clone()),
                duduclaw_mcp_boundary::io_authority::scope(context.clone(), Box::pin(self.request(request)))))).await?
        })
    }
}

#[cfg(test)]
#[path = "mcp_isolation_tests.rs"]
mod tests;
