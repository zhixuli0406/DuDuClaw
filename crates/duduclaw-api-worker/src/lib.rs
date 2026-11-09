//! Bounded protocol for the root-published OpenAI-compatible employee worker.
//! Provider credentials belong to the employee home; no gateway key is handed in.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const PROTOCOL: &str = "openai-compat-v1";
pub const MAX_INPUT: usize = 1024 * 1024;
pub const MAX_RESPONSE: usize = 4 * 1024 * 1024;
pub const MAX_ROUNDS: usize = 50;
pub const ARGUMENTS: &[&str] = &["--duduclaw-protocol", PROTOCOL, "--mcp-config", "/config/mcp.json"];

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub protocol: String,
    pub provider: Option<String>,
    pub model: String,
    pub messages: Vec<Message>,
    pub max_tokens: u32,
    pub timeout_secs: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message { pub role: String, pub content: String }
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response { pub content: String, pub input_tokens: u64, pub output_tokens: u64 }

impl Request {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.protocol != PROTOCOL || self.model.trim().is_empty()
            || self.model.len() > 256 || self.model.chars().any(char::is_control)
            || self.provider.as_ref().is_some_and(|name| name.is_empty() || name.len() > 128
                || name.chars().any(char::is_control))
            || self.max_tokens == 0 || self.max_tokens > 1_000_000 || self.timeout_secs == 0
            || self.timeout_secs > 86400 || self.messages.is_empty() || self.messages.len() > 1000
            || self.messages.iter().any(|m| !matches!(m.role.as_str(), "system" | "user" | "assistant")) {
            return Err("invalid request");
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| "invalid request")?;
        if bytes.len() > MAX_INPUT { return Err("request too large"); }
        Ok(bytes)
    }
}

/// Employee-owned provider logins, provisioned separately from gateway accounts.
/// Do not derive Debug: an error must never interpolate credential contents.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credentials { pub providers: Vec<Credential> }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credential { pub name: String, pub api_key: String, pub base_url: String,
    #[serde(default)] pub protocol: vendor::Protocol }
impl Credential {
    pub fn validate(&self) -> Result<(), &'static str> {
        let url = reqwest::Url::parse(&self.base_url).map_err(|_| "invalid endpoint")?;
        if self.name.is_empty() || self.name.len() > 128 || self.name.chars().any(char::is_control)
            || self.api_key.is_empty() || self.api_key.len() > 16384 || self.api_key.chars().any(char::is_control)
            || self.base_url.len() > 4096 || url.scheme() != "https" || url.host_str().is_none()
            || !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
            return Err("invalid employee credential");
        }
        Ok(())
    }
}
impl Credentials {
    pub fn select(self, provider: Option<&str>) -> Result<Credential, &'static str> {
        if self.providers.is_empty() || self.providers.len() > 32 { return Err("invalid employee credentials"); }
        let mut seen = std::collections::BTreeSet::new();
        for credential in &self.providers {
            credential.validate()?;
            if !seen.insert(&credential.name) { return Err("duplicate employee provider"); }
        }
        if provider.is_none() && self.providers.len() != 1 { return Err("employee provider required"); }
        self.providers.into_iter().find(|c| provider.is_none_or(|name| c.name == name))
            .ok_or("employee provider unavailable")
    }
}

/// Exact config, no third-party server, env, bearer or command override.
pub fn verify_config(bytes: &[u8]) -> Result<(), &'static str> {
    let actual: Value = serde_json::from_slice(bytes).map_err(|_| "invalid MCP projection")?;
    if actual != json!({"mcpServers":{"duduclaw":{"command":"/runtime/duduclaw-mcp-bridge","args":[]}}}) {
        return Err("invalid MCP projection");
    }
    Ok(())
}

pub fn provider_tools(result: &Value) -> Result<Vec<Value>, &'static str> {
    if result.get("nextCursor").is_some() { return Err("incomplete tool inventory"); }
    let tools = result.get("tools").and_then(Value::as_array).ok_or("invalid tool inventory")?;
    if tools.len() > 1000 { return Err("tool inventory too large"); }
    let mut names = std::collections::BTreeSet::new();
    tools.iter().map(|tool| {
        let name = tool.get("name").and_then(Value::as_str).ok_or("invalid tool name")?;
        if name.is_empty() || name.len() > 128 || !names.insert(name)
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')) {
            return Err("invalid tool name");
        }
        let schema = tool.get("inputSchema").filter(|s| s.is_object()).ok_or("invalid tool schema")?;
        Ok(json!({"type":"function", "function":{"name":name,
            "description":tool.get("description").and_then(Value::as_str).unwrap_or(""), "parameters":schema}}))
    }).collect()
}

/// Only names advertised by this run's live D capability can be requested.
pub fn tool_call<'a>(call: &'a Value, tools: &[Value]) -> Result<(&'a str, Value, &'a str), &'static str> {
    let id = call.get("id").and_then(Value::as_str).filter(|s| !s.is_empty() && s.len() <= 256)
        .ok_or("invalid tool call")?;
    let function = call.get("function").ok_or("invalid tool call")?;
    let name = function.get("name").and_then(Value::as_str).ok_or("invalid tool call")?;
    if call.get("type").and_then(Value::as_str) != Some("function")
        || !tools.iter().any(|tool| tool.pointer("/function/name").and_then(Value::as_str) == Some(name)) {
        return Err("unadvertised tool");
    }
    let args = function.get("arguments").and_then(Value::as_str).filter(|s| s.len() <= 128 * 1024)
        .ok_or("invalid tool arguments")?;
    let args: Value = serde_json::from_str(args).map_err(|_| "invalid tool arguments")?;
    if !args.is_object() { return Err("invalid tool arguments"); }
    Ok((name, args, id))
}

#[derive(Debug)]
pub struct Invocation { pub name: String, pub arguments: Value, pub id: String }
#[derive(Debug)]
pub enum Step { Answer(String), Tools { assistant: Value, calls: Vec<Invocation> } }

/// Validate the complete batch before any tool side effect. Never promote a
/// length-limited reply or malformed tool call to a successful completion.
pub fn provider_step(response: &Value, tools: &[Value]) -> Result<(u64, u64, Step), &'static str> {
    let input = response.pointer("/usage/prompt_tokens").and_then(Value::as_u64).ok_or("provider usage missing")?;
    let output = response.pointer("/usage/completion_tokens").and_then(Value::as_u64).ok_or("provider usage missing")?;
    let choices = response.get("choices").and_then(Value::as_array).filter(|c| c.len() == 1)
        .ok_or("provider choice invalid")?;
    let choice = &choices[0];
    let message = choice.get("message").ok_or("provider message missing")?;
    if message.get("role").and_then(Value::as_str) != Some("assistant") { return Err("provider role invalid"); }
    let calls = match message.get("tool_calls") {
        None | Some(Value::Null) => None,
        Some(Value::Array(calls)) => Some(calls),
        _ => return Err("provider calls invalid"),
    };
    if calls.is_none_or(|calls| calls.is_empty()) {
        if choice.get("finish_reason").and_then(Value::as_str) != Some("stop") { return Err("provider completion incomplete"); }
        let content = message.get("content").and_then(Value::as_str).filter(|s| !s.trim().is_empty())
            .ok_or("empty provider answer")?;
        return Ok((input, output, Step::Answer(content.into())));
    }
    let calls = calls.ok_or("provider calls invalid")?;
    if calls.len() > 16 || choice.get("finish_reason").and_then(Value::as_str) != Some("tool_calls") {
        return Err("provider calls invalid");
    }
    let mut checked = Vec::new();
    let mut sanitized = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for call in calls {
        let (name, arguments, id) = tool_call(call, tools)?;
        if !seen.insert(id) { return Err("duplicate tool call"); }
        sanitized.push(json!({"id":id,"type":"function","function":{"name":name,
            "arguments":serde_json::to_string(&arguments).map_err(|_| "invalid tool arguments")?}}));
        checked.push(Invocation { name: name.into(), arguments, id: id.into() });
    }
    let mut assistant = json!({"role":"assistant","content":message.get("content").filter(|v| v.is_string()),
        "tool_calls":sanitized});
    if let Some(replay) = response.get("_vendor_replay") { assistant["_vendor_replay"] = replay.clone(); }
    Ok((input, output, Step::Tools { assistant, calls: checked }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> Request { Request { protocol: PROTOCOL.into(), provider: Some("provider".into()), model: "model".into(),
        messages: vec![Message { role: "user".into(), content: "hello".into() }], max_tokens: 100, timeout_secs: 30 } }
    #[test]
    fn request_bounds_and_credential_endpoint_are_checked_before_network() {
        assert!(request().encode().is_ok());
        for endpoint in ["http://example.com/v1", "https://user:pass@example.com/v1", "https://example.com/v1?key=x",
            "https://example.com/v1#fragment", "file:///tmp/input"] {
            let c = Credential { name: "provider".into(), api_key: "test-only-key".into(), protocol: vendor::Protocol::default(), base_url: endpoint.into() };
            assert!(c.validate().is_err());
        }
        let mut r = request(); r.messages[0].content = "x".repeat(MAX_INPUT); assert!(r.encode().is_err());
        let mut r = request(); r.provider = Some("bad\nprovider".into()); assert!(r.encode().is_err());
        let mut r = request(); r.messages[0].role = "tool".into(); assert!(r.encode().is_err());
    }
    #[test]
    fn employee_credentials_never_come_from_the_gateway_request() {
        let value = serde_json::to_value(request()).unwrap();
        assert!(value.get("api_key").is_none());
        assert!(value.get("base_url").is_none());
        let mut injected = value; injected["api_key"] = json!("not-accepted");
        assert!(serde_json::from_value::<Request>(injected).is_err());
        let credentials = || Credentials { providers: vec![Credential { name: "provider".into(),
            api_key: "test-only-key".into(), protocol: vendor::Protocol::default(), base_url: "https://provider.example/v1".into() }] };
        assert!(credentials().select(Some("provider")).is_ok());
        assert!(credentials().select(None).is_ok());
        assert!(credentials().select(Some("other")).is_err());
        let mut c = credentials(); c.providers.push(Credential { name: "provider".into(),
            api_key: "other-test-key".into(), protocol: vendor::Protocol::default(), base_url: "https://other.example/v1".into() });
        assert!(c.select(None).is_err());
    }
    #[test]
    fn provider_steps_refuse_partial_answers_missing_usage_and_malformed_batches() {
        let good = json!({"usage":{"prompt_tokens":10,"completion_tokens":5},
            "choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"answer"}}]});
        assert!(matches!(provider_step(&good, &[]).unwrap(), (10,5,Step::Answer(_))));
        let mut bad = good.clone(); bad["choices"][0]["finish_reason"] = json!("length");
        assert!(provider_step(&bad, &[]).is_err());
        let mut bad = good.clone(); bad.as_object_mut().unwrap().remove("usage");
        assert!(provider_step(&bad, &[]).is_err());
        let mut bad = good.clone(); bad["choices"][0]["message"]["tool_calls"] = json!({});
        assert!(provider_step(&bad, &[]).is_err());
        let tools = provider_tools(&json!({"tools":[{"name":"memory_search","inputSchema":{"type":"object"}}]})).unwrap();
        let call = json!({"id":"1","type":"function","function":{"name":"memory_search","arguments":"{}"}});
        let mut batch = good;
        batch["choices"][0]["finish_reason"] = json!("tool_calls");
        batch["choices"][0]["message"]["tool_calls"] = json!([call]);
        assert!(matches!(provider_step(&batch, &tools).unwrap().2, Step::Tools { .. }));
        batch["choices"][0]["message"]["tool_calls"] = json!([call,call]);
        assert!(provider_step(&batch, &tools).is_err());
        batch["choices"][0]["message"]["tool_calls"][1]["function"]["name"] = json!("unadvertised");
        assert!(provider_step(&batch, &tools).is_err());
    }
    #[test]
    fn mcp_projection_refuses_extra_servers_env_and_command_changes() {
        let good = json!({"mcpServers":{"duduclaw":{"command":"/runtime/duduclaw-mcp-bridge","args":[]}}});
        assert!(verify_config(&serde_json::to_vec(&good).unwrap()).is_ok());
        let mut bad = good.clone(); bad["mcpServers"]["duduclaw"]["env"] = json!({});
        assert!(verify_config(&serde_json::to_vec(&bad).unwrap()).is_err());
        let mut bad = good; bad["mcpServers"]["other"] = json!({"command":"sh"});
        assert!(verify_config(&serde_json::to_vec(&bad).unwrap()).is_err());
    }
    #[test]
    fn only_advertised_tools_with_object_arguments_can_cross_the_bridge() {
        let tools = provider_tools(&json!({"tools":[{"name":"memory_search","inputSchema":{"type":"object"}}]})).unwrap();
        let good = json!({"id":"1","type":"function","function":{"name":"memory_search","arguments":"{}"}});
        assert!(tool_call(&good, &tools).is_ok());
        let mut bad = good.clone(); bad["function"]["name"] = json!("execute_program"); assert!(tool_call(&bad, &tools).is_err());
        let mut bad = good; bad["function"]["arguments"] = json!("[]"); assert!(tool_call(&bad, &tools).is_err());
        assert!(provider_tools(&json!({"tools":[],"nextCursor":"more"})).is_err());
        assert!(provider_tools(&json!({"tools":[{"name":"x","inputSchema":{}},{"name":"x","inputSchema":{}}]})).is_err());
    }
}

#[cfg(target_os = "linux")]
pub mod sealed;
pub mod native;

pub mod vendor;
