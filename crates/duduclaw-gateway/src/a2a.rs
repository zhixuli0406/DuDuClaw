//! A2A v1 JSON-RPC vocabulary and fail-closed exposure policy.
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

pub(crate) mod proxy;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Config {
    pub exposed_agents: Vec<String>,
    pub public_url: String,
    pub trusted_proxy_cidrs: Vec<String>,
}

impl Config {
    pub fn from_home(home: &Path) -> Result<Self, Error> {
        let text = std::fs::read_to_string(home.join("config.toml")).map_err(|_| Error::unavailable())?;
        let root: toml::Value = toml::from_str(&text).map_err(|_| Error::unavailable())?;
        let config: Self = match root.get("a2a") {
            Some(value) => value.clone().try_into().map_err(|_| Error::unavailable())?,
            None => Self::default(),
        };
        if config.exposed_agents.len() > 100 || config.exposed_agents.iter().any(|id| !duduclaw_core::is_valid_agent_id(id)) {
            return Err(Error::unavailable());
        }
        if config.trusted_proxy_cidrs.len() > 32
            || config.trusted_proxy_cidrs.iter().any(|cidr| proxy::Cidr::parse(cidr).is_none()) {
            return Err(Error::unavailable());
        }
        if !config.exposed_agents.is_empty() {
            let url = url::Url::parse(&config.public_url).map_err(|_| Error::unavailable())?;
            if url.scheme() != "https" || url.host_str().is_none() || !url.username().is_empty()
                || url.password().is_some() || url.query().is_some() || url.fragment().is_some()
                || url.path() != "/a2a"
            {
                return Err(Error::unavailable());
            }
        }
        Ok(config)
    }

    pub fn exposes(&self, id: &str) -> bool {
        self.exposed_agents.iter().any(|exposed| exposed == id)
    }
}

#[derive(Debug)]
pub(crate) struct Error {
    pub code: i64,
    pub message: &'static str,
    pub reason: &'static str,
}

impl Error {
    pub const fn new(code: i64, message: &'static str, reason: &'static str) -> Self {
        Self { code, message, reason }
    }
    pub fn invalid() -> Self { Self::new(-32602, "Invalid parameters", "INVALID_ARGUMENT") }
    pub fn denied() -> Self { Self::new(-32000, "Permission denied", "PERMISSION_DENIED") }
    pub fn missing() -> Self { Self::new(-32001, "Task not found", "TASK_NOT_FOUND") }
    pub fn unavailable() -> Self { Self::new(-32603, "A2A unavailable", "INTERNAL_ERROR") }
    pub fn unsupported() -> Self { Self::new(-32004, "Unsupported operation", "UNSUPPORTED_OPERATION") }
    pub fn value(&self) -> Value {
        json!({"code": self.code, "message": self.message, "data": [{
            "@type": "type.googleapis.com/google.rpc.ErrorInfo", "reason": self.reason,
            "domain": "a2a-protocol.org"
        }]})
    }
}

/// An unenumerated method requires Admin and still has no dispatch target.
pub(crate) fn minimum_role(method: &str) -> duduclaw_auth::UserRole {
    use duduclaw_auth::UserRole;
    match method {
        "SendMessage" | "GetTask" | "ListTasks" | "CancelTask" | "GetExtendedAgentCard"
        | "SendStreamingMessage" | "SubscribeToTask" | "CreateTaskPushNotificationConfig"
        | "GetTaskPushNotificationConfig" | "ListTaskPushNotificationConfigs"
        | "DeleteTaskPushNotificationConfig" => UserRole::Employee,
        _ => UserRole::Admin,
    }
}

pub(crate) fn canonical_method(method: &str) -> &str {
    match method {
        // These two names were already promised by the stdio A2A server.
        "message/send" => "SendMessage",
        "tasks/get" => "GetTask",
        other => other,
    }
}

pub(crate) fn version(value: Option<&str>) -> Result<(), Error> {
    if value == Some("1.0") { Ok(()) } else {
        Err(Error::new(-32009, "Unsupported A2A version; use 1.0", "VERSION_NOT_SUPPORTED"))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SendRequest {
    message: Message,
    #[serde(default)]
    configuration: SendConfiguration,
    metadata: Target,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Target { agent_id: String }

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SendConfiguration {
    return_immediately: bool,
    accepted_output_modes: Option<Vec<String>>,
    history_length: Option<u32>,
    task_push_notification_config: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Message {
    message_id: String,
    role: String,
    parts: Vec<Value>,
    task_id: Option<String>,
    context_id: Option<String>,
}

pub(crate) struct Send {
    pub message_id: String,
    pub payload_sha256: String,
    pub agent: String,
    pub text: String,
}

pub(crate) fn parse_send(value: Value) -> Result<Send, Error> {
    use sha2::{Digest, Sha256};
    let payload_sha256 = format!("{:x}", Sha256::digest(crate::a2a_signing::canonical_json(&value).as_bytes()));
    let request: SendRequest = serde_json::from_value(value).map_err(|_| Error::invalid())?;
    let message = request.message;
    let config = request.configuration;
    if message.role != "ROLE_USER" || message.message_id.trim().is_empty()
        || message.message_id.len() > 256 || !duduclaw_core::is_valid_agent_id(&request.metadata.agent_id)
        || message.parts.is_empty() || message.parts.len() > 100 {
        return Err(Error::invalid());
    }
    if config.task_push_notification_config.is_some() {
        return Err(Error::new(-32003, "Push notifications not supported", "PUSH_NOTIFICATION_NOT_SUPPORTED"));
    }
    if !config.return_immediately || config.history_length.is_some_and(|n| n != 0)
        || message.task_id.is_some() || message.context_id.is_some() {
        return Err(Error::unsupported());
    }
    if config.accepted_output_modes.is_some_and(|modes| !modes.iter().any(|m| m == "text/plain")) {
        return Err(Error::new(-32005, "Content type not supported", "CONTENT_TYPE_NOT_SUPPORTED"));
    }
    let mut texts = Vec::new();
    for part in &message.parts {
        let object = part.as_object().ok_or_else(Error::invalid)?;
        if object.len() != 1 || !object.contains_key("text") {
            return Err(Error::new(-32005, "Only text parts are supported", "CONTENT_TYPE_NOT_SUPPORTED"));
        }
        texts.push(object["text"].as_str().ok_or_else(Error::invalid)?);
    }
    let text = texts.join("\n");
    if text.trim().is_empty() || text.chars().count() > 4000 {
        return Err(Error::invalid());
    }
    Ok(Send { message_id: message.message_id, payload_sha256, agent: request.metadata.agent_id, text })
}

pub(crate) fn state(status: &str) -> &'static str {
    match status {
        "todo" | "pending" => "TASK_STATE_SUBMITTED",
        "in_progress" | "review" => "TASK_STATE_WORKING",
        "done" => "TASK_STATE_COMPLETED",
        "failed" => "TASK_STATE_FAILED",
        "cancelled" => "TASK_STATE_CANCELED",
        "blocked" | "needs_human" => "TASK_STATE_INPUT_REQUIRED",
        _ => "TASK_STATE_UNSPECIFIED",
    }
}

pub(crate) fn task(task: &crate::task_store::TaskRow, include_artifacts: bool) -> Value {
    let mut value = json!({"id": task.id, "contextId": task.id,
        "status": {"state": state(&task.status), "timestamp": task.updated_at}});
    if include_artifacts && let Some(result) = &task.result_summary {
        value["artifacts"] = json!([{"artifactId": format!("{}:result", task.id),
            "parts": [{"text": result}]}]);
    }
    value
}

pub(crate) fn audit(home: &Path, caller: &str, method: &str, outcome: &str) -> Result<(), Error> {
    use duduclaw_security::audit::{AuditEvent, Severity, try_append_audit_event};
    try_append_audit_event(home, &AuditEvent::new("a2a_request", "", Severity::Info,
        json!({"caller": caller, "method": method, "outcome": outcome})))
        .map_err(|_| Error::unavailable())
}

#[cfg(test)]
mod tests;
