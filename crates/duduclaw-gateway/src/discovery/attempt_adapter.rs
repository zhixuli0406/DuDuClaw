//! Discovery-specific runtime contracts, separate from interactive runtimes.
//! Evidence: local `claude --help`, `codex exec --help`, `agy --help`, `grok --help`;
//! real event captures in commercial/research/release-live-validation-2026-10-01-evidence/runtime-parity;
//! https://code.claude.com/docs/en/cli-reference
//! https://developers.openai.com/codex/config-reference
//! https://geminicli.com/docs/reference/configuration/
//! Design: commercial/docs/DESIGN-discovery-runtime-parity-2026-10.md (§2, §5, §6, §7).
use std::{collections::{BTreeMap, BTreeSet}, path::Path};
use serde_json::{Value, json};
use super::contracts::AttemptInfraError;
use super::attempt_guard::{ANTIGRAVITY_TOOLS, GROK_DISALLOWED_TOOLS, GROK_TOOLS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeFamily { Claude, Codex, Gemini, Antigravity, Grok, OpenAiCompat }
impl RuntimeFamily {
    pub fn parse(name: &str) -> Result<Self, AttemptInfraError> {
        match name {
            "claude" => Ok(Self::Claude), "codex" => Ok(Self::Codex),
            "gemini" => Ok(Self::Gemini), "openai-compat" | "openai_compat" => Ok(Self::OpenAiCompat),
            // Tool surface and step ceiling are enforced on the host
            // (attempt_guard); agy's hook is the second line.
            "antigravity" | "agy" => Ok(Self::Antigravity), "grok" => Ok(Self::Grok),
            _ => Err(AttemptInfraError::RuntimeUnsupported(name.into())),
        }
    }
    pub fn name(self) -> &'static str {
        match self { Self::Claude=>"claude", Self::Codex=>"codex", Self::Gemini=>"gemini",
            Self::Antigravity=>"antigravity", Self::Grok=>"grok", Self::OpenAiCompat=>"openai-compat" }
    }
    pub fn provider(self) -> &'static str {
        match self { Self::Claude=>"anthropic", Self::Codex|Self::OpenAiCompat=>"openai",
            Self::Gemini|Self::Antigravity=>"gemini", Self::Grok=>"xai" }
    }
    /// True when no native step ceiling exists and the host counts steps.
    pub fn host_counted_steps(self) -> bool { matches!(self, Self::Codex|Self::Antigravity) }
    pub fn argv(self, model: &str, turns: u32, node: &Path) -> Vec<String> {
        match self {
            Self::Claude => {
                let mut argv=super::agent_spawn::build_claude_argv(model,turns,Path::new("/dudu-runtime/empty-mcp.json"));
                argv.retain(|arg|arg!="--restricted"); argv.push("--safe-mode".into()); argv
            }
            Self::Codex => {
                let mut argv=vec!["exec".into(),"--json".into(),"--strict-config".into(),
                    "--ignore-user-config".into(),"--ignore-rules".into(),"--ephemeral".into(),
                    "--skip-git-repo-check".into(),"--dangerously-bypass-approvals-and-sandbox".into(),
                    "--model".into(),model.into(),"--cd".into(),node.to_string_lossy().into_owned()];
                // Docker is the external hard sandbox named in exec --help.
                // No --add-dir (that flag grants writable directories).
                for setting in ["approval_policy=\"never\"","mcp_servers={}","web_search=\"disabled\"",
                    "features.multi_agent=false","features.hooks=false","features.plugins=false",
                    "features.memories=false","project_doc_max_bytes=0","features.apps=false",
                    "check_for_update_on_startup=false","analytics.enabled=false"] {
                    argv.extend(["-c".into(),setting.into()]);
                }
                argv.push("-".into()); argv
            }
            Self::Gemini => vec!["--prompt".into(),String::new(),"--model".into(),model.into(),
                "--output-format".into(),"stream-json".into(),"--approval-mode".into(),"yolo".into()],
            // Headless grok does not read stdin; no --trust, so workspace hooks/settings stay unloaded.
            Self::Grok => vec!["--prompt-file".into(),"/dudu-runtime/prompt.txt".into(),
                "--output-format".into(),"streaming-messages-json".into(),"--cwd".into(),node.to_string_lossy().into_owned(),
                "-m".into(),model.into(),"--max-turns".into(),turns.to_string(),
                "--permission-mode".into(),"bypassPermissions".into(),"--tools".into(),GROK_TOOLS.join(","),
                "--disallowed-tools".into(),GROK_DISALLOWED_TOOLS.join(","),
                "--disable-web-search".into(),"--no-subagents".into(),"--no-plan".into(),"--no-auto-update".into()],
            // No cwd flag: the container --workdir is the node. Prompt arrives on stdin.
            Self::Antigravity => vec!["--print".into(),String::new(),"--input-format".into(),"stream-json".into(),
                "--output-format".into(),"stream-json".into(),"--dangerously-skip-permissions".into(),
                "--disable-slash-commands".into(),"--model".into(),model.into()],
            Self::OpenAiCompat => vec!["-I".into(),"-S".into(),"-B".into(),"-c".into(),
                include_str!("python/attempt_openai_compat.py").into(), model.into(),turns.to_string()],
        }
    }
}

/// Gemini CLI core tools; the host guard checks against the same list.
/// gemini-cli 0.61 names its grep tool `grep_search`; older CLIs used `search_file_content`.
pub const GEMINI_TOOLS: &[&str] = &["read_file","read_many_files","write_file","replace","list_directory","glob","grep_search","search_file_content","run_shell_command"];

pub fn gemini_settings(turns: u32) -> Value {
    json!({"model":{"maxSessionTurns":turns},"context":{"fileName":[]},
        "tools":{"core":GEMINI_TOOLS,
            "exclude":["web_fetch","google_web_search","save_memory","write_todos"]},
        "mcpServers":{},"admin":{"mcp":{"enabled":false},"extensions":{"enabled":false},"skills":{"enabled":false}},
        "experimental":{"enableAgents":false},"security":{"enablePermanentToolApproval":false}})
}

/// Fixed in-container paths. HOME is a private tmpfs directory.
pub const PRIVATE_HOME: &str = "/tmp/dudu-private/home";
const AGY_GUARD_TEMPLATE: &str = include_str!("python/agy_tool_guard.py");
const AGY_ALLOWLIST_SLOT: &str = "__DUDU_ALLOWED_TOOLS_JSON__";

/// Host-written files for the read-only `/dudu-runtime` directory, beyond
/// `empty-mcp.json` and `gemini.json`. Paths are relative to that directory.
pub fn runtime_files(family: RuntimeFamily, node: &Path, _turns: u32, prompt: &str) -> Vec<(String, Vec<u8>)> {
    match family {
        RuntimeFamily::Grok => vec![("prompt.txt".into(), prompt.as_bytes().to_vec())],
        RuntimeFamily::Antigravity => {
            let hook = "python3 -I -S -B /dudu-runtime/agy_tool_guard.py";
            let hooks = json!({"dudu-tool-surface":{"PreToolUse":[{"matcher":"*","hooks":[{"type":"command","command":hook,"timeout":10}]}]}});
            let settings = json!({"modelProvider":"gemini","trustedWorkspaces":[node.to_string_lossy()]});
            vec![("agy_tool_guard.py".into(), agy_guard_script().into_bytes()),
                ("home-seed/.gemini/antigravity-cli/settings.json".into(), settings.to_string().into_bytes()),
                ("home-seed/.gemini/config/hooks.json".into(), hooks.to_string().into_bytes())]
        }
        _ => vec![],
    }
}

/// The hook script with [`ANTIGRAVITY_TOOLS`] rendered in: one source of truth.
pub fn agy_guard_script() -> String {
    AGY_GUARD_TEMPLATE.replacen(AGY_ALLOWLIST_SLOT, &json!(ANTIGRAVITY_TOOLS).to_string(), 1)
}

/// Bytes written to the CLI's stdin. Grok reads `--prompt-file` instead.
pub fn stdin_payload(family: RuntimeFamily, prompt: &str) -> Vec<u8> {
    match family {
        RuntimeFamily::Grok => Vec::new(),
        RuntimeFamily::Antigravity => {
            let mut line = json!({"event":"user","message":{"content":prompt}}).to_string().into_bytes();
            line.push(b'\n'); line
        }
        _ => prompt.as_bytes().to_vec(),
    }
}

fn credential_keys(family: RuntimeFamily) -> &'static [&'static str] {
    match family {
        RuntimeFamily::Claude=>&["ANTHROPIC_API_KEY","CLAUDE_CODE_OAUTH_TOKEN"],
        RuntimeFamily::Codex|RuntimeFamily::OpenAiCompat=>&["OPENAI_API_KEY"],
        RuntimeFamily::Gemini|RuntimeFamily::Antigravity=>&["GEMINI_API_KEY","GOOGLE_API_KEY"],
        RuntimeFamily::Grok=>&["XAI_API_KEY"],
    }
}

/// Only selected credentials enter the image; HOME/config paths are fresh.
pub fn environment(family: RuntimeFamily, selected: &std::collections::HashMap<String,String>) -> BTreeMap<String,String> {
    let mut out=BTreeMap::from([
        ("PATH".into(),"/usr/local/bin:/usr/bin:/bin".into()),
        ("HOME".into(),PRIVATE_HOME.into()),("TMPDIR".into(),"/tmp/dudu-private/tmp".into()),
        ("LANG".into(),"C.UTF-8".into()),("TZ".into(),"UTC".into()),
        ("DISABLE_AUTOUPDATER".into(),"1".into()),("PYTHONDONTWRITEBYTECODE".into(),"1".into()),
        ("CLAUDE_CONFIG_DIR".into(),"/tmp/dudu-private/home/.claude".into()),
        ("CLAUDE_CODE_TMPDIR".into(),"/tmp/dudu-private/tmp".into()),
        ("CODEX_HOME".into(),"/tmp/dudu-private/home/.codex".into()),
        ("GEMINI_CLI_SYSTEM_SETTINGS_PATH".into(),"/dudu-runtime/gemini.json".into()),
        ("GEMINI_CLI_SYSTEM_DEFAULTS_PATH".into(),"/dudu-runtime/gemini.json".into()),
    ]);
    if family==RuntimeFamily::Grok {
        for (key,value) in [("GROK_HOME","/tmp/dudu-private/home/.grok"),("GROK_DISABLE_AUTOUPDATER","1"),
            ("GROK_SUBAGENTS","0"),("GROK_MEMORY","0")] { out.insert(key.into(),value.into()); }
    }
    for key in credential_keys(family) {
        if let Some(value)=selected.get(*key).filter(|v|!v.is_empty()) { out.insert((*key).into(),value.clone()); }
    }
    // codex exec reads CODEX_API_KEY first; both carry the one selected key.
    if family==RuntimeFamily::Codex {
        if let Some(value)=out.get("OPENAI_API_KEY").cloned() { out.insert("CODEX_API_KEY".into(),value); }
    }
    out
}

/// Environment variables whose value is a secret: every family's credential
/// keys, the derived `CODEX_API_KEY` and the credential document. They reach
/// the container by name only (never as argv text).
pub fn is_secret_env(key: &str) -> bool {
    key == "CODEX_API_KEY" || key == CREDENTIAL_DOC_ENV
        || [RuntimeFamily::Claude,RuntimeFamily::Codex,RuntimeFamily::Gemini,RuntimeFamily::Antigravity,RuntimeFamily::Grok,RuntimeFamily::OpenAiCompat]
            .into_iter().any(|family|credential_keys(family).contains(&key))
}

/// Environment names of the credential document handed to the trusted PID 1.
pub const CREDENTIAL_DOC_ENV: &str = "DUDU_CREDENTIAL_DOC";
pub const CREDENTIAL_DEST_ENV: &str = "DUDU_CREDENTIAL_DEST";
pub const CREDENTIAL_DOC_MAX_BYTES: usize = 64 * 1024;

/// HOME-relative destination of a subscription login document. Host-fixed.
pub fn credential_destination(family: RuntimeFamily) -> Option<&'static str> {
    match family { RuntimeFamily::Codex=>Some(".codex/auth.json"), RuntimeFamily::Grok=>Some(".grok/auth.json"), _=>None }
}

/// The selected OAuth account's seat credential is the CLI's `auth.json`.
/// It must be a JSON object of at most 64 KiB; it is re-serialised to one line.
/// Families without a document path ignore the seat. Invalid ⇒ `NoAccount`.
/// The value is never logged and the error never carries it.
pub fn credential_document(family: RuntimeFamily, seat_token: Option<&str>) -> Result<Option<String>, AttemptInfraError> {
    let (Some(_), Some(seat)) = (credential_destination(family), seat_token.filter(|s|!s.trim().is_empty())) else { return Ok(None) };
    if seat.len() > CREDENTIAL_DOC_MAX_BYTES { return Err(AttemptInfraError::NoAccount); }
    match serde_json::from_str::<Value>(seat) {
        Ok(Value::Object(fields)) if !fields.is_empty() => {
            let line = Value::Object(fields).to_string();
            if line.len() > CREDENTIAL_DOC_MAX_BYTES { return Err(AttemptInfraError::NoAccount); }
            Ok(Some(line))
        }
        _ => Err(AttemptInfraError::NoAccount),
    }
}

/// Model ids accepted from a provider stream: `[A-Za-z0-9._:-]{1,64}`.
pub fn observed_model_id(value: &str) -> Option<&str> {
    let ok = !value.is_empty() && value.len() <= 64 && value != "unknown"
        && value.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.'|b'_'|b':'|b'-'));
    ok.then_some(value)
}

/// Native protocol translated into the runner's common result/usage envelope.
/// No model id is invented when the provider stream omits it.
#[derive(Default)]
pub struct StreamAdapter {
    text: String,
    model: Option<String>,
    steps: BTreeMap<u64, String>,
    settled: BTreeSet<u64>,
    usage_input: u64,
    usage_output: u64,
    usage_cache: u64,
    usage_complete: bool,
    last_text: String,
}
impl StreamAdapter {
    /// The model the provider stream reported (Grok, Antigravity), filtered.
    pub fn observed_model(&self) -> Option<&str> { self.model.as_deref() }
    fn observe_model(&mut self, value: &Value) {
        if let Some(model) = value.as_str().and_then(observed_model_id) { self.model = Some(model.to_owned()); }
    }
    /// The run was stopped at the host step ceiling. agy: every generation's
    /// usage is in the stream, so the sum is complete. Codex reports usage only
    /// per turn, so it stays unknown (null); its text is the last agent message.
    pub fn synthetic_result(&self) -> Value {
        let usage = (self.usage_complete && !self.settled.is_empty()).then(||json!({"input_tokens":self.usage_input,
            "output_tokens":self.usage_output,"cache_read_input_tokens":self.usage_cache}));
        let text = if self.last_text.is_empty() { &self.text } else { &self.last_text };
        json!({"type":"result","subtype":"step_limit","result":text,"usage":usage,"is_error":false})
    }
    pub fn normalize(&mut self, family:RuntimeFamily, event:Value) -> Value {
        match family {
            RuntimeFamily::Claude|RuntimeFamily::OpenAiCompat=>event,
            RuntimeFamily::Grok=>{
                // Same envelope as Claude stream-json; only the model is filtered.
                let mut event=event;
                if event["type"].as_str()==Some("assistant") {
                    if let Some(message)=event.get_mut("message").and_then(Value::as_object_mut) {
                        match message.get("model").and_then(Value::as_str).and_then(observed_model_id).map(str::to_owned) {
                            Some(model)=>{self.model=Some(model.clone());message.insert("model".into(),Value::String(model));}
                            None=>{message.remove("model");}
                        }
                    }
                }
                event
            }
            RuntimeFamily::Antigravity=>self.antigravity(event),
            RuntimeFamily::Codex=>match event["type"].as_str() {
                Some("item.completed") if event["item"]["type"].as_str()==Some("agent_message") => {
                    self.text=event["item"]["text"].as_str().unwrap_or("").into();
                    json!({"type":"assistant","message":{"content":[{"type":"text","text":self.text}]}})
                }
                Some("item.started")|Some("item.completed") if matches!(event["item"]["type"].as_str(),Some("command_execution"|"file_change")) => json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":event["item"]["type"]}]}}),
                Some("turn.completed") => {
                    let usage=&event["usage"];
                    let cached=usage["cached_input_tokens"].as_u64().unwrap_or(0);
                    let usage=usage["input_tokens"].as_u64().zip(usage["output_tokens"].as_u64())
                        .filter(|(input,_)|cached<=*input)
                        .map(|(input,output)|json!({"input_tokens":input-cached,"output_tokens":output,"cache_read_input_tokens":cached}));
                    json!({"type":"result","result":self.text,"usage":usage,"is_error":false})
                }
                Some("turn.failed")|Some("error")=>json!({"type":"result","is_error":true,"error":event["error"],"errors":event["message"]}),
                _=>json!({"type":"system"}),
            },
            RuntimeFamily::Gemini=>match event["type"].as_str() {
                Some("message") if event["role"].as_str()==Some("assistant")=>{
                    self.text.push_str(event["content"].as_str().unwrap_or(""));
                    json!({"type":"assistant","message":{"content":[{"type":"text","text":self.text}]}})
                }
                Some("tool_use")=>json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":event["tool_name"]}]}}),
                Some("result")=>{
                    let stats=&event["stats"];
                    let input=stats["input_tokens"].as_u64().or_else(||stats["inputTokens"].as_u64());
                    let output=stats["output_tokens"].as_u64().or_else(||stats["outputTokens"].as_u64());
                    let cached=stats["cached"].as_u64().or_else(||stats["cached_tokens"].as_u64()).unwrap_or(0);
                    let usage=input.zip(output).filter(|(i,_)|cached<=*i).map(|(i,o)|json!({"input_tokens":i-cached,"output_tokens":o,"cache_read_input_tokens":cached}));
                    json!({"type":"result","result":self.text,"usage":usage,
                        "is_error":event["status"].as_str()==Some("error"),"error":event["error"]})
                }
                Some("error")=>json!({"type":"error","error":event["error"],"message":event["message"]}),
                _=>json!({"type":"system"}),
            },
        }
    }
    /// agy 1.2.14: `input_tokens` excludes cache reads, `thinking_tokens` is
    /// already inside `output_tokens`.
    fn agy_usage(usage: &Value) -> Option<(u64, u64, u64)> {
        Some((usage["input_tokens"].as_u64()?, usage["output_tokens"].as_u64()?, usage["cache_read_tokens"].as_u64().unwrap_or(0)))
    }
    fn antigravity(&mut self, event: Value) -> Value {
        match event["event"].as_str() {
            Some("init") => { self.observe_model(&event["init"]["model"]); json!({"type":"system","subtype":"init"}) }
            Some("step_update") => {
                let step = &event["step_update"];
                let state = step["state"].as_str();
                match step["step_type"].as_str() {
                    Some("tool") if state==Some("ACTIVE") => {
                        let name = step["tool_name"].as_str().or_else(||step["tool_info"]["name"].as_str()).unwrap_or("");
                        json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":name}]}})
                    }
                    Some("agent_response") => {
                        let Some(index) = step["step_index"].as_u64() else { return json!({"type":"system"}) };
                        if self.settled.contains(&index) { return json!({"type":"system"}); }
                        let text = self.steps.entry(index).or_default();
                        text.push_str(step["text_delta"].as_str().unwrap_or(""));
                        let text = text.clone();
                        if state != Some("DONE") { return json!({"type":"system"}); }
                        self.settled.insert(index);
                        if !text.trim().is_empty() { self.last_text = text.clone(); }
                        let Some((input, output, cache)) = Self::agy_usage(&step["usage"]) else {
                            self.usage_complete = false;
                            return json!({"type":"system"});
                        };
                        if self.settled.len() == 1 { self.usage_complete = true; }
                        self.usage_input = self.usage_input.saturating_add(input);
                        self.usage_output = self.usage_output.saturating_add(output);
                        self.usage_cache = self.usage_cache.saturating_add(cache);
                        let mut message = json!({"id":format!("step-{index}"),"content":[{"type":"text","text":text}],
                            "usage":{"input_tokens":input,"output_tokens":output,"cache_read_input_tokens":cache}});
                        if let Some(model) = &self.model { message["model"] = json!(model); }
                        json!({"type":"assistant","message":message})
                    }
                    Some("error_message") => {
                        let mut detail = step.clone();
                        if let Some(fields) = detail.as_object_mut() { fields.remove("conversation_id"); }
                        json!({"type":"error","message":detail})
                    }
                    _ => json!({"type":"system"}),
                }
            }
            Some("result") => {
                let result = &event["result"];
                let usage = Self::agy_usage(&result["usage"]).map(|(input, output, cache)|
                    json!({"input_tokens":input,"output_tokens":output,"cache_read_input_tokens":cache}));
                json!({"type":"result","result":result["response"],"usage":usage,
                    "is_error":result["status"].as_str()!=Some("SUCCESS"),"error":result["error"]})
            }
            _ => json!({"type":"system"}),
        }
    }
}

/// An attempt has credentials when its family's key reached the environment,
/// or (Codex, Grok) a validated login document is delivered.
pub fn has_credentials(family:RuntimeFamily,env:&BTreeMap<String,String>,doc:Option<&str>)->bool {
    credential_keys(family).iter().any(|key|env.get(*key).is_some_and(|v|!v.is_empty()))
        || (credential_destination(family).is_some() && doc.is_some_and(|d|!d.is_empty()))
}

/// No userinfo/redirect credentials; unencrypted transport is local probes only.
pub fn compatible_endpoint(value:&str)->Result<String,AttemptInfraError> {
    let url=reqwest::Url::parse(value).map_err(|_|AttemptInfraError::IsolationUnavailable)?;
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some()
        || !(url.scheme()=="https" || (url.scheme()=="http" && matches!(url.host_str(),Some("localhost"|"127.0.0.1"|"[::1]")))) {
        return Err(AttemptInfraError::IsolationUnavailable);
    }
    Ok(value.trim_end_matches('/').into())
}
#[cfg(test)]
#[path = "tests_attempt_adapter.rs"]
mod tests;
