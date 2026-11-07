//! The supported AG-UI input profile. Client history never supplies authority.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

const MAX_MESSAGES: usize = 256;
const MAX_ID_CHARS: usize = 128;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", try_from = "Value")]
pub(super) struct RunInput {
    pub thread_id: String,
    pub run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<String>,
    pub messages: Vec<Value>,
    pub state: Value,
    pub tools: Vec<Value>,
    pub context: Vec<Value>,
    pub forwarded_props: Value,
    pub resume: Vec<ResumeEntry>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", try_from = "Value")]
pub(super) struct ResumeEntry {
    pub interrupt_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

fn object(value: Value) -> Result<Map<String, Value>, &'static str> {
    value.as_object().cloned().ok_or("expected a JSON object")
}

fn required<T: serde::de::DeserializeOwned>(map: &mut Map<String, Value>, key: &str) -> Result<T, &'static str> {
    serde_json::from_value(map.remove(key).ok_or("missing required field")?)
        .map_err(|_| "invalid field type")
}

fn optional<T: serde::de::DeserializeOwned>(map: &mut Map<String, Value>, key: &str) -> Result<Option<T>, &'static str> {
    match map.remove(key) {
        None => Ok(None),
        Some(Value::Null) => Err("optional fields must be omitted, not null"),
        Some(value) => serde_json::from_value(value).map(Some).map_err(|_| "invalid field type"),
    }
}

fn note_unknown(count: usize) {
    static LAST_LOG: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    note_unknown_at(count, &LAST_LOG, std::time::Instant::now());
}

fn note_unknown_at(count: usize, last: &std::sync::Mutex<Option<std::time::Instant>>, now: std::time::Instant) {
    if count == 0 { return; }
    let Ok(mut last) = last.try_lock() else { return; };
    if last.is_some_and(|at| now.saturating_duration_since(at) < std::time::Duration::from_secs(60)) { return; }
    *last = Some(now);
    // Neither names nor values may enter logs, even at debug level.
    tracing::debug!(unknown_count = count, "AG-UI ignored unknown fields");
}

impl TryFrom<Value> for RunInput {
    type Error = &'static str;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let mut map = object(value)?;
        let thread_id = required(&mut map, "threadId")?;
        let run_id = required(&mut map, "runId")?;
        let parent_run_id = optional(&mut map, "parentRunId")?;
        let protocol_version = optional(&mut map, "protocolVersion")?;
        let mut messages: Vec<Value> = required(&mut map, "messages")?;
        let state = match map.remove("state") {
            None | Some(Value::Null) => Value::Object(Map::new()),
            Some(value) => value,
        };
        let tools = optional(&mut map, "tools")?.unwrap_or_default();
        let context = optional(&mut map, "context")?.unwrap_or_default();
        let forwarded_props = optional(&mut map, "forwardedProps")?.unwrap_or_else(|| Value::Object(Map::new()));
        let resume = optional(&mut map, "resume")?.unwrap_or_default();
        note_unknown(map.len());
        for message in &mut messages {
            strip_message_unknown(message)?;
        }
        Ok(Self { thread_id, run_id, parent_run_id, protocol_version, messages,
            state, tools, context, forwarded_props, resume })
    }
}

impl TryFrom<Value> for ResumeEntry {
    type Error = &'static str;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let mut map = object(value)?;
        let entry = Self {
            interrupt_id: required(&mut map, "interruptId")?,
            status: required(&mut map, "status")?,
            payload: optional(&mut map, "payload")?,
            metadata: optional(&mut map, "metadata")?,
        };
        note_unknown(map.len());
        Ok(entry)
    }
}

fn valid_id(value: &str) -> bool {
    !value.trim().is_empty() && value.chars().count() <= MAX_ID_CHARS
        && !value.chars().any(char::is_control)
}

impl ResumeEntry {
    pub fn approved(&self) -> Result<bool, &'static str> {
        if !valid_id(&self.interrupt_id) { return Err("invalid interruptId"); }
        if self.metadata.as_ref().is_some_and(|v| !v.is_object()) {
            return Err("metadata must be an object");
        }
        match (self.status.as_str(), &self.payload) {
            ("cancelled", None) => Ok(false),
            ("resolved", Some(Value::Object(payload))) if payload.len() == 1 => {
                payload.get("approved").and_then(Value::as_bool)
                    .ok_or("resume payload must contain approved: boolean")
            }
            _ => Err("resume requires resolved with approved, or cancelled without payload"),
        }
    }
}

impl RunInput {
    /// Only the latest user message is dispatched. Prior history, system roles,
    /// client tools and state cannot replace the gateway's stored conversation.
    pub fn validate(&self) -> Result<Option<String>, &'static str> {
        if !valid_id(&self.thread_id) || !valid_id(&self.run_id)
            || self.parent_run_id.as_ref().is_some_and(|id| !valid_id(id)) {
            return Err("invalid threadId, runId or parentRunId");
        }
        if self.messages.len() > MAX_MESSAGES { return Err("too many messages"); }
        for message in &self.messages { validate_message(message)?; }
        for tool in &self.tools {
            let map = tool.as_object().ok_or("tool must be an object")?;
            if !map.get("name").and_then(Value::as_str).is_some_and(valid_id)
                || !map.get("description").is_some_and(Value::is_string)
                || !map.get("parameters").is_some_and(|v| v.is_object() || v.is_boolean()) {
                return Err("invalid tool definition");
            }
        }
        for context in &self.context {
            let map = context.as_object().ok_or("context must be an object")?;
            if !map.get("description").is_some_and(Value::is_string)
                || !map.get("value").is_some_and(Value::is_string) {
                return Err("invalid context entry");
            }
        }
        let mut ids = std::collections::HashSet::new();
        for entry in &self.resume {
            entry.approved()?;
            if !ids.insert(&entry.interrupt_id) { return Err("duplicate interruptId"); }
        }
        let latest = self.messages.iter().rev().find(|m| m.get("role").and_then(Value::as_str) == Some("user"));
        match latest {
            Some(message) => {
                let content = text_content(message.get("content").ok_or("user content is required")?)?;
                if content.trim().is_empty() { return Err("user content is empty"); }
                Ok(Some(content))
            }
            None if !self.resume.is_empty() => Ok(None),
            None => Err("a user message is required"),
        }
    }
}

fn strip_message_unknown(message: &mut Value) -> Result<(), &'static str> {
    let map = message.as_object_mut().ok_or("message must be an object")?;
    let mut unknown = 0;
    map.retain(|key, _| {
        let known = matches!(key.as_str(), "id" | "role" | "content" | "name" | "toolCalls" | "toolCallId" | "error" | "activityType");
        if !known { unknown += 1; }
        known
    });
    note_unknown(unknown);
    Ok(())
}

fn validate_message(message: &Value) -> Result<(), &'static str> {
    let map = message.as_object().ok_or("message must be an object")?;
    if !map.get("id").and_then(Value::as_str).is_some_and(valid_id) { return Err("invalid message id"); }
    let role = map.get("role").and_then(Value::as_str).ok_or("invalid message role")?;
    if !matches!(role, "user" | "assistant" | "system" | "developer" | "tool" | "activity" | "reasoning") {
        return Err("unknown message role");
    }
    for key in ["name", "toolCallId", "activityType"] {
        if map.get(key).is_some_and(|v| !v.is_string()) { return Err("invalid message field"); }
    }
    if map.get("error").is_some_and(|v| !v.is_boolean()) { return Err("invalid tool error"); }
    match (role, map.get("content")) {
        ("assistant", None) => {},
        ("activity", Some(content)) if content.is_object() && map.get("activityType").is_some() => {},
        ("user" | "tool", Some(content)) if content.is_string() || content.is_array() => {
            if role == "tool" && !map.get("toolCallId").and_then(Value::as_str).is_some_and(valid_id) {
                return Err("invalid toolCallId");
            }
            if let Some(parts) = content.as_array() {
                for part in parts { validate_content_part(part)?; }
            }
        },
        ("assistant" | "system" | "developer" | "reasoning", Some(Value::String(_))) => {},
        _ => return Err("invalid message content"),
    }
    if let Some(calls) = map.get("toolCalls") {
        let calls = calls.as_array().ok_or("invalid toolCalls")?;
        for call in calls {
            if !call.get("id").and_then(Value::as_str).is_some_and(valid_id)
                || call.get("type").and_then(Value::as_str) != Some("function")
                || !call.pointer("/function/name").and_then(Value::as_str).is_some_and(valid_id)
                || !call.pointer("/function/arguments").is_some_and(Value::is_string) {
                return Err("invalid tool call");
            }
        }
    }
    Ok(())
}

fn validate_content_part(part: &Value) -> Result<(), &'static str> {
    match part.get("type").and_then(Value::as_str) {
        Some("text") if part.get("text").is_some_and(Value::is_string) => Ok(()),
        Some("image" | "audio" | "video" | "document") if part.is_object() => Ok(()),
        _ => Err("invalid content part"),
    }
}

fn text_content(content: &Value) -> Result<String, &'static str> {
    if let Some(text) = content.as_str() { return Ok(text.to_owned()); }
    let parts = content.as_array().ok_or("invalid user content")?;
    let mut result = String::new();
    for part in parts {
        if part.get("type").and_then(Value::as_str) != Some("text") {
            return Err("this endpoint accepts text user content only");
        }
        result.push_str(part.get("text").and_then(Value::as_str).ok_or("invalid text part")?);
    }
    Ok(result)
}

#[cfg(test)]
#[path = "input_tests.rs"]
mod tests;
