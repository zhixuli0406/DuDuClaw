//! Only the verified internal stdio route executes MCP workflow steps.
use super::*;
use crate::approval::{
    ApprovalBroker, DecisionContext, ExecutionBinding, OperationState, payload_hash,
    policy_revision,
};
use duduclaw_core::workflow_mcp::{self, ExecuteTicket, PrepareContext, PrepareTicket};
use duduclaw_llm::{McpClient, McpError};
use rand::RngCore;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

pub const READ_TOOLS: &[&str] = &["web_fetch_cached"];
pub const EFFECT_TOOLS: &[&str] = &["tasks_update", "update_cron_task"];
pub struct WorkflowExecutor {
    home: PathBuf,
    binary: PathBuf,
}
pub struct WorkflowSession {
    client: McpClient,
    secret: Vec<u8>,
    session_id: String,
    environment: WorkflowEnvironment,
}
impl WorkflowExecutor {
    pub fn new(home: PathBuf, binary: PathBuf) -> Result<Self, String> {
        Ok(Self {
            home: std::fs::canonicalize(home).map_err(|e| e.to_string())?,
            binary: std::fs::canonicalize(binary).map_err(|e| e.to_string())?,
        })
    }
    pub fn environment(&self, actor: &str) -> Result<WorkflowEnvironment, String> {
        if !duduclaw_core::is_valid_agent_id(actor) {
            return Err("invalid workflow actor".into());
        }
        Ok(WorkflowEnvironment {
            schema_version: 1,
            binary_version: env!("CARGO_PKG_VERSION").into(),
            binary_hash: binary_hash(&self.binary)?,
            home: self.home.to_string_lossy().into_owned(),
            cwd: std::fs::canonicalize(self.home.join("agents").join(actor))
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .into_owned(),
            receipt_adapter_version: 1,
        })
    }
    pub async fn connect(&self, actor: &str) -> Result<WorkflowSession, String> {
        let environment = self.environment(actor)?;
        let mut envs = duduclaw_core::agent_identity_env_vars(&self.home, actor);
        if !envs.iter().any(|(k, _)| k == "DUDUCLAW_AGENT_TOKEN") {
            return Err("verified workflow identity unavailable".into());
        }
        let key = crate::mcp_internal_key::valid_internal_keys(&self.home)
            .into_iter()
            .next()
            .ok_or("workflow internal MCP key unavailable")?;
        let mut secret = vec![0u8; 32];
        rand::thread_rng().fill_bytes(&mut secret);
        let session_id = uuid::Uuid::new_v4().to_string();
        envs.extend([
            ("DUDUCLAW_HOME".into(), environment.home.clone()),
            ("DUDUCLAW_MCP_API_KEY".into(), key),
            ("DUDUCLAW_WORKFLOW_SESSION_ID".into(), session_id.clone()),
            (
                "DUDUCLAW_WORKFLOW_SESSION_SECRET".into(),
                hex::encode(&secret),
            ),
        ]);
        if let Ok(raw) = std::fs::read_to_string(self.home.join("config.toml")) {
            let config: toml::Value =
                toml::from_str(&raw).map_err(|_| "workflow host config invalid")?;
            if let Some(proxy) = config
                .get("workflow")
                .and_then(|w| w.get("staging_proxy"))
                .and_then(toml::Value::as_str)
            {
                if config
                    .get("workflow")
                    .and_then(|w| w.get("fixture_environment"))
                    .and_then(toml::Value::as_str)
                    != Some("staging")
                {
                    return Err("workflow proxy requires explicit staging home".into());
                }
                let url = url::Url::parse(proxy).map_err(|_| "invalid workflow staging proxy")?;
                if url.scheme() != "http"
                    || url.host_str() != Some("127.0.0.1")
                    || url.port().is_none()
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.path() != "/"
                    || url.query().is_some()
                    || url.fragment().is_some()
                {
                    return Err("workflow staging proxy scope refused".into());
                }
                envs.extend([
                    ("HTTP_PROXY".into(), proxy.into()),
                    ("http_proxy".into(), proxy.into()),
                    ("NO_PROXY".into(), String::new()),
                    ("no_proxy".into(), String::new()),
                ]);
            }
        }
        let args = vec!["mcp-server".into()];
        let client = McpClient::connect(
            self.binary
                .to_str()
                .ok_or("workflow binary path unsupported")?,
            &args,
            &envs,
            Duration::from_secs(240),
        )
        .await
        .map_err(|e| format!("workflow stdio unavailable: {e}"))?;
        Ok(WorkflowSession {
            client,
            secret,
            session_id,
            environment,
        })
    }
}
/// SHA-256 of the CLI binary this gateway spawns, cached per process and
/// keyed by the file's identity (size, mtime and, on Unix, device/inode),
/// so replacing the binary on disk is still noticed.
fn binary_hash(path: &Path) -> Result<String, String> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static CACHE: std::sync::OnceLock<Mutex<HashMap<PathBuf, (String, String)>>> =
        std::sync::OnceLock::new();
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    let inode = {
        use std::os::unix::fs::MetadataExt;
        format!("{}:{}", meta.dev(), meta.ino())
    };
    #[cfg(not(unix))]
    let inode = String::new();
    let identity = format!(
        "{}:{:?}:{inode}",
        meta.len(),
        meta.modified().map_err(|e| e.to_string())?
    );
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some((key, hash)) = cache.lock().map_err(|_| "binary hash cache poisoned")?.get(path) {
        if *key == identity {
            return Ok(hash.clone());
        }
    }
    let hash = hex::encode(Sha256::digest(
        std::fs::read(path).map_err(|e| e.to_string())?,
    ));
    cache
        .lock()
        .map_err(|_| "binary hash cache poisoned")?
        .insert(path.to_path_buf(), (identity, hash.clone()));
    Ok(hash)
}

pub fn authority_digest(
    run: &WorkflowRun,
    revision: &ApprovedWorkflowRevision,
    step: &StepDefinition,
    evidence: &StepEvidence,
) -> String {
    payload_hash(
        &json!({
            "revision_hash": revision.revision_hash,
            "creator_grant": run.creator_grant,
            "actor": run.actor,
            "audience": run.audience,
            "task": run.task,
            "step_id": step.step_id,
            "input_hash": evidence.input_hash,
            "grant": run.grant
        }),
    )
}
impl WorkflowSession {
    fn context(&self, run: &WorkflowRun, step: &StepEvidence) -> Result<Value, String> {
        let mut ctx = PrepareContext {
            run_id: run.run_id.clone(),
            step_key: step.step_id.clone(),
            input_hash: step.input_hash.clone(),
            session_id: self.session_id.clone(),
            expires_at: run.deadline_at.clone(),
            mac: String::new(),
        };
        ctx.mac = workflow_mcp::sign(&self.secret, "context", &workflow_mcp::unsigned(&ctx)?)?;
        serde_json::to_value(ctx).map_err(|e| e.to_string())
    }
    pub async fn read(
        &mut self,
        run: &WorkflowRun,
        revision: &ApprovedWorkflowRevision,
        step: &StepDefinition,
        evidence: &StepEvidence,
        arguments: Value,
    ) -> Result<(Value, Value), String> {
        let StepAction::McpRead { tool } = &step.action else {
            return Err("not a read step".into());
        };
        let call = json!({"name":tool,"arguments":arguments});
        let context = self.context(run, evidence)?;
        let reply = self
            .client
            .read_workflow_call(call.clone(), context)
            .await
            .map_err(|e| match e {
                McpError::Timeout | McpError::Closed | McpError::Io(_) => {
                    "workflow_read_transport_unavailable".to_string()
                }
                McpError::Rpc { code, .. } => format!("workflow_read_denied:{code}"),
                _ => "workflow_read_protocol_rejected".to_string(),
            })?;
        if reply.version != 1
            || reply.run_id != run.run_id
            || reply.step_key != step.step_id
            || reply.actor != run.actor
            || reply.tool != *tool
            || reply.payload_hash != payload_hash(&call)
            || reply.policy_revision != run.policy_revision
            || reply.environment_hash != self.environment.hash()
            || reply.authority_digest != authority_digest(run, revision, step, evidence)
            || reply.result_hash != payload_hash(&reply.result)
            || reply.error_code.is_some()
        {
            return Err("workflow read evidence mismatch".into());
        }
        step.output_schema.validate(&reply.result)?;
        Ok((reply.result, reply.evidence))
    }
    pub async fn prepare(
        &mut self,
        run: &WorkflowRun,
        step: &StepEvidence,
        call: Value,
    ) -> Result<PrepareTicket, String> {
        let context = self.context(run, step)?;
        let ticket = self
            .client
            .prepare_workflow_call(call, context)
            .await
            .map_err(|e| format!("workflow prepare blocked: {e}"))?;
        workflow_mcp::verify(
            &self.secret,
            "prepare",
            &workflow_mcp::unsigned(&ticket)?,
            &ticket.mac,
        )?;
        Ok(ticket)
    }
    pub async fn execute(
        &mut self,
        broker: &Arc<ApprovalBroker>,
        operation_id: &str,
        binding: &ExecutionBinding,
        prepared: &PrepareTicket,
        call: Value,
    ) -> Result<EffectReply, String> {
        let mut ticket = ExecuteTicket {
            version: 1,
            session_id: self.session_id.clone(),
            operation_id: operation_id.into(),
            binding_digest: binding.digest(),
            prepare_digest: workflow_mcp::digest(prepared)?,
            expires_at: binding.expires_at.clone(),
            mac: String::new(),
        };
        ticket.mac =
            workflow_mcp::sign(&self.secret, "execute", &workflow_mcp::unsigned(&ticket)?)?;
        let reply = self.client.execute_workflow_call(call, ticket).await;
        // A lost response is not permission to submit again. The ledger decides.
        let operation = broker
            .inspect_operation(operation_id)
            .await?
            .ok_or("operation ledger missing")?;
        if operation.binding != *binding {
            return Err("workflow receipt binding changed".into());
        }
        let refusal = match reply {
            Ok(reply) => {
                let field = if reply.operation_id != operation_id {
                    Some("operation")
                } else if reply.binding_digest != binding.digest() {
                    Some("binding")
                } else if serde_json::to_value(reply.authority_source).map_err(|e| e.to_string())?
                    != serde_json::to_value(&operation.authority).map_err(|e| e.to_string())?
                {
                    Some("authority")
                } else if reply.receipt_digest != operation.receipt.as_ref().map(payload_hash) {
                    Some("receipt")
                } else {
                    None
                };
                if let Some(field) = field {
                    return Err(format!("workflow stdio receipt mismatch: {field}"));
                }
                None
            }
            // R-M2: infrastructure answers before begin (another executor's
            // live claim, a busy store, a lost tool process) are not refusals:
            // the operation never began, the runner retries it.
            Err(McpError::Rpc { message, .. }) if is_infrastructure_refusal(&message) => None,
            // A gate answered: a definite refusal, not a lost reply.
            Err(McpError::Rpc { code, .. }) => Some(format!("workflow_effect_refused:{code}")),
            Err(_) => None,
        };
        Ok(EffectReply {
            state: operation.state,
            receipt: operation.receipt,
            error_code: operation.error_code,
            refusal,
        })
    }
}

/// Errors a CLI reports before `begin` that say nothing about the gate.
pub fn is_infrastructure_refusal(message: &str) -> bool {
    const MARKERS: &[&str] = &[
        crate::approval::OPERATION_LEASE_HELD,
        "database is locked",
        "database table is locked",
        "database busy",
        "disk I/O error",
        "workflow operation ledger unavailable",
    ];
    MARKERS.iter().any(|m| message.contains(m))
}

/// Ledger outcome of one execute call. `refusal` is set only when the
/// server answered with an error; transport loss leaves it empty.
pub struct EffectReply {
    pub state: OperationState,
    pub receipt: Option<Value>,
    pub error_code: Option<String>,
    pub refusal: Option<String>,
}
