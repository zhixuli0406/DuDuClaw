//! Bounded vendor translations. Endpoint roots and keys come only from the
//! employee credential file; the gateway request cannot choose the wire dialect.
use serde::Deserialize;
use serde_json::{Value, json};
type Result<T> = std::result::Result<T, &'static str>;
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Protocol { #[default] ChatCompletions, Responses, Anthropic, Gemini }
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value.get(key).and_then(Value::as_str).ok_or("invalid provider field")
}
fn function(call: &Value) -> Result<Value> {
    let f = call.get("function").ok_or("invalid tool replay")?;
    let args: Value = serde_json::from_str(text(f, "arguments")?).map_err(|_| "invalid tool replay")?;
    if !args.is_object() { return Err("invalid tool replay"); }
    Ok(json!({"id":text(call,"id")?,"name":text(f,"name")?,"args":args}))
}
impl Protocol {
    pub fn endpoint(self, base: &str, model: &str) -> Result<String> {
        let base = base.trim_end_matches('/');
        Ok(match self {
            Self::ChatCompletions => format!("{base}/chat/completions"),
            Self::Responses => format!("{base}/responses"),
            Self::Anthropic => format!("{base}/v1/messages"),
            Self::Gemini => {
                if !model.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')) {
                    return Err("invalid Gemini model");
                }
                format!("{base}/models/{model}:generateContent")
            },
        })
    }
    pub fn body(self, model: &str, messages: &[Value], tools: &[Value], max_tokens: u32) -> Result<Value> {
        if self == Self::ChatCompletions {
            let mut body = json!({"model":model,"messages":messages,"max_tokens":max_tokens,"stream":false});
            if !tools.is_empty() { body["tools"] = json!(tools); }
            return Ok(body);
        }
        let mut system = Vec::new();
        let mut turns = Vec::new();
        let mut names = std::collections::BTreeMap::new();
        for message in messages {
            let role = text(message, "role")?;
            if role == "system" { system.push(text(message,"content")?); continue; }
            if let Some(replay) = message.get("_vendor_replay") {
                match self {
                    Self::Responses => turns.extend(replay.as_array().ok_or("invalid replay")?.iter().cloned()),
                    Self::Anthropic => turns.push(json!({"role":"assistant","content":replay})),
                    Self::Gemini => {
                        if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                            for call in calls { let f = function(call)?; names.insert(text(&f,"id")?.to_owned(), text(&f,"name")?.to_owned()); }
                        }
                        turns.push(json!({"role":"model","parts":replay}));
                    },
                    _ => return Err("invalid replay protocol"),
                }
                continue;
            }
            if role == "tool" {
                let id = text(message,"tool_call_id")?;
                let content = text(message,"content")?;
                turns.push(match self {
                    Self::Responses => json!({"type":"function_call_output","call_id":id,"output":content}),
                    Self::Anthropic => json!({"role":"user","content":[{"type":"tool_result","tool_use_id":id,"content":content}]}),
                    Self::Gemini => json!({"role":"user","parts":[{"functionResponse":{"name":names.get(id).ok_or("missing tool replay")?,"response":{"result":content}}}]}),
                    _ => return Err("invalid role"),
                });
            } else {
                if !matches!(role, "assistant" | "user") { return Err("invalid role"); }
                let content = text(message,"content")?;
                turns.push(match self {
                    Self::Responses => json!({"role":role,"content":content}),
                    Self::Anthropic => json!({"role":role,"content":[{"type":"text","text":content}]}),
                    Self::Gemini => json!({"role":if role == "assistant" {"model"} else {"user"},"parts":[{"text":content}]}),
                    _ => return Err("invalid role"),
                });
            }
        }
        let definitions: Vec<_> = tools.iter().map(|tool| {
            let f = tool.get("function").ok_or("invalid tool")?;
            let (name, description) = (text(f,"name")?, text(f,"description")?);
            let schema = f.get("parameters").ok_or("invalid schema")?;
            Ok(match self {
                Self::Responses => json!({"type":"function","name":name,"description":description,"parameters":schema}),
                Self::Anthropic => json!({"name":name,"description":description,"input_schema":schema}),
                Self::Gemini => json!({"name":name,"description":description,"parameters":schema}),
                _ => return Err("invalid protocol"),
            })
        }).collect::<Result<_>>()?;
        Ok(match self {
            Self::Responses => json!({"model":model,"instructions":system.join("\n"),"input":turns,
                "tools":definitions,"max_output_tokens":max_tokens,"stream":false,"store":false,
                "include":["reasoning.encrypted_content"]}),
            Self::Anthropic => json!({"model":model,"system":system.join("\n"),"messages":turns,
                "tools":definitions,"max_tokens":max_tokens,"stream":false}),
            Self::Gemini => {
                let mut body = json!({"systemInstruction":{"parts":[{"text":system.join("\n")}]},"contents":turns,
                    "generationConfig":{"maxOutputTokens":max_tokens}});
                if !definitions.is_empty() { body["tools"] = json!([{"functionDeclarations":definitions}]); }
                body
            },
            _ => return Err("invalid protocol"),
        })
    }
    /// Normalize complete native responses before the common all-calls validator.
    /// Opaque reasoning/signature blocks stay inside replay, never user output.
    pub fn normalize(self, value: Value) -> Result<Value> {
        if self == Self::ChatCompletions {
            let mut value = value;
            if let Some(object) = value.as_object_mut() { object.remove("_vendor_replay"); }
            return Ok(value);
        }
        let mut answer = String::new();
        let mut calls = Vec::new();
        let (input, output, replay) = match self {
            Self::Anthropic => {
                if text(&value,"role")? != "assistant" || text(&value,"type")? != "message" { return Err("invalid provider message"); }
                let blocks = value.get("content").and_then(Value::as_array).ok_or("invalid response")?;
                for block in blocks {
                    match text(block,"type")? {
                        "text" => answer.push_str(text(block,"text")?),
                        "tool_use" => calls.push(call(text(block,"id")?,text(block,"name")?,block.get("input").ok_or("invalid tool")?)?),
                        "thinking" | "redacted_thinking" => {},
                        _ => return Err("unsupported provider block"),
                    }
                }
                let expected = if calls.is_empty() { "end_turn" } else { "tool_use" };
                if text(&value,"stop_reason")? != expected { return Err("incomplete provider response"); }
                let input = usage(&value,"/usage/input_tokens")?.checked_add(value.pointer("/usage/cache_read_input_tokens").and_then(Value::as_u64).unwrap_or(0))
                    .and_then(|v| v.checked_add(value.pointer("/usage/cache_creation_input_tokens").and_then(Value::as_u64).unwrap_or(0))).ok_or("usage overflow")?;
                (input, usage(&value,"/usage/output_tokens")?, json!(blocks))
            },
            Self::Responses => {
                if text(&value,"status")? != "completed" || value.get("error").is_some_and(|e| !e.is_null()) { return Err("incomplete provider response"); }
                let blocks = value.get("output").and_then(Value::as_array).ok_or("invalid response")?;
                for block in blocks {
                    match text(block,"type")? {
                        "message" => {
                            if text(block,"role")? != "assistant" || text(block,"status")? != "completed" { return Err("invalid provider message"); }
                            for part in block.get("content").and_then(Value::as_array).ok_or("invalid response")? {
                                if text(part,"type")? != "output_text" { return Err("unsupported provider content"); }
                                answer.push_str(text(part,"text")?);
                            }
                        },
                        "function_call" => {
                            if text(block,"status")? != "completed" { return Err("incomplete provider call"); }
                            let args: Value = serde_json::from_str(text(block,"arguments")?).map_err(|_| "invalid tool")?;
                            calls.push(call(text(block,"call_id")?,text(block,"name")?,&args)?);
                        },
                        "reasoning" => {},
                        _ => return Err("unsupported provider block"),
                    }
                }
                (usage(&value,"/usage/input_tokens")?, usage(&value,"/usage/output_tokens")?, json!(blocks))
            },
            Self::Gemini => {
                let candidates = value.get("candidates").and_then(Value::as_array).filter(|v| v.len() == 1).ok_or("invalid candidates")?;
                let candidate = &candidates[0];
                if text(candidate,"finishReason")? != "STOP" || candidate.pointer("/content/role").and_then(Value::as_str) != Some("model") { return Err("incomplete provider response"); }
                let parts = candidate.pointer("/content/parts").and_then(Value::as_array).ok_or("invalid response")?;
                for (index, part) in parts.iter().enumerate() {
                    if let Some(f) = part.get("functionCall") {
                        calls.push(call(&format!("gemini-{index}"),text(f,"name")?,f.get("args").ok_or("invalid tool")?)?);
                    } else if part.get("thought").and_then(Value::as_bool) == Some(true) {
                        // Retain signed thought parts only in native replay.
                    } else { answer.push_str(text(part,"text")?); }
                }
                let output = usage(&value,"/usageMetadata/candidatesTokenCount")?.checked_add(value.pointer("/usageMetadata/thoughtsTokenCount").and_then(Value::as_u64).unwrap_or(0)).ok_or("usage overflow")?;
                (usage(&value,"/usageMetadata/promptTokenCount")?, output, json!(parts))
            },
            _ => return Err("invalid protocol"),
        };
        Ok(json!({"usage":{"prompt_tokens":input,"completion_tokens":output},
            "choices":[{"finish_reason":if calls.is_empty() {"stop"} else {"tool_calls"},
                "message":{"role":"assistant","content":answer,"tool_calls":calls}}],"_vendor_replay":replay}))
    }
}
fn usage(value: &Value, path: &str) -> Result<u64> { value.pointer(path).and_then(Value::as_u64).ok_or("provider usage missing") }
fn call(id: &str, name: &str, args: &Value) -> Result<Value> {
    if !args.is_object() { return Err("invalid tool arguments"); }
    Ok(json!({"id":id,"type":"function","function":{"name":name,"arguments":serde_json::to_string(args).map_err(|_| "invalid arguments")?}}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_vendors_translate_tools_and_refuse_partial_or_unmeasured_output() {
        let tools = crate::provider_tools(&json!({"tools":[{"name":"search","description":"find","inputSchema":{"type":"object"}}]})).unwrap();
        let fixtures = [
            (Protocol::Anthropic,json!({"role":"assistant","type":"message","stop_reason":"tool_use","usage":{"input_tokens":3,"output_tokens":4},"content":[{"type":"tool_use","id":"1","name":"search","input":{"q":"x"}}]})),
            (Protocol::Responses,json!({"status":"completed","usage":{"input_tokens":3,"output_tokens":4},"output":[{"type":"function_call","status":"completed","call_id":"1","name":"search","arguments":"{\"q\":\"x\"}"}]})),
            (Protocol::Gemini,json!({"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":4},"candidates":[{"finishReason":"STOP","content":{"role":"model","parts":[{"functionCall":{"name":"search","args":{"q":"x"}},"thoughtSignature":"opaque"}]}}]})),
        ];
        for (protocol, fixture) in fixtures {
            let normalized = protocol.normalize(fixture.clone()).unwrap();
            let (_,_,step) = crate::provider_step(&normalized,&tools).unwrap();
            let crate::Step::Tools { assistant, calls } = step else { panic!() };
            let messages = vec![json!({"role":"system","content":"system"}),json!({"role":"user","content":"hello"}),assistant,
                json!({"role":"tool","tool_call_id":calls[0].id,"content":"result"})];
            let body = protocol.body("model",&messages,&tools,100).unwrap();
            assert!(body.to_string().contains("result"));
            if protocol == Protocol::Gemini { assert!(body.to_string().contains("opaque")); }
            let mut missing = fixture;
            missing.as_object_mut().unwrap().remove(if protocol == Protocol::Gemini {"usageMetadata"} else {"usage"});
            assert!(protocol.normalize(missing).is_err());
        }
        assert!(Protocol::Gemini.endpoint("https://api.example/v1beta","model/../../other?key=x").is_err());
    }
}
